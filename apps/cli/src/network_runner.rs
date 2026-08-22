//! Minimal Connected Runner consumer for `portable_capsule_v2`.
//!
//! It reuses the canonical control-plane lease wire and launches the ordinary
//! `PortableSession` command inside bwrap. It is intentionally not another
//! scheduler or execution engine.

// The command remains present on every CLI build so unsupported hosts can fail
// with the policy error, while its evaluator and relay implementation are
// deliberately Unix-only.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_ipc::terminal_surface::{MAX_TERMINAL_INPUT_FRAME_BYTES, TERMINAL_WEBSOCKET_SUBPROTOCOL};
use clap::{Args, Subcommand};
use netd::pixel_authorization::{HmacSurfaceAccessAuthorizer, SurfaceAssertionKeyring};
use netd::surface_authorization::{SurfaceAccessAuthorizer, SurfaceGatewayScope};
use netd::surface_websocket_auth::{
    SurfaceHandshakeAuthorizer, is_normalized_allowed_origin, new_consumed_surface_grants,
};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use tungstenite::client::IntoClientRequest;

use crate::activity_executor::{self, ActivityExecutorInput};
use crate::supervisor::PresentationCaptureReceipt;

#[derive(Debug, Args)]
pub(crate) struct RunnerArgs {
    #[command(subcommand)]
    command: RunnerCommands,
}

#[derive(Debug, Subcommand)]
enum RunnerCommands {
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, env = "ATO_API_URL")]
    api_base: Option<String>,
    #[arg(long, env = "ATO_RUNNER_ID")]
    runner_id: Option<String>,
    #[arg(long, env = "ATO_RUNNER_TOKEN")]
    runner_token: Option<String>,
    #[arg(long, env = "ATO_RUNNER_PUBLIC_BASE_URL")]
    public_base_url: String,
    #[arg(long, env = "ATO_RUNNER_STATE_DIR")]
    state_dir: Option<PathBuf>,
    /// Absolute Chrome/Chromium executable for hosted Browser Activity leases.
    #[arg(long, env = "ATO_RUNNER_CHROME")]
    chrome: Option<PathBuf>,
    #[arg(long, default_value = "127.0.0.1:8420")]
    proxy_listen: SocketAddr,
    #[arg(long)]
    once: bool,
}

struct ResolvedServeArgs {
    api_base: String,
    runner_id: String,
    runner_token: String,
    public_base_url: String,
    state_dir: PathBuf,
    chrome: Option<PathBuf>,
    proxy_listen: SocketAddr,
    once: bool,
}

#[derive(Deserialize)]
struct StoredRunnerCredentials {
    api_base: String,
    runner_id: String,
    runner_token: String,
}

#[derive(Debug, Deserialize)]
struct ClaimResponse {
    lease: Option<ClaimedLease>,
    #[serde(default = "default_poll_seconds")]
    next_poll_seconds: u64,
}

fn default_poll_seconds() -> u64 {
    2
}

#[derive(Debug, Deserialize)]
struct ClaimedLease {
    id: String,
    run_id: String,
    command: PortableLeaseCommand,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PortableLeaseCommand {
    kind: String,
    bundle_id: String,
    transport_digest: String,
    expected_root_computation_ref: String,
    exported_port_id: String,
    session_id: String,
    surface_contract_version: String,
    session_surface: SessionSurface,
    #[serde(default)]
    compose_inputs: Vec<ComposeInput>,
    #[serde(default)]
    output_upload_url: String,
    activity_id: String,
    activity_run_id: String,
}

#[derive(Debug, Deserialize)]
struct ComposeInput {
    bundle_id: String,
    transport_digest: String,
    root_computation_ref: String,
    download_url: String,
}

#[derive(Debug, Default, Deserialize)]
struct SessionSurface {
    kind: String,
    surface_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct HostedSessionReport {
    pub(crate) root_computation_ref: String,
    pub(crate) branch: String,
    pub(crate) exported_ports: Vec<HostedPort>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct HostedPort {
    pub(crate) port_id: String,
    pub(crate) protocol: String,
    pub(crate) role: String,
    pub(crate) local_endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReplayWorkerEvent {
    Prepared {
        anchor_root: String,
        target_root: String,
        cursor: usize,
        total_records: usize,
    },
    Progress {
        cursor: usize,
        total_records: usize,
        current_head: String,
        current_record: ReplayRecordProgress,
    },
    Complete {
        cursor: usize,
        total_records: usize,
        current_head: String,
        surface: HostedSessionReport,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct ReplayRecordProgress {
    id: ato_objects::RecordId,
    protocol_id: String,
    port_id: String,
    direction: ato_objects::Direction,
}

#[derive(Debug, Deserialize)]
struct BindingGrant {
    bindings: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ControlResponse {
    stop_requested: bool,
    capture: Option<CaptureRequest>,
}

#[derive(Debug, Deserialize)]
struct CaptureRequest {
    request_id: String,
    upload_url: String,
}

const UNTRUSTED_ISOLATION_CAPABILITY: &str = "isolation=untrusted-v1";
const PROCESS_EXECUTION_ABI_CAPABILITY: &str = "execution_abi=process";

pub(crate) struct UntrustedProcessEvaluator {
    bwrap: PathBuf,
}

#[derive(Serialize)]
struct StatusReport<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorReport<'a>>,
}

#[derive(Serialize)]
struct ErrorReport<'a> {
    code: &'a str,
    message: &'a str,
}

pub(crate) fn run(args: RunnerArgs) -> Result<()> {
    match args.command {
        RunnerCommands::Serve(args) => serve(args),
    }
}

fn serve(args: ServeArgs) -> Result<()> {
    let args = resolve_serve_args(args)?;
    let evaluator = UntrustedProcessEvaluator::discover()?;
    fs::create_dir_all(&args.state_dir)?;
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let base = args.api_base.trim_end_matches('/');
    heartbeat(&client, base, &args)?;
    loop {
        let claim: ClaimResponse = authorized(
            client.get(format!(
                "{base}/v1/runners/{}/leases/next?wait_ms=20000",
                args.runner_id
            )),
            &args.runner_token,
        )
        .send()?
        .error_for_status()?
        .json()?;
        let Some(lease) = claim.lease else {
            if args.once {
                return Ok(());
            }
            thread_sleep(claim.next_poll_seconds);
            heartbeat(&client, base, &args)?;
            continue;
        };
        if let Err(error) = execute_lease(&client, base, &args, &evaluator, &lease) {
            let replay = lease.command.kind == "portable_capsule_replay_v1";
            if replay {
                // Payload-free operator diagnostics stay on the Runner host;
                // the public control plane receives only the stable safe code.
                eprintln!("isolated Replay lease {} failed: {error:#}", lease.id);
            }
            let message = if replay {
                "Replay Session failed and its isolated state was discarded.".to_owned()
            } else {
                format!("portable Capsule execution failed: {error:#}")
            };
            let _ = report_status(
                &client,
                base,
                &args.runner_token,
                &lease.id,
                StatusReport {
                    status: "failed",
                    execution_id: None,
                    error: Some(ErrorReport {
                        code: if replay {
                            "capsule_replay_failed"
                        } else {
                            "portable_capsule_failed"
                        },
                        message: &message,
                    }),
                },
            );
        }
        if args.once {
            return Ok(());
        }
        heartbeat(&client, base, &args)?;
    }
}

fn resolve_serve_args(args: ServeArgs) -> Result<ResolvedServeArgs> {
    let ato_home = std::env::var_os("ATO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".ato")))
        .context("ATO_HOME is unset and the current home directory is unavailable")?;
    resolve_serve_args_from_home(args, ato_home)
}

fn resolve_serve_args_from_home(args: ServeArgs, ato_home: PathBuf) -> Result<ResolvedServeArgs> {
    let credentials_path = ato_home.join("runner").join("credentials.json");
    let stored =
        if args.api_base.is_none() || args.runner_id.is_none() || args.runner_token.is_none() {
            let bytes = fs::read(&credentials_path).with_context(|| {
                format!(
                    "runner credentials are not fully supplied and {} cannot be read",
                    credentials_path.display()
                )
            })?;
            Some(
                serde_json::from_slice::<StoredRunnerCredentials>(&bytes).with_context(|| {
                    format!(
                        "invalid runner credentials in {}",
                        credentials_path.display()
                    )
                })?,
            )
        } else {
            None
        };
    let api_base = args
        .api_base
        .or_else(|| stored.as_ref().map(|value| value.api_base.clone()))
        .context("runner API base is unavailable")?;
    let runner_id = args
        .runner_id
        .or_else(|| stored.as_ref().map(|value| value.runner_id.clone()))
        .context("runner identity is unavailable")?;
    let runner_token = args
        .runner_token
        .or_else(|| stored.map(|value| value.runner_token))
        .context("runner credential is unavailable")?;
    Ok(ResolvedServeArgs {
        api_base,
        runner_id,
        runner_token,
        public_base_url: args.public_base_url,
        state_dir: args
            .state_dir
            .unwrap_or_else(|| ato_home.join("network-runner")),
        chrome: args.chrome,
        proxy_listen: args.proxy_listen,
        once: args.once,
    })
}

impl UntrustedProcessEvaluator {
    fn discover() -> Result<Self> {
        let bwrap = find_executable("bwrap").context("bwrap is not available on PATH")?;
        let available = Command::new(&bwrap)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if std::env::consts::OS != "linux" || !available {
            bail!(
                "portable Capsule runner has no Evaluator satisfying {UNTRUSTED_ISOLATION_CAPABILITY}"
            );
        }
        Ok(Self { bwrap })
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

fn heartbeat(client: &Client, base: &str, args: &ResolvedServeArgs) -> Result<()> {
    authorized(
        client.post(format!("{base}/v1/runners/{}/heartbeat", args.runner_id)),
        &args.runner_token,
    )
    .json(&serde_json::json!({
        "capabilities": [PROCESS_EXECUTION_ABI_CAPABILITY, UNTRUSTED_ISOLATION_CAPABILITY],
        "evaluator": { "implementation": "linux-bwrap", "policy": "untrusted-v1" },
        "supported_lease_kinds": [
            "portable_capsule_v2",
            "portable_capsule_replay_v1",
            "portable_capsule_compose_v1",
            "activity_browser_executor_v0"
        ],
        "supported_session_surfaces": [
            {
                "kind": "web",
                "profiles": ["ato.web-surface.v1"],
                "transports": ["https"]
            },
            {
                "kind": "terminal",
                "profiles": ["ato.terminal-surface.v1"],
                "transports": ["terminal_websocket"]
            }
        ],
        "public_base_url": args.public_base_url,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "max_slots": 1,
        "active_slots": 0,
        "agent_version": env!("CARGO_PKG_VERSION"),
    }))
    .send()?
    .error_for_status()?;
    Ok(())
}

fn execute_lease(
    client: &Client,
    base: &str,
    args: &ResolvedServeArgs,
    evaluator: &UntrustedProcessEvaluator,
    lease: &ClaimedLease,
) -> Result<()> {
    #[cfg(unix)]
    {
        execute_lease_unix(client, base, args, evaluator, lease)
    }
    #[cfg(not(unix))]
    {
        let _ = (client, base, args, evaluator, lease);
        bail!(
            "portable Capsule runner has no Evaluator satisfying {UNTRUSTED_ISOLATION_CAPABILITY}"
        )
    }
}

#[cfg(unix)]
fn execute_lease_unix(
    client: &Client,
    base: &str,
    args: &ResolvedServeArgs,
    evaluator: &UntrustedProcessEvaluator,
    lease: &ClaimedLease,
) -> Result<()> {
    if lease.command.kind == "activity_browser_executor_v0" {
        let chrome = args
            .chrome
            .as_deref()
            .context("ATO_RUNNER_CHROME is required for hosted Browser Activities")?;
        if !chrome.is_absolute() || !chrome.is_file() {
            bail!("Activity Runner Chrome must be an absolute existing file");
        }
        if lease.command.activity_id.is_empty() || lease.command.activity_run_id.is_empty() {
            bail!("Activity executor lease has incomplete Run identity");
        }
        return activity_executor::execute(ActivityExecutorInput {
            client,
            api_base: base,
            runner_token: &args.runner_token,
            lease_id: &lease.id,
            run_id: &lease.run_id,
            activity_id: &lease.command.activity_id,
            activity_run_id: &lease.command.activity_run_id,
            state_dir: &args.state_dir,
            chrome,
            evaluator,
        });
    }
    if lease.command.kind == "portable_capsule_compose_v1" {
        return execute_compose_lease(client, base, args, lease);
    }
    if lease.command.kind == "portable_capsule_replay_v1" {
        return execute_replay_lease_unix(client, base, args, evaluator, lease);
    }
    if lease.command.kind != "portable_capsule_v2" {
        bail!("unsupported lease kind `{}`", lease.command.kind);
    }
    if !lease.command.bundle_id.starts_with("bnd_") {
        bail!("portable lease carries an invalid bundle identity");
    }
    report_status(
        client,
        base,
        &args.runner_token,
        &lease.id,
        StatusReport {
            status: "preparing",
            execution_id: None,
            error: None,
        },
    )?;
    let repository = args.state_dir.join("sessions").join(&lease.run_id);
    fs::create_dir_all(&repository)?;
    let bundle_path = repository.join("input.capsule");
    let bytes = authorized(
        client.get(format!(
            "{base}/v1/runner-leases/{}/capsule-bundle",
            lease.id
        )),
        &args.runner_token,
    )
    .send()?
    .error_for_status()?
    .bytes()?;
    let actual_digest = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
    if actual_digest != lease.command.transport_digest {
        bail!("downloaded bundle transport digest mismatch");
    }
    fs::write(&bundle_path, &bytes)?;
    let binding_grant: BindingGrant = authorized(
        client.get(format!(
            "{base}/v1/runner-leases/{}/capsule-bindings",
            lease.id
        )),
        &args.runner_token,
    )
    .send()?
    .error_for_status()?
    .json()?;
    let mut child = evaluator.spawn_session(
        &bundle_path,
        &repository,
        &lease.command.expected_root_computation_ref,
        &lease.command.exported_port_id,
        &binding_grant.bindings,
    )?;
    let report = read_session_report(&mut child)?;
    if report.root_computation_ref != lease.command.expected_root_computation_ref {
        terminate_child(&mut child);
        bail!("sandboxed session reported an unexpected root");
    }
    let port = report
        .exported_ports
        .iter()
        .find(|port| port.port_id == lease.command.exported_port_id)
        .context("selected exported Port was not realized")?;
    if (lease.command.session_surface.kind == "web" && port.protocol != "ato.http@1")
        || (lease.command.session_surface.kind == "terminal" && port.protocol != "ato.pty@1")
    {
        terminate_child(&mut child);
        bail!("realized Port does not match the negotiated session surface");
    }
    let local_endpoint = port
        .local_endpoint
        .as_deref()
        .context("realized Port did not report a runtime endpoint")?;
    if local_endpoint != "unix:surface.sock" {
        terminate_child(&mut child);
        bail!("sandboxed session did not select the isolated surface relay");
    }
    let proxy = start_surface_proxy(
        args.proxy_listen,
        repository.join("surface.sock"),
        &lease.command,
    )?;
    report_status(
        client,
        base,
        &args.runner_token,
        &lease.id,
        StatusReport {
            status: "running",
            execution_id: None,
            error: None,
        },
    )?;
    let execution_id = format!("portable:{}:{}", lease.run_id, child.id());
    authorized(
        client.post(format!("{base}/v1/runner-leases/{}/ready", lease.id)),
        &args.runner_token,
    )
    .json(&serde_json::json!({
        "execution_id": execution_id,
        "ready_url": args.public_base_url,
        "local_port": args.proxy_listen.port(),
    }))
    .send()?
    .error_for_status()?;

    let mut handled_capture: Option<String> = None;
    loop {
        if child.try_wait()?.is_some() {
            bail!("sandboxed portable session exited while active");
        }
        let control: ControlResponse = authorized(
            client.get(format!("{base}/v1/runner-leases/{}/control", lease.id)),
            &args.runner_token,
        )
        .send()?
        .error_for_status()?
        .json()?;
        if control.stop_requested {
            terminate_child(&mut child);
            drop(proxy);
            authorized(
                client.post(format!("{base}/v1/runner-leases/{}/stopped", lease.id)),
                &args.runner_token,
            )
            .json(&serde_json::json!({ "execution_id": execution_id }))
            .send()?
            .error_for_status()?;
            return Ok(());
        }
        if let Some(capture) = control.capture
            && handled_capture.as_deref() != Some(&capture.request_id)
        {
            capture_and_upload(client, base, args, &repository, &capture)?;
            handled_capture = Some(capture.request_id);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(unix)]
fn execute_compose_lease(
    client: &Client,
    base: &str,
    args: &ResolvedServeArgs,
    lease: &ClaimedLease,
) -> Result<()> {
    if !(2..=8).contains(&lease.command.compose_inputs.len())
        || !lease
            .command
            .output_upload_url
            .starts_with(&format!("/v1/runner-leases/{}/compose-output", lease.id))
    {
        bail!("portable Compose lease carries an invalid bounded command");
    }
    report_status(
        client,
        base,
        &args.runner_token,
        &lease.id,
        StatusReport {
            status: "preparing",
            execution_id: None,
            error: None,
        },
    )?;
    let repository = args.state_dir.join("compose-jobs").join(&lease.run_id);
    fs::create_dir_all(&repository)?;
    let result = (|| {
        let mut inputs = Vec::with_capacity(lease.command.compose_inputs.len());
        for (index, input) in lease.command.compose_inputs.iter().enumerate() {
            if !input.bundle_id.starts_with("bnd_")
                || !input.root_computation_ref.starts_with("blake3:")
                || !input
                    .download_url
                    .starts_with(&format!("/v1/runner-leases/{}/compose-inputs/", lease.id))
            {
                bail!("portable Compose input is invalid");
            }
            let bytes = authorized(
                client.get(format!("{base}{}", input.download_url)),
                &args.runner_token,
            )
            .send()?
            .error_for_status()?
            .bytes()?;
            let actual_digest = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
            if actual_digest != input.transport_digest {
                bail!("portable Compose input transport digest mismatch");
            }
            let path = repository.join(format!("input-{index:02}.capsule"));
            fs::write(&path, &bytes)?;
            inputs.push(path);
        }
        let output = repository.join("composed.capsule");
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("__bundle").arg("compose").arg("--input");
        command
            .args(&inputs)
            .arg("--output")
            .arg(&output)
            .arg("--json");
        command.env_clear();
        let status = command.status()?;
        if !status.success() {
            bail!("canonical ato-compose worker rejected the inputs");
        }
        authorized(
            client.put(format!("{base}{}", lease.command.output_upload_url)),
            &args.runner_token,
        )
        .header("content-type", "application/vnd.ato.capsule")
        .body(fs::read(&output)?)
        .send()?
        .error_for_status()?;
        let execution_id = format!("compose:{}", lease.run_id);
        report_status(
            client,
            base,
            &args.runner_token,
            &lease.id,
            StatusReport {
                status: "ready",
                execution_id: Some(&execution_id),
                error: None,
            },
        )
    })();
    let _ = fs::remove_dir_all(&repository);
    result
}

#[cfg(unix)]
fn execute_replay_lease_unix(
    client: &Client,
    base: &str,
    args: &ResolvedServeArgs,
    evaluator: &UntrustedProcessEvaluator,
    lease: &ClaimedLease,
) -> Result<()> {
    if !lease.command.bundle_id.starts_with("bnd_") {
        bail!("Replay lease carries an invalid bundle identity");
    }
    report_status(
        client,
        base,
        &args.runner_token,
        &lease.id,
        StatusReport {
            status: "preparing",
            execution_id: None,
            error: None,
        },
    )?;
    let repository = args.state_dir.join("replay-sessions").join(&lease.run_id);
    fs::create_dir_all(&repository)?;
    let bundle_path = repository.join("input.capsule");
    let bytes = authorized(
        client.get(format!(
            "{base}/v1/runner-leases/{}/capsule-bundle",
            lease.id
        )),
        &args.runner_token,
    )
    .send()?
    .error_for_status()?
    .bytes()?;
    let actual_digest = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
    if actual_digest != lease.command.transport_digest {
        bail!("downloaded Replay bundle transport digest mismatch");
    }
    fs::write(&bundle_path, &bytes)?;
    let child = evaluator.spawn_replay_session(
        &bundle_path,
        &repository,
        &lease.command.expected_root_computation_ref,
        &lease.command.exported_port_id,
    )?;
    let mut sandbox = ReplaySandbox {
        child,
        repository: repository.clone(),
    };
    let stdout = sandbox
        .child
        .stdout
        .take()
        .context("Replay sandbox stdout unavailable")?;
    let mut lines = BufReader::new(stdout).lines();
    let execution_id = format!("replay:{}:{}", lease.run_id, sandbox.child.id());
    let mut proxy = loop {
        let line = lines
            .next()
            .transpose()?
            .context("Replay sandbox exited before completion")?;
        let event: ReplayWorkerEvent =
            serde_json::from_str(&line).context("invalid safe Replay progress")?;
        // A user may stop while the sandbox is still preparing or stepping.
        // Observe that request before publishing another progress transition;
        // otherwise the control plane correctly rejects the late progress and
        // the voluntary stop is misclassified as a Replay failure.
        let control: ControlResponse = authorized(
            client.get(format!("{base}/v1/runner-leases/{}/control", lease.id)),
            &args.runner_token,
        )
        .send()?
        .error_for_status()?
        .json()?;
        if control.capture.is_some() {
            bail!("Replay Session cannot capture");
        }
        if control.stop_requested {
            terminate_child(&mut sandbox.child);
            authorized(
                client.post(format!("{base}/v1/runner-leases/{}/stopped", lease.id)),
                &args.runner_token,
            )
            .json(&serde_json::json!({ "execution_id": execution_id }))
            .send()?
            .error_for_status()?;
            return Ok(());
        }
        match &event {
            ReplayWorkerEvent::Prepared { target_root, .. } => {
                if target_root != &lease.command.expected_root_computation_ref {
                    bail!("Replay prepared an unexpected target");
                }
                report_replay_progress(client, base, args, lease, "playing", &event)?;
            }
            ReplayWorkerEvent::Progress { current_head, .. } => {
                if current_head.is_empty() {
                    bail!("Replay reported an empty causal head");
                }
                report_replay_progress(client, base, args, lease, "playing", &event)?;
            }
            ReplayWorkerEvent::Complete {
                current_head,
                surface,
                ..
            } => {
                if current_head != &lease.command.expected_root_computation_ref
                    || surface.root_computation_ref != lease.command.expected_root_computation_ref
                {
                    bail!("Replay completed at an unexpected target");
                }
                let port = surface
                    .exported_ports
                    .iter()
                    .find(|port| port.port_id == lease.command.exported_port_id)
                    .context("selected Replay Port was not realized")?;
                if (lease.command.session_surface.kind == "web" && port.protocol != "ato.http@1")
                    || (lease.command.session_surface.kind == "terminal"
                        && port.protocol != "ato.pty@1")
                    || port.local_endpoint.as_deref() != Some("unix:surface.sock")
                {
                    bail!("Replay surface does not match the negotiated Port");
                }
                let replay_proxy = start_surface_proxy(
                    args.proxy_listen,
                    repository.join("surface.sock"),
                    &lease.command,
                )?;
                // Persist completion before /ready makes the lease terminal. The
                // Replay projection is independent of the ordinary Session
                // Surface readiness transition and must never be stranded at
                // `playing` if the latter succeeds first.
                report_replay_progress(client, base, args, lease, "complete", &event)?;
                report_status(
                    client,
                    base,
                    &args.runner_token,
                    &lease.id,
                    StatusReport {
                        status: "running",
                        execution_id: None,
                        error: None,
                    },
                )?;
                authorized(
                    client.post(format!("{base}/v1/runner-leases/{}/ready", lease.id)),
                    &args.runner_token,
                )
                .json(&serde_json::json!({
                    "execution_id": execution_id,
                    "ready_url": args.public_base_url,
                    "local_port": args.proxy_listen.port(),
                }))
                .send()?
                .error_for_status()?;
                break Some(replay_proxy);
            }
        }
    };

    loop {
        if sandbox.child.try_wait()?.is_some() {
            bail!("Replay sandbox exited while its view-only surface was active");
        }
        let control: ControlResponse = authorized(
            client.get(format!("{base}/v1/runner-leases/{}/control", lease.id)),
            &args.runner_token,
        )
        .send()?
        .error_for_status()?
        .json()?;
        if control.capture.is_some() {
            bail!("Replay Session cannot capture");
        }
        if control.stop_requested {
            drop(proxy.take());
            terminate_child(&mut sandbox.child);
            authorized(
                client.post(format!("{base}/v1/runner-leases/{}/stopped", lease.id)),
                &args.runner_token,
            )
            .json(&serde_json::json!({ "execution_id": execution_id }))
            .send()?
            .error_for_status()?;
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(unix)]
fn report_replay_progress(
    client: &Client,
    base: &str,
    args: &ResolvedServeArgs,
    lease: &ClaimedLease,
    status: &str,
    event: &ReplayWorkerEvent,
) -> Result<()> {
    authorized(
        client.post(format!(
            "{base}/v1/runner-leases/{}/replay-progress",
            lease.id
        )),
        &args.runner_token,
    )
    .json(&serde_json::json!({ "status": status, "event": event }))
    .send()?
    .error_for_status()?;
    Ok(())
}

#[cfg(unix)]
struct ReplaySandbox {
    child: Child,
    repository: PathBuf,
}

#[cfg(unix)]
impl Drop for ReplaySandbox {
    fn drop(&mut self) {
        terminate_child(&mut self.child);
        let _ = fs::remove_dir_all(&self.repository);
    }
}

impl UntrustedProcessEvaluator {
    pub(crate) fn spawn_session(
        &self,
        bundle: &Path,
        repository: &Path,
        root: &str,
        surface_port: &str,
        bindings: &BTreeMap<String, String>,
    ) -> Result<Child> {
        let executable = std::env::current_exe()?;
        let repository = repository.canonicalize()?;
        let bundle = bundle.canonicalize()?;
        if bundle != repository.join("input.capsule") {
            bail!("portable Bundle must be inside the isolated workspace");
        }
        let arguments = sandbox_arguments(&executable, &repository)?;
        let mut command = evaluator_command(&self.bwrap, arguments);
        let mut child = command
            .args(["--setenv", "ATO_EXTERNAL_SANDBOX_PROFILE", "untrusted-v1"])
            .args(["--setenv", "ATO_PTY_GATEWAY_LISTEN", "127.0.0.1:8431"])
            .arg("/opt/ato/bin/ato")
            .args(["__hosted-session", "start"])
            .arg("/workspace/input.capsule")
            .args(["--expected-root", root, "--repository"])
            .arg("/workspace")
            .args(["--surface-port", surface_port])
            .args(["--surface-relay", "/workspace/surface.sock"])
            .arg("--bindings-stdin")
            .arg("--hold")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to enter untrusted-v1 Evaluator")?;
        let mut payload = serde_json::to_vec(bindings)?;
        child
            .stdin
            .take()
            .context("sandbox stdin unavailable")?
            .write_all(&payload)?;
        payload.fill(0);
        Ok(child)
    }

    pub(crate) fn spawn_activity_session(
        &self,
        bundle: &Path,
        repository: &Path,
        root: &str,
        surface_port: &str,
    ) -> Result<Child> {
        fs::create_dir_all(repository.join("browser-runtime"))?;
        let executable = std::env::current_exe()?;
        let repository = repository.canonicalize()?;
        let bundle = bundle.canonicalize()?;
        if bundle != repository.join("input.capsule") {
            bail!("portable Activity Bundle must be inside the isolated workspace");
        }
        let arguments = sandbox_arguments(&executable, &repository)?;
        let mut child = evaluator_command(&self.bwrap, arguments)
            .args(["--setenv", "ATO_EXTERNAL_SANDBOX_PROFILE", "untrusted-v1"])
            .args([
                "--setenv",
                "ATO_BROWSER_RUNTIME_DIR",
                "/workspace/browser-runtime",
            ])
            .args(["--setenv", "ATO_BROWSER_CONTROL_RELAY", "unix"])
            .arg("/opt/ato/bin/ato")
            .args(["__hosted-session", "start"])
            .arg("/workspace/input.capsule")
            .args(["--expected-root", root, "--repository"])
            .arg("/workspace")
            .args(["--surface-port", surface_port])
            .args(["--surface-relay", "/workspace/surface.sock"])
            .arg("--bindings-stdin")
            .arg("--hold")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to enter generic Activity Evaluator")?;
        child
            .stdin
            .take()
            .context("Activity sandbox stdin unavailable")?
            .write_all(b"{}")?;
        Ok(child)
    }

    fn spawn_replay_session(
        &self,
        bundle: &Path,
        repository: &Path,
        root: &str,
        surface_port: &str,
    ) -> Result<Child> {
        let executable = std::env::current_exe()?;
        let repository = repository.canonicalize()?;
        let bundle = bundle.canonicalize()?;
        if bundle != repository.join("input.capsule") {
            bail!("portable Replay Bundle must be inside the isolated workspace");
        }
        let arguments = sandbox_arguments(&executable, &repository)?;
        evaluator_command(&self.bwrap, arguments)
            .args(["--setenv", "ATO_EXTERNAL_SANDBOX_PROFILE", "untrusted-v1"])
            .args(["--setenv", "ATO_PTY_GATEWAY_LISTEN", "127.0.0.1:8431"])
            .arg("/opt/ato/bin/ato")
            .args(["__hosted-session", "replay"])
            .arg("/workspace/input.capsule")
            .args(["--expected-root", root, "--repository"])
            .arg("/workspace")
            .args(["--surface-port", surface_port])
            .args(["--surface-relay", "/workspace/surface.sock"])
            .arg("--hold")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to enter isolated Replay Evaluator")
    }
}

fn evaluator_command(bwrap: &Path, arguments: Vec<OsString>) -> Command {
    let mut command = Command::new(bwrap);
    command.env_clear().args(arguments);
    command
}

fn sandbox_arguments(executable: &Path, repository: &Path) -> Result<Vec<OsString>> {
    let mut arguments = vec![
        "--die-with-parent".into(),
        "--new-session".into(),
        "--unshare-all".into(),
        "--clearenv".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--tmpfs".into(),
        "/".into(),
    ];
    for directory in ["/usr", "/lib", "/lib64", "/bin", "/sbin"] {
        append_read_only_runtime_path(&mut arguments, Path::new(directory))?;
    }
    for file in ["/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/ld.so.conf.d"] {
        append_read_only_runtime_path(&mut arguments, Path::new(file))?;
    }
    arguments.extend([
        "--dir".into(),
        "/opt".into(),
        "--dir".into(),
        "/opt/ato".into(),
        "--dir".into(),
        "/opt/ato/bin".into(),
        "--ro-bind".into(),
        executable.as_os_str().to_owned(),
        "/opt/ato/bin/ato".into(),
        "--dir".into(),
        "/workspace".into(),
        "--bind".into(),
        repository.as_os_str().to_owned(),
        "/workspace".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--tmpfs".into(),
        "/home".into(),
        "--dir".into(),
        "/home/ato".into(),
        "--dir".into(),
        "/run".into(),
        "--hostname".into(),
        "ato-capsule".into(),
        "--chdir".into(),
        "/workspace".into(),
        "--setenv".into(),
        "HOME".into(),
        "/home/ato".into(),
        "--setenv".into(),
        "TMPDIR".into(),
        "/tmp".into(),
        "--setenv".into(),
        "PATH".into(),
        "/usr/bin:/bin".into(),
    ]);
    Ok(arguments)
}

fn append_read_only_runtime_path(arguments: &mut Vec<OsString>, path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        arguments.extend([
            OsStr::new("--symlink").to_owned(),
            fs::read_link(path)?.into_os_string(),
            path.as_os_str().to_owned(),
        ]);
    } else {
        arguments.extend([
            OsStr::new("--ro-bind").to_owned(),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]);
    }
    Ok(())
}

pub(crate) fn read_session_report(child: &mut Child) -> Result<HostedSessionReport> {
    let stdout = child.stdout.take().context("sandbox stdout unavailable")?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    if line.trim().is_empty() {
        let mut stderr = String::new();
        if let Some(mut stream) = child.stderr.take() {
            let _ = stream.read_to_string(&mut stderr);
        }
        bail!("sandboxed session failed before readiness: {stderr}");
    }
    serde_json::from_str(&line).context("invalid hosted session readiness report")
}

fn capture_and_upload(
    client: &Client,
    base: &str,
    args: &ResolvedServeArgs,
    repository: &Path,
    capture: &CaptureRequest,
) -> Result<()> {
    if capture.request_id.is_empty()
        || !capture
            .request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("Runner received an unsafe capture request id");
    }
    let output = repository.join(format!("{}.capsule", capture.request_id));
    let presentation_output = repository.join(format!("{}.presentation", capture.request_id));
    let result = (|| {
        let status = Command::new(std::env::current_exe()?)
            .args(["__hosted-session", "capture", "--repository"])
            .arg(repository)
            .args(["--output"])
            .arg(&output)
            .arg("--presentation-output")
            .arg(&presentation_output)
            .env("ATO_EXTERNAL_SANDBOX_PROFILE", "untrusted-v1")
            .status()?;
        if !status.success() {
            bail!("current-point hosted capture failed");
        }
        let bytes = fs::read(&output)?;
        authorized(
            client.put(format!("{base}{}", capture.upload_url)),
            &args.runner_token,
        )
        .header("content-type", "application/vnd.ato.capsule")
        .body(bytes)
        .send()?
        .error_for_status()?;

        let receipt_bytes = fs::read(presentation_output.join("receipt.json"))?;
        let receipt: PresentationCaptureReceipt = serde_json::from_slice(&receipt_bytes)?;
        if serde_jcs::to_vec(&receipt)? != receipt_bytes {
            bail!("presentation capture receipt is not canonical")
        }
        for asset in receipt.assets {
            if asset.path.components().count() != 1 {
                bail!("presentation capture receipt contains an unsafe asset path")
            }
            let bytes = fs::read(presentation_output.join(&asset.path))?;
            let mut upload = authorized(
                client.put(format!(
                    "{base}{}/presentation-assets/{}/{}",
                    capture.upload_url, asset.kind, asset.sequence
                )),
                &args.runner_token,
            )
            .header("content-type", &asset.content_type);
            if let (Some(width), Some(height)) = (asset.width, asset.height) {
                upload = upload
                    .header("x-ato-viewport-width", width)
                    .header("x-ato-viewport-height", height);
            }
            upload.body(bytes).send()?.error_for_status()?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(output);
    let _ = fs::remove_dir_all(presentation_output);
    result
}

fn report_status(
    client: &Client,
    base: &str,
    token: &str,
    lease_id: &str,
    report: StatusReport<'_>,
) -> Result<()> {
    authorized(
        client.post(format!("{base}/v1/runner-leases/{lease_id}/status")),
        token,
    )
    .json(&report)
    .send()?
    .error_for_status()?;
    Ok(())
}

fn authorized(request: RequestBuilder, token: &str) -> RequestBuilder {
    request.bearer_auth(token)
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn thread_sleep(seconds: u64) {
    std::thread::sleep(Duration::from_secs(seconds.clamp(1, 30)));
}

#[cfg(unix)]
fn start_surface_proxy(
    listen: SocketAddr,
    target: PathBuf,
    command: &PortableLeaseCommand,
) -> Result<Box<dyn Send>> {
    if command.session_surface.kind == "web" {
        return Ok(Box::new(TcpProxy::start_unix(listen, target)?));
    }
    if command.session_surface.kind != "terminal" {
        bail!("unsupported hosted session surface");
    }
    if command.surface_contract_version != "1"
        || command.session_id.trim().is_empty()
        || command.session_surface.surface_id.trim().is_empty()
    {
        bail!("terminal hosted session has an invalid surface scope");
    }
    let keyring_json = std::env::var("ATO_SURFACE_ASSERTION_KEYS_JSON")
        .context("ATO_SURFACE_ASSERTION_KEYS_JSON is required for Terminal Surface")?;
    let keys: BTreeMap<String, String> = serde_json::from_str(&keyring_json)
        .context("ATO_SURFACE_ASSERTION_KEYS_JSON is invalid")?;
    let keyring = Arc::new(
        SurfaceAssertionKeyring::new(keys)
            .context("Terminal Surface assertion keyring is invalid")?,
    );
    let origins_json = std::env::var("ATO_SURFACE_ALLOWED_ORIGINS_JSON")
        .context("ATO_SURFACE_ALLOWED_ORIGINS_JSON is required for Terminal Surface")?;
    let allowed_origins: BTreeSet<String> = serde_json::from_str(&origins_json)
        .context("ATO_SURFACE_ALLOWED_ORIGINS_JSON is invalid")?;
    if allowed_origins.is_empty()
        || allowed_origins
            .iter()
            .any(|origin| !is_normalized_allowed_origin(origin))
    {
        bail!("Terminal Surface requires normalized exact allowed origins");
    }
    let scope = SurfaceGatewayScope {
        session_id: command.session_id.clone(),
        surface_id: command.session_surface.surface_id.clone(),
    };
    let readiness = keyring
        .issue_readiness_assertion(&scope)
        .context("Terminal Surface readiness assertion could not be issued")?;
    let probe_origin = allowed_origins
        .iter()
        .next()
        .cloned()
        .context("Terminal Surface has no readiness Origin")?;
    let authorizer: Arc<dyn SurfaceAccessAuthorizer> =
        Arc::new(HmacSurfaceAccessAuthorizer::new(keyring));
    let proxy = TerminalSurfaceProxy::start(listen, target, scope, allowed_origins, authorizer)?;
    probe_terminal_surface_ready(listen, &probe_origin, readiness.as_str())?;
    Ok(Box::new(proxy))
}

#[cfg(unix)]
fn probe_terminal_surface_ready(listen: SocketAddr, origin: &str, assertion: &str) -> Result<()> {
    let mut request = format!("ws://{listen}/").into_client_request()?;
    request.headers_mut().insert(
        tungstenite::http::header::ORIGIN,
        tungstenite::http::HeaderValue::from_str(origin)
            .context("Terminal Surface readiness Origin is invalid")?,
    );
    request.headers_mut().insert(
        tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
        tungstenite::http::HeaderValue::from_static(TERMINAL_WEBSOCKET_SUBPROTOCOL),
    );
    request.headers_mut().insert(
        netd::surface_websocket_auth::SURFACE_ASSERTION_HEADER,
        tungstenite::http::HeaderValue::from_str(assertion)
            .context("Terminal Surface readiness assertion is invalid")?,
    );
    let stream = TcpStream::connect_timeout(&listen, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let (mut websocket, response) = tungstenite::client(request, stream)
        .map_err(|error| anyhow::anyhow!("Terminal Surface readiness handshake failed: {error}"))?;
    if response
        .headers()
        .get(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        != Some(TERMINAL_WEBSOCKET_SUBPROTOCOL)
    {
        bail!("Terminal Surface readiness did not negotiate ato.terminal.v1");
    }
    let message = websocket
        .read()
        .map_err(|error| anyhow::anyhow!("Terminal Surface readiness frame failed: {error}"))?;
    let tungstenite::Message::Text(text) = message else {
        bail!("Terminal Surface readiness did not emit a control frame");
    };
    let control: ato_ipc::terminal_surface::TerminalServerControl =
        serde_json::from_str(&text).context("Terminal Surface readiness control is invalid")?;
    if !matches!(
        control,
        ato_ipc::terminal_surface::TerminalServerControl::Ready { .. }
    ) || control.validate().is_err()
    {
        bail!("Terminal Surface readiness control is not ready");
    }
    let _ = websocket.close(None);
    Ok(())
}

#[cfg(unix)]
struct TerminalSurfaceProxy {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl TerminalSurfaceProxy {
    fn start(
        listen: SocketAddr,
        target: PathBuf,
        scope: SurfaceGatewayScope,
        allowed_origins: BTreeSet<String>,
        authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let consumed_grants = new_consumed_surface_grants();
        let active_viewer = Arc::new(AtomicBool::new(false));
        let worker = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let target = target.clone();
                        let scope = scope.clone();
                        let origins = allowed_origins.clone();
                        let authorizer = Arc::clone(&authorizer);
                        let consumed = Arc::clone(&consumed_grants);
                        let active = Arc::clone(&active_viewer);
                        std::thread::spawn(move || {
                            let callback = SurfaceHandshakeAuthorizer::new(
                                origins,
                                scope,
                                authorizer,
                                consumed,
                                TERMINAL_WEBSOCKET_SUBPROTOCOL,
                                true,
                            );
                            let config = tungstenite::protocol::WebSocketConfig::default()
                                .max_message_size(Some(MAX_TERMINAL_INPUT_FRAME_BYTES))
                                .max_frame_size(Some(MAX_TERMINAL_INPUT_FRAME_BYTES));
                            let Ok(mut browser) =
                                tungstenite::accept_hdr_with_config(client, callback, Some(config))
                            else {
                                return;
                            };
                            if active
                                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                                .is_err()
                            {
                                let _ = browser.close(None);
                                return;
                            }
                            let _viewer = ActiveTerminalViewer(active);
                            let Ok(stream) = UnixStream::connect(&target) else {
                                let _ = browser.close(None);
                                return;
                            };
                            let mut request = match "ws://localhost/".into_client_request() {
                                Ok(request) => request,
                                Err(_) => return,
                            };
                            request.headers_mut().insert(
                                tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
                                tungstenite::http::HeaderValue::from_static(
                                    TERMINAL_WEBSOCKET_SUBPROTOCOL,
                                ),
                            );
                            let Ok((mut sandbox, response)) = tungstenite::client(request, stream)
                            else {
                                let _ = browser.close(None);
                                return;
                            };
                            if response
                                .headers()
                                .get(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL)
                                .and_then(|value| value.to_str().ok())
                                != Some(TERMINAL_WEBSOCKET_SUBPROTOCOL)
                            {
                                let _ = browser.close(None);
                                let _ = sandbox.close(None);
                                return;
                            }
                            let timeout = Some(Duration::from_millis(25));
                            let _ = browser.get_mut().set_read_timeout(timeout);
                            let _ = browser
                                .get_mut()
                                .set_write_timeout(Some(Duration::from_secs(2)));
                            let _ = sandbox.get_mut().set_read_timeout(timeout);
                            let _ = sandbox
                                .get_mut()
                                .set_write_timeout(Some(Duration::from_secs(2)));
                            relay_terminal_websockets(&mut browser, &mut sandbox);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

#[cfg(unix)]
struct ActiveTerminalViewer(Arc<AtomicBool>);

#[cfg(unix)]
impl Drop for ActiveTerminalViewer {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(unix)]
fn relay_terminal_websockets(
    browser: &mut tungstenite::WebSocket<TcpStream>,
    sandbox: &mut tungstenite::WebSocket<UnixStream>,
) {
    loop {
        match browser.read() {
            Ok(message) => {
                let closing = message.is_close();
                if sandbox.send(message).is_err() || closing {
                    return;
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return;
            }
            Err(_) => return,
        }
        match sandbox.read() {
            Ok(message) => {
                let closing = message.is_close();
                if browser.send(message).is_err() || closing {
                    return;
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return;
            }
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
impl Drop for TerminalSurfaceProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) struct TcpProxy {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TcpProxy {
    #[cfg(test)]
    fn start(listen: SocketAddr, target: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        std::thread::spawn(move || proxy_connection(client, target));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    #[cfg(unix)]
    pub(crate) fn start_unix(listen: SocketAddr, target: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let target = target.clone();
                        std::thread::spawn(move || proxy_unix_connection(client, &target));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for TcpProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
fn proxy_connection(mut client: TcpStream, target: SocketAddr) {
    let Ok(mut upstream) = TcpStream::connect(target) else {
        return;
    };
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let forward = std::thread::spawn(move || {
        let _ = std::io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut upstream, &mut client);
    let _ = client.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

#[cfg(unix)]
fn proxy_unix_connection(mut client: TcpStream, target: &Path) {
    let Ok(mut upstream) = UnixStream::connect(target) else {
        return;
    };
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let forward = std::thread::spawn(move || {
        let _ = std::io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut upstream, &mut client);
    let _ = client.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

use sha2::Digest as _;

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    use super::*;

    #[cfg(unix)]
    #[allow(clippy::result_large_err)]
    fn accept_test_terminal_upstream(stream: UnixStream) -> tungstenite::WebSocket<UnixStream> {
        tungstenite::accept_hdr(
            stream,
            |request: &tungstenite::handshake::server::Request,
             mut response: tungstenite::handshake::server::Response| {
                assert_eq!(
                    request
                        .headers()
                        .get(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL)
                        .unwrap(),
                    TERMINAL_WEBSOCKET_SUBPROTOCOL
                );
                response.headers_mut().insert(
                    tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
                    tungstenite::http::HeaderValue::from_static(TERMINAL_WEBSOCKET_SUBPROTOCOL),
                );
                Ok(response)
            },
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn terminal_surface_proxy_requires_scoped_assertion_and_relays_v1_frames() {
        let test_root = PathBuf::from(".tmp").join(format!("tsp-{}", std::process::id()));
        fs::create_dir_all(&test_root).unwrap();
        let socket_path = test_root.join("surface.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let upstream = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = accept_test_terminal_upstream(stream);
            websocket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(
                        &ato_ipc::terminal_surface::TerminalServerControl::Ready {
                            cols: 80,
                            rows: 24,
                        },
                    )
                    .unwrap()
                    .into(),
                ))
                .unwrap();
            let message = websocket.read().unwrap();
            websocket.send(message).unwrap();
            let _ = websocket.close(None);
        });
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let scope = SurfaceGatewayScope {
            session_id: "run:test".to_owned(),
            surface_id: "run:test".to_owned(),
        };
        let keyring = Arc::new(
            SurfaceAssertionKeyring::new(BTreeMap::from([(
                "test-v1".to_owned(),
                "0123456789abcdef0123456789abcdef".to_owned(),
            )]))
            .unwrap(),
        );
        let assertion = keyring.issue_readiness_assertion(&scope).unwrap();
        let authorizer: Arc<dyn SurfaceAccessAuthorizer> =
            Arc::new(HmacSurfaceAccessAuthorizer::new(Arc::clone(&keyring)));
        let proxy = TerminalSurfaceProxy::start(
            address,
            socket_path,
            scope,
            BTreeSet::from(["https://stg-app.ato.run".to_owned()]),
            authorizer,
        )
        .unwrap();

        let unauthenticated = tungstenite::connect(format!("ws://{address}/")).unwrap_err();
        assert!(unauthenticated.to_string().contains("403"));

        let mut request = format!("ws://{address}/").into_client_request().unwrap();
        request.headers_mut().insert(
            tungstenite::http::header::ORIGIN,
            tungstenite::http::HeaderValue::from_static("https://stg-app.ato.run"),
        );
        request.headers_mut().insert(
            tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
            tungstenite::http::HeaderValue::from_static(TERMINAL_WEBSOCKET_SUBPROTOCOL),
        );
        request.headers_mut().insert(
            netd::surface_websocket_auth::SURFACE_ASSERTION_HEADER,
            tungstenite::http::HeaderValue::from_str(assertion.as_str()).unwrap(),
        );
        let replay_request = request.clone();
        let (mut websocket, response) = tungstenite::connect(request).unwrap();
        assert_eq!(
            response
                .headers()
                .get(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL)
                .unwrap(),
            TERMINAL_WEBSOCKET_SUBPROTOCOL
        );
        let ready: ato_ipc::terminal_surface::TerminalServerControl =
            serde_json::from_slice(&websocket.read().unwrap().into_data()).unwrap();
        assert!(matches!(
            ready,
            ato_ipc::terminal_surface::TerminalServerControl::Ready { .. }
        ));
        websocket
            .send(tungstenite::Message::Binary(b"whoami\n".to_vec().into()))
            .unwrap();
        assert_eq!(websocket.read().unwrap().into_data().as_ref(), b"whoami\n");
        websocket.close(None).unwrap();
        upstream.join().unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let replayed = tungstenite::connect(replay_request).unwrap_err();
        assert!(replayed.to_string().contains("401"));
        drop(proxy);
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn runner_reuses_registered_credentials_and_derives_private_state_directory() {
        let test_root = std::env::current_dir()
            .unwrap()
            .join(".tmp")
            .join(format!("network-runner-credentials-{}", std::process::id()));
        let credentials_dir = test_root.join("runner");
        fs::create_dir_all(&credentials_dir).unwrap();
        fs::write(
            credentials_dir.join("credentials.json"),
            r#"{"api_base":"https://api.example","runner_id":"runr_test","runner_token":"private-token"}"#,
        )
        .unwrap();
        let resolved = resolve_serve_args_from_home(
            ServeArgs {
                api_base: None,
                runner_id: None,
                runner_token: None,
                public_base_url: "https://runner.example".to_owned(),
                state_dir: None,
                chrome: None,
                proxy_listen: "127.0.0.1:8420".parse().unwrap(),
                once: true,
            },
            test_root.clone(),
        )
        .unwrap();
        assert_eq!(resolved.api_base, "https://api.example");
        assert_eq!(resolved.runner_id, "runr_test");
        assert_eq!(resolved.runner_token, "private-token");
        assert_eq!(resolved.state_dir, test_root.join("network-runner"));
        fs::remove_dir_all(test_root).unwrap();
    }

    #[test]
    fn untrusted_evaluator_has_minimal_mounts_namespaces_and_empty_host_environment() {
        let repository = std::env::current_dir().unwrap();
        let executable = repository.join("ato-test-bin");
        let arguments = sandbox_arguments(&executable, &repository).unwrap();
        let rendered: Vec<_> = arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert!(rendered.iter().any(|argument| argument == "--unshare-all"));
        assert!(rendered.iter().any(|argument| argument == "--clearenv"));
        assert!(rendered.windows(2).any(|pair| pair == ["--dev", "/dev"]));
        assert!(
            !rendered
                .windows(3)
                .any(|pair| { pair == ["--ro-bind", "/", "/"] || pair[0] == "--dev-bind" })
        );
        assert!(rendered.windows(3).any(|pair| {
            pair[0] == "--bind"
                && pair[1] == repository.to_string_lossy()
                && pair[2] == "/workspace"
        }));
        let command = evaluator_command(Path::new("/usr/bin/bwrap"), arguments);
        assert_eq!(command.get_envs().count(), 0);
    }

    #[test]
    fn tcp_proxy_preserves_bidirectional_http_bytes() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").unwrap();
        });
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_address = probe.local_addr().unwrap();
        drop(probe);
        let proxy = TcpProxy::start(proxy_address, upstream_address).unwrap();
        let mut client = TcpStream::connect(proxy_address).unwrap();
        client.write_all(b"ping").unwrap();
        let mut response = [0_u8; 4];
        client.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"pong");
        drop(proxy);
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dropping_replay_sandbox_terminates_process_tree_and_removes_workspace() {
        let root = std::env::current_dir()
            .unwrap()
            .join(".tmp")
            .join(format!("replay-drop-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let child = Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap();
        let pid = child.id();
        drop(ReplaySandbox {
            child,
            repository: root.clone(),
        });
        assert!(!root.exists());
        assert!(
            !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .is_ok_and(|status| status.success())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn replay_namespace_cannot_write_host_or_reach_external_canary() {
        let Some(bwrap) = find_executable("bwrap") else {
            return;
        };
        let root = std::env::current_dir()
            .unwrap()
            .join(".tmp")
            .join(format!("replay-isolation-{}", std::process::id()));
        let repository = root.join("workspace");
        let marker = root.join("host-canary");
        fs::create_dir_all(&repository).unwrap();
        let canary = TcpListener::bind("127.0.0.1:0").unwrap();
        canary.set_nonblocking(true).unwrap();
        let mut arguments =
            sandbox_arguments(&std::env::current_exe().unwrap(), &repository).unwrap();
        arguments.extend([
            OsString::from("/bin/bash"),
            OsString::from("-c"),
            OsString::from(format!(
                "echo escaped > {}; exec 3<>/dev/tcp/127.0.0.1/{}",
                marker.display(),
                canary.local_addr().unwrap().port()
            )),
        ]);
        let status = evaluator_command(&bwrap, arguments).status().unwrap();
        assert!(!status.success());
        assert!(!marker.exists());
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(
            canary.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
