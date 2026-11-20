//! A managed Run has an owner and a foreground client. Closing the client's
//! pipe requests a stop, including when the terminal or client disappears.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ato_formation::verify::ContractVerificationReceipt;
use ato_portable_application::local_instance::LOCAL_RUN_STOP_PENDING_FILE;
use ato_portable_application::portable_run::NativeSourceConsent;
use ato_portable_application::{
    PortableRealizationKind, StaticApplicationAsset, StaticApplicationState, bundle_sha256,
    validate_bundle_for_derivation,
};
use ato_runtime_attempt::realize::StopClass;
use serde::{Deserialize, Serialize};

use crate::run_data::{self, Draft, LockedEntry, WorkerResult, read_optional, write_json};
use crate::{
    PortableRuntimeState, RunArgs, clear_stop_pending, copy_scenario_sidecars, hub_public,
    mark_stop_pending, open_browser, portable_export_bundle, read_local_bundle,
    scenario_sidecar_directory, start_portable_attempt,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    pub(super) bundle_path: PathBuf,
    pub(super) digest: String,
    pub(super) derivation: String,
    pub(super) binding_environment: BTreeMap<String, String>,
    pub(super) allow_unsigned_native: bool,
    pub(super) allow_native_source_build: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    url: String,
    runtime: String,
    local_window: bool,
}

pub(super) fn foreground(
    args: &RunArgs,
    hub: Option<&hub_public::ResolvedHubCapsule>,
    entry: &mut LockedEntry,
    draft: &Draft,
    launch: Launch,
    cancelled: &AtomicBool,
) -> Result<()> {
    let active = run_client(args, hub, draft, launch, cancelled);
    // The client sends a stop before waiting for a decision. Never delete
    // data after startup/transport/stop failures even with --on-exit discard.
    if let Err(error) = active {
        entry.pending_message(draft);
        return Err(error);
    }
    entry.confirm_stopped(draft)?;
    let selected = match args.on_exit {
        Some(policy) => Some(policy),
        None if crate::stdin_is_a_terminal() => {
            cancelled.store(false, Ordering::Relaxed);
            match run_data::ask_policy(
                Some("The app stopped. Its working copy is ready."),
                Some(cancelled),
            ) {
                Ok(policy) => policy,
                Err(error) => {
                    entry.pending_message(draft);
                    return Err(error);
                }
            }
        }
        None => None,
    };
    if let Some(policy) = selected {
        if let Err(error) = entry.apply_policy(draft, policy) {
            entry.pending_message(draft);
            return Err(error);
        }
    } else {
        entry.pending_message(draft);
    }
    Ok(())
}

fn run_client(
    args: &RunArgs,
    hub: Option<&hub_public::ResolvedHubCapsule>,
    draft: &Draft,
    launch: Launch,
    cancelled: &AtomicBool,
) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__portable-run-worker")
        .arg(&draft.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ato_local_execution::configure_detached_process(&mut command);
    let mut child = command
        .spawn()
        .context("Could not start the local Run owner")?;
    let mut pipe = child
        .stdin
        .take()
        .context("Run owner control pipe is unavailable")?;
    let session = (|| -> Result<()> {
        serde_json::to_writer(&mut pipe, &launch).context("Send local Run inputs to the owner")?;
        pipe.write_all(b"\n")?;
        let ready_path = draft.path.join("ready.json");
        let ready: Ready = loop {
            if let Some(value) = read_optional(&ready_path)? {
                break value;
            }
            if cancelled.load(Ordering::Relaxed) || child.try_wait()?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        let root = draft.runtime();
        let receipt_bytes = fs::read(root.join("verification-receipt.json"))?;
        let receipt: ContractVerificationReceipt = serde_json::from_slice(&receipt_bytes)?;
        ensure!(
            receipt.fully_satisfied,
            "The app did not satisfy its startup checks"
        );
        if let Some(path) = &args.verification_receipt {
            fs::write(path, receipt_bytes)
                .with_context(|| format!("Write receipt {}", path.display()))?;
            copy_scenario_sidecars(&root, &scenario_sidecar_directory(path))?;
        }
        if args.verbose || !io::IsTerminal::is_terminal(&io::stdout()) {
            println!("Capsule: {}", receipt.contract_ref);
            println!("Route: {}", receipt.derivation_ref);
            println!("Runtime: {}", ready.runtime);
            if let Some(bundle) = &receipt.bundle_sha256 {
                println!("Bundle: {bundle}");
            }
        } else {
            println!("App ready.");
        }
        if ready.local_window {
            println!("Surface: local desktop window");
        }
        if ready.url.is_empty() {
            println!("URL: none (this route has no HTTP address)");
        } else {
            println!("URL: {}", ready.url);
        }
        io::stdout().flush()?;
        if let Some(resolved) = hub
            && receipt.bundle_sha256.as_deref() == Some(resolved.digest.as_str())
        {
            hub_public::report_successful_run(resolved, &receipt);
        }
        if !args.headless
            && !ready.local_window
            && !ready.url.is_empty()
            && let Err(error) = open_browser(&ready.url)
        {
            eprintln!("Open {} in your browser ({error})", ready.url);
        }
        println!("Press Ctrl-C to stop.");
        io::stdout().flush()?;
        while !cancelled.load(Ordering::Relaxed) && child.try_wait()?.is_none() {
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(())
    })();
    drop(pipe);
    let status = child.wait().context("Wait for the Run owner to stop")?;
    let result = draft
        .worker_result()?
        .context("The Run owner exited without recording a stop. Its data was kept")?;
    if let Some(error) = result.error {
        if !result.stopped {
            bail!("Stop could not be confirmed. {error}");
        }
        bail!("{error}\nNo app from this Run is still running.");
    }
    ensure!(
        status.success() && result.stopped,
        "The app's stop could not be confirmed. Its data was kept"
    );
    session
}

pub(super) fn worker(path: PathBuf) -> Result<()> {
    let draft = Draft { path };
    let _lock = draft.lock_worker()?;
    draft.check_pending()?;
    let mut input = io::BufReader::new(io::stdin());
    let mut line = Vec::new();
    // A newline frames the launch message; the still-open pipe is the lease.
    input.read_until(b'\n', &mut line)?;
    let launch: Launch =
        serde_json::from_slice(&line).context("Invalid local Run launch message")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let owner_disconnected = Arc::clone(&cancel);
    std::thread::Builder::new().spawn(move || {
        let mut byte = [0];
        // The launch frame is the only permitted input. EOF or any unexpected
        // data ends the lease.
        let _ = input.read(&mut byte);
        owner_disconnected.store(true, Ordering::Relaxed);
    })?;

    let root = draft.runtime();
    write_json(&draft.path.join("worker-started.json"), &true)?;
    let attempt = start_worker(&draft, &launch, &cancel);
    let stopped = !root.join(LOCAL_RUN_STOP_PENDING_FILE).try_exists()?;
    let error = attempt
        .err()
        .map(|error| redact(format!("{error:#}"), &launch.binding_environment));
    draft.finish_worker(&WorkerResult { stopped, error })?;
    Ok(())
}

fn start_worker(draft: &Draft, launch: &Launch, cancel: &AtomicBool) -> Result<()> {
    let bytes = read_local_bundle(&launch.bundle_path, true)?;
    ensure!(
        bundle_sha256(&bytes) == launch.digest,
        "The cached bundle was changed. Retry with the original app"
    );
    let snapshot = run_data::snapshot_mounts(draft)?;
    let mut observed_snapshot = None;
    let mut static_state = None;
    let mut state_paths = BTreeMap::new();
    let bytes = if let Some((store, id)) = &snapshot {
        let instance = store.instance(id)?;
        let observed = store.observe_run_state(id, &instance.selected_derivation_ref)?;
        observed_snapshot = observed.snapshot_ref;
        state_paths = store.filesystem_state_paths(id)?;
        if let Some(snapshot) = store.local_surface_snapshot(id)? {
            static_state = Some(StaticApplicationState {
                persistence_token: run_data::token()?,
                local_storage: snapshot.local_storage,
                assets: snapshot
                    .assets
                    .into_iter()
                    .map(|asset| StaticApplicationAsset {
                        asset_id: asset.asset_id,
                        filename: asset.filename,
                        content_type: asset.content_type,
                        bytes: asset.bytes,
                    })
                    .collect(),
            });
        }
        store.bundle_bytes(&instance)?
    } else {
        bytes
    };
    let bundle = portable_export_bundle(&bytes)?;
    let route = validate_bundle_for_derivation(&bundle, &launch.derivation)?;
    if snapshot.is_none() {
        state_paths = route
            .derivation
            .state
            .iter()
            .map(|state| (state.id.clone(), draft.path.join("state").join(&state.id)))
            .collect();
    }
    let root = draft.runtime();
    fs::create_dir(&root)?;
    mark_stop_pending(&root)?;
    let started = start_portable_attempt(
        &bytes,
        &bundle,
        &route,
        &root,
        Some(cancel),
        PortableRuntimeState {
            restored_snapshot_ref: observed_snapshot.as_deref(),
            static_state,
            filesystem_state: Some(&state_paths),
            ..PortableRuntimeState::default()
        },
        &launch.binding_environment,
        launch.allow_unsigned_native,
        if launch.allow_native_source_build {
            NativeSourceConsent::Approved
        } else {
            NativeSourceConsent::Unavailable
        },
    )?;

    let mut runtime = started.runtime;
    let result = (|| -> Result<()> {
        ensure!(
            started.receipt.fully_satisfied,
            "The app did not satisfy its startup checks"
        );
        write_json(
            &draft.path.join("ready.json"),
            &Ready {
                url: runtime.base_url().to_owned(),
                runtime: runtime.label().to_owned(),
                local_window: runtime.local_window,
            },
        )?;
        while !cancel.load(Ordering::Relaxed) {
            if runtime.local_window && runtime.live.exited()?.is_some() {
                break;
            }
            if matches!(route.realization, PortableRealizationKind::LocalProcess)
                || runtime.stops_when_a_service_exits
            {
                runtime.try_wait()?;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    })();
    let browser_state = runtime.local_storage();
    let stopped = runtime.stop_recorded(Some(&root));
    if StopClass::of(&stopped).is_confirmed() {
        clear_stop_pending(&root)?;
    }
    stopped?;
    if let Some(storage) = browser_state? {
        write_json(&draft.path.join("browser.json"), &storage)
            .context("Could not capture the browser state")?;
    }
    result
}

fn redact(mut message: String, values: &BTreeMap<String, String>) -> String {
    let mut values = values
        .values()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for value in values {
        message = message.replace(value, "[redacted]");
    }
    message
}
