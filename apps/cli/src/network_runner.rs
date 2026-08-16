//! Minimal Connected Runner consumer for `portable_capsule_v2`.
//!
//! It reuses the canonical control-plane lease wire and launches the ordinary
//! `PortableSession` command inside bwrap. It is intentionally not another
//! scheduler or execution engine.

// The command remains present on every CLI build so unsupported hosts can fail
// with the policy error, while its evaluator and relay implementation are
// deliberately Unix-only.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;
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
use clap::{Args, Subcommand};
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Deserialize)]
struct PortableLeaseCommand {
    kind: String,
    bundle_id: String,
    transport_digest: String,
    expected_root_computation_ref: String,
    exported_port_id: String,
    session_surface: SessionSurface,
}

#[derive(Debug, Deserialize)]
struct SessionSurface {
    kind: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct HostedSessionReport {
    root_computation_ref: String,
    exported_ports: Vec<HostedPort>,
}

#[derive(Debug, Deserialize, Serialize)]
struct HostedPort {
    port_id: String,
    protocol: String,
    local_endpoint: Option<String>,
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

struct UntrustedProcessEvaluator {
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
        "supported_lease_kinds": ["portable_capsule_v2", "portable_capsule_replay_v1"],
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
    let proxy = TcpProxy::start_unix(args.proxy_listen, repository.join("surface.sock"))?;
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
                let replay_proxy =
                    TcpProxy::start_unix(args.proxy_listen, repository.join("surface.sock"))?;
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
    fn spawn_session(
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

fn read_session_report(child: &mut Child) -> Result<HostedSessionReport> {
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
    let output = repository.join(format!("{}.capsule", capture.request_id));
    let status = Command::new(std::env::current_exe()?)
        .args(["__hosted-session", "capture", "--repository"])
        .arg(repository)
        .args(["--output"])
        .arg(&output)
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
    let _ = fs::remove_file(output);
    Ok(())
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

struct TcpProxy {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TcpProxy {
    #[cfg(test)]
    fn start(listen: SocketAddr, target: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
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
            stop,
            worker: Some(worker),
        })
    }

    #[cfg(unix)]
    fn start_unix(listen: SocketAddr, target: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
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
            stop,
            worker: Some(worker),
        })
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

    use super::*;

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
