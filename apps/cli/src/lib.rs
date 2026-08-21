//! Product assembly for the Capsule lifecycle.

#![deny(unsafe_op_in_unsafe_fn)]

mod activity_executor;
mod activity_executor_gateway;
mod authoring;
mod browser_host;
mod network_runner;
mod supervisor;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::{Shutdown, SocketAddr, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use ato_adapter_api::{AdapterContext, AdapterRegistry};
use ato_adapter_binding::BindingAdapter;
use ato_adapter_browser::BrowserAdapter;
use ato_adapter_http::HttpAdapter;
use ato_adapter_process::ProcessLifecycleAdapter;
use ato_adapter_pty::PtyAdapter;
use ato_adapter_workspace::{WorkspaceAdapter, restore_workspace, workspace_snapshot_file_count};
use ato_compose::{
    COMPOSE_SEMANTICS_ID, ComposeReferences, CompositeResidual, Endpoint,
    MAX_COMPOSITE_RESIDUAL_BYTES, decode_composite_residual, encode_composite_residual,
};
use ato_computation::{
    ComputationObject, ComputationRef, ContentRef, NodeId, PortId, SemanticsId, computation_ref,
    encode_computation_object,
};
use ato_materializer_api::{
    Compatibility, MaterializerContext, MaterializerRegistry, RealizationVerification,
    RestoreCapability,
};
use ato_materializer_replay::{
    ReplayMaterializer, ReplayReferences, ReplaySequence, ReplayStepper, records_for_descriptor,
};
use ato_materializer_snapshot::{SnapshotMaterializer, SnapshotReferences};
use ato_objects::{
    BranchOrigin, BundleMaterialization, CapsuleSelector, LocalCapsuleRepository,
    MemoryObjectStore, ObjectStore, RecordId, ReferenceRegistry, decode_bundle, encode_bundle,
    export_bundle, export_bundle_with_materializations, import_bundle, read_exact_object,
    resolve_computation, verify_bundle,
};
use ato_runtime::{
    PortableRuntimeFactory, PortableSession, PortableSessionContext, PortableSessionError,
    PortableSessionRuntime,
};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::authoring::{
    AuthoringReferences, evolve_workspace, initial_computation, load_config, load_runtime_state,
    workspace_policy,
};
use crate::supervisor::{
    CliRealizationDriver, PortableContinuationCapture, capture_active, start_durable,
    start_durable_with_descriptor, stop_active,
};

#[derive(Parser)]
#[command(
    name = "ato",
    version,
    about = "Author, seal, transport, and resume Capsules"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create C0 and start recording an authored Capsule.
    Init(InitArgs),
    /// Continue a branch or create a new future from a historical Record.
    Resume(ResumeArgs),
    /// Quiesce the active Run and atomically seal its branch head.
    Stop { capsule: String },
    /// Materialize one selected point into a portable .capsule bundle.
    Encap(EncapArgs),
    /// Consume a portable .capsule ephemerally.
    Run(RunArgs),
    /// Share a branch point as an unlisted Capsule URL.
    Share(ShareArgs),
    /// Internal machine interface for canonical portable bundle validation.
    #[command(name = "__bundle", hide = true)]
    Bundle(BundleArgs),
    /// Internal hosted-runner interface for a portable Capsule session.
    #[command(name = "__hosted-session", hide = true)]
    HostedSession(HostedSessionArgs),
    /// Internal Connected Runner worker for portable Capsule leases.
    #[command(name = "__runner", hide = true)]
    Runner(network_runner::RunnerArgs),
    #[command(name = "__worker", hide = true)]
    Worker {
        project: PathBuf,
        branch: String,
        head: String,
        token: String,
        descriptor: Option<String>,
    },
    /// Internal host-side Chrome/CDP bridge for Browser Adapter delivery.
    #[command(name = "__browser-host", hide = true)]
    BrowserHost(BrowserHostArgs),
    /// Internal idle workload used by the Activity Browser Adapter Run.
    #[command(name = "__activity-idle", hide = true)]
    ActivityIdle,
}

#[derive(Debug, Args)]
struct HostedSessionArgs {
    #[command(subcommand)]
    command: HostedSessionCommands,
}

#[derive(Debug, Subcommand)]
enum HostedSessionCommands {
    Start(HostedSessionStartArgs),
    Replay(HostedReplayStartArgs),
    Capture(HostedSessionCaptureArgs),
    Stop(HostedSessionStopArgs),
}

#[derive(Debug, Args)]
struct HostedReplayStartArgs {
    bundle: PathBuf,
    #[arg(long)]
    expected_root: String,
    #[arg(long)]
    repository: PathBuf,
    #[arg(long)]
    surface_port: String,
    #[arg(long)]
    surface_relay: PathBuf,
    /// Presentation timing only. It never changes descriptor order.
    #[arg(long, default_value_t = 80)]
    minimum_delay_ms: u64,
    /// Presentation timing only. Long idle gaps are compressed to this bound.
    #[arg(long, default_value_t = 750)]
    maximum_delay_ms: u64,
    #[arg(long)]
    hold: bool,
}

#[derive(Debug, Args)]
struct HostedSessionStartArgs {
    bundle: PathBuf,
    #[arg(long)]
    expected_root: String,
    #[arg(long)]
    repository: PathBuf,
    #[arg(long)]
    surface_port: String,
    #[arg(long)]
    surface_relay: PathBuf,
    #[arg(long = "bind", value_parser = parse_binding)]
    bindings: Vec<(String, String)>,
    /// Read a JSON object of Binding values from stdin. Hosted runner only.
    #[arg(long, conflicts_with = "bindings")]
    bindings_stdin: bool,
    /// Keep the sandbox command alive for the duration of the durable Run.
    #[arg(long)]
    hold: bool,
}

#[derive(Debug, Args)]
struct HostedSessionCaptureArgs {
    #[arg(long)]
    repository: PathBuf,
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long = "materialize")]
    materializers: Vec<String>,
    /// Runtime-private destination for presentation bytes and receipt metadata.
    #[arg(long)]
    presentation_output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct HostedSessionStopArgs {
    #[arg(long)]
    repository: PathBuf,
}

#[derive(Debug, Args)]
struct InitArgs {
    capsule: String,
    #[arg(long)]
    initial_only: bool,
    #[arg(long = "bind", value_parser = parse_binding)]
    bindings: Vec<(String, String)>,
}

#[derive(Debug, Args)]
struct ResumeArgs {
    selector: String,
    #[arg(long)]
    branch: Option<String>,
    #[arg(long = "bind", value_parser = parse_binding)]
    bindings: Vec<(String, String)>,
}

#[derive(Debug, Args)]
struct EncapArgs {
    selector: String,
    /// Export the current active Run frontier without sealing or stopping it.
    #[arg(long)]
    current: bool,
    #[arg(long = "materialize")]
    materializers: Vec<String>,
    #[arg(short, long, default_value = "computation.capsule")]
    output: PathBuf,
    /// Runtime-private presentation receipt. It never changes Bundle identity.
    #[arg(long, hide = true)]
    presentation_output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct RunArgs {
    capsule: PathBuf,
    #[arg(long = "bind", value_parser = parse_binding)]
    bindings: Vec<(String, String)>,
}

#[derive(Debug, Args)]
struct BrowserHostArgs {
    /// Absolute host-private directory containing Browser Adapter discovery.
    #[arg(long)]
    runtime_dir: PathBuf,
    /// Browser page to open. Its origin must exactly match Adapter discovery.
    #[arg(long)]
    target_url: String,
    /// Absolute path to the Chrome/Chromium executable owned by the Runner.
    #[arg(long)]
    chrome: PathBuf,
    /// Run Chrome without a display. Intended for Runner smoke tests only.
    #[arg(long)]
    headless: bool,
}

#[derive(Debug, Args)]
struct ShareArgs {
    selector: String,
    #[arg(long = "materialize")]
    materializers: Vec<String>,
    #[arg(long, default_value = "Shared Capsule")]
    title: String,
    #[arg(long, default_value = "")]
    description: String,
    /// Skip the share-safety confirmation.
    #[arg(long)]
    yes: bool,
    #[arg(long, env = "ATO_API_URL", default_value = "https://api.ato.run")]
    api_base: String,
    #[arg(long, env = "ATO_SHARE_URL", default_value = "https://ato.run")]
    share_base: String,
    /// Existing Ato device credential (`ato_dev_…`).
    #[arg(long, env = "ATO_DEVICE_TOKEN", hide_env_values = true)]
    device_token: String,
}

#[derive(Debug, Args)]
struct BundleArgs {
    #[command(subcommand)]
    command: BundleCommands,
}

#[derive(Debug, Subcommand)]
enum BundleCommands {
    Verify(BundleVerifyArgs),
    ValidateQueue(BundleValidateQueueArgs),
    Compose(BundleComposeArgs),
}

#[derive(Debug, Args)]
struct BundleComposeArgs {
    #[arg(long = "input", required = true, num_args = 2..=8)]
    inputs: Vec<PathBuf>,
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct BundleValidateQueueArgs {
    #[arg(long, env = "ATO_API_URL")]
    api_base: String,
    #[arg(long, env = "ATO_CAPSULE_VALIDATOR_TOKEN")]
    token: String,
    #[arg(long)]
    agent_id: String,
    /// Process at most one job. Intended for job schedulers and tests.
    #[arg(long)]
    once: bool,
}

#[derive(Debug, Args)]
struct BundleVerifyArgs {
    capsule: PathBuf,
    #[arg(long)]
    json: bool,
    /// Validator-only fork proof: require this Computation to be in the
    /// verifier-approved portable closure. The value is never echoed.
    #[arg(long, hide = true)]
    require_computation: Option<String>,
}

pub fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Init(args) => init(args),
        Commands::Resume(args) => resume(args),
        Commands::Stop { capsule } => stop(&capsule),
        Commands::Encap(args) => encap(args),
        Commands::Run(args) => run_capsule(args),
        Commands::Share(args) => share(args),
        Commands::Bundle(args) => match args.command {
            BundleCommands::Verify(args) => verify_bundle_command(args),
            BundleCommands::ValidateQueue(args) => validate_bundle_queue(args),
            BundleCommands::Compose(args) => compose_bundle_command(args),
        },
        Commands::HostedSession(args) => hosted_session(args),
        Commands::Runner(args) => network_runner::run(args),
        Commands::Worker {
            project,
            branch,
            head,
            token,
            descriptor,
        } => supervisor::worker(
            &project,
            &branch,
            &ComputationRef::parse(head)?,
            &token,
            descriptor.map(ContentRef::parse).transpose()?.as_ref(),
        ),
        Commands::BrowserHost(args) => browser_host::run(
            &args.runtime_dir,
            &args.target_url,
            &args.chrome,
            args.headless,
        ),
        Commands::ActivityIdle => loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        },
    }
}

#[derive(Deserialize)]
struct ValidationClaimResponse {
    job: ValidationJob,
}

#[derive(Deserialize)]
struct ValidationJob {
    job_id: String,
    claim_id: String,
    claimed_parent_root: Option<String>,
    download_url: String,
}

fn validate_bundle_queue(args: BundleValidateQueueArgs) -> Result<()> {
    if args.agent_id.trim().is_empty() {
        bail!("validator agent id must not be empty");
    }
    let base = args.api_base.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder().build()?;
    loop {
        let response = client
            .post(format!("{base}/v1/capsule-bundles/validation-jobs/claim"))
            .bearer_auth(&args.token)
            .json(&serde_json::json!({ "agent_id": args.agent_id }))
            .send()?
            .error_for_status()?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            if args.once {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }
        let claimed: ValidationClaimResponse = response.json()?;
        validate_claimed_bundle(&client, base, &args, claimed.job)?;
        if args.once {
            return Ok(());
        }
    }
}

fn validate_claimed_bundle(
    client: &reqwest::blocking::Client,
    base: &str,
    args: &BundleValidateQueueArgs,
    job: ValidationJob,
) -> Result<()> {
    let headers = |request: reqwest::blocking::RequestBuilder| {
        request
            .bearer_auth(&args.token)
            .header("x-ato-validator-agent-id", &args.agent_id)
            .header("x-ato-validation-claim-id", &job.claim_id)
    };
    let bytes = headers(client.get(format!("{base}{}", job.download_url)))
        .send()?
        .error_for_status()?
        .bytes()?;
    let verified = build_bundle_verification_report(&bytes, job.claimed_parent_root.as_deref());
    let body = match verified {
        Ok(report) => {
            let mut body = serde_json::json!({
                "status": "verified",
                "report": report,
            });
            if job.claimed_parent_root.is_some() {
                body["parent_reachable"] = serde_json::Value::Bool(true);
            }
            body
        }
        Err(_) => serde_json::json!({
            "status": "rejected",
            "rejection_code": "validator_failed",
        }),
    };
    headers(client.post(format!(
        "{base}/v1/capsule-bundles/validation-jobs/{}/ack",
        job.job_id
    )))
    .json(&body)
    .send()?
    .error_for_status()?;
    Ok(())
}

#[derive(Serialize)]
struct HostedSessionReport {
    root_computation_ref: String,
    branch: &'static str,
    exported_ports: Vec<HostedPortReport>,
}

#[derive(Serialize)]
struct HostedPortReport {
    port_id: String,
    protocol: String,
    role: String,
    local_endpoint: Option<String>,
}

fn hosted_session(args: HostedSessionArgs) -> Result<()> {
    require_external_sandbox()?;
    match args.command {
        HostedSessionCommands::Start(args) => hosted_session_start(args),
        HostedSessionCommands::Replay(args) => hosted_replay_start(args),
        HostedSessionCommands::Capture(args) => hosted_session_capture(args),
        HostedSessionCommands::Stop(args) => hosted_session_stop(args),
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostedReplayEvent {
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
        current_record: HostedReplayRecord,
    },
    Complete {
        cursor: usize,
        total_records: usize,
        current_head: String,
        surface: HostedSessionReport,
    },
}

#[derive(Serialize)]
struct HostedReplayRecord {
    id: RecordId,
    protocol_id: String,
    port_id: String,
    direction: ato_objects::Direction,
}

fn emit_replay_event(event: &HostedReplayEvent) -> Result<()> {
    println!("{}", serde_json::to_string(event)?);
    std::io::stdout().flush()?;
    Ok(())
}

fn hosted_replay_start(args: HostedReplayStartArgs) -> Result<()> {
    if args.minimum_delay_ms > args.maximum_delay_ms {
        bail!("Replay presentation delay bounds are invalid");
    }
    let bytes = fs::read(&args.bundle)?;
    let references = reference_registry()?;
    let verified = verify_bundle(&decode_bundle(&bytes)?, &references)?;
    let expected = ComputationRef::parse(&args.expected_root)?;
    if verified.root() != &expected {
        bail!("portable Replay root does not match the expected Post root");
    }
    fs::create_dir_all(&args.repository)?;
    let session = PortableSession::import(&bytes, &args.repository, &references)?;
    let replay_descriptor = session
        .context()
        .materializations()
        .iter()
        .find(|candidate| candidate.materializer_id == "ato.replay@1")
        .context("Replay Materialization is unavailable")?;
    let replay_descriptor = ContentRef::parse(&replay_descriptor.descriptor_ref)?;
    let state = load_runtime_state(
        session.context().parent_root(),
        session.context().repository().objects(),
    )?;
    if !state.config.binding.is_empty() {
        bail!("replay_requires_binding");
    }
    const AUDITED: &[&str] = &[
        "ato.process@1",
        "ato.pty@1",
        "ato.workspace@1",
        "ato.binding@1",
        "ato.http@1",
    ];
    if state
        .config
        .adapter
        .iter()
        .any(|adapter| !AUDITED.contains(&adapter.use_adapter.as_str()))
    {
        bail!("replay_requires_non_audited_adapter");
    }
    let adapters = adapter_registry()?;
    let sequence = ReplaySequence::load(
        &replay_descriptor,
        session.context().repository().objects(),
        &adapters,
    )?;
    if sequence.target() != &expected {
        bail!("Replay target does not match the expected Post root");
    }
    if !sequence.required_bindings().is_empty()
        || sequence
            .required_adapters()
            .iter()
            .any(|adapter| !AUDITED.contains(&adapter.as_str()))
    {
        bail!("Replay safety preflight failed");
    }
    let total_records = sequence.records().len();
    let delays = sequence
        .records()
        .windows(2)
        .map(|pair| {
            pair[0]
                .observed_at
                .parse::<u128>()
                .ok()
                .zip(pair[1].observed_at.parse::<u128>().ok())
                .and_then(|(before, after)| after.checked_sub(before))
                .map(|nanos| (nanos / 1_000_000).min(u64::MAX as u128) as u64)
                .unwrap_or(args.minimum_delay_ms)
                .clamp(args.minimum_delay_ms, args.maximum_delay_ms)
        })
        .collect::<Vec<_>>();
    emit_replay_event(&HostedReplayEvent::Prepared {
        anchor_root: sequence.anchor().to_string(),
        target_root: sequence.target().to_string(),
        cursor: 0,
        total_records,
    })?;

    let driver =
        CliRealizationDriver::new(session.context().repository().project(), &BTreeMap::new());
    let mut stepper = ReplayStepper::begin(sequence, &driver)?;
    while let Some(progress) = stepper.step()? {
        emit_replay_event(&HostedReplayEvent::Progress {
            cursor: progress.cursor,
            total_records: progress.total,
            current_head: progress.head_after.to_string(),
            current_record: HostedReplayRecord {
                id: progress.record_id,
                protocol_id: progress.protocol_id.to_string(),
                port_id: progress.port_id.to_string(),
                direction: progress.direction,
            },
        })?;
        if let Some(delay) = delays.get(progress.cursor.saturating_sub(1)) {
            std::thread::sleep(std::time::Duration::from_millis(*delay));
        }
    }
    let mut realization = stepper.finish()?;
    realization.activate()?;
    let computation = resolve_computation(session.context().repository().objects(), &expected)?;
    let selected_endpoint = computation
        .object()
        .boundary
        .get(&ato_computation::PortId::parse(&args.surface_port)?)
        .and_then(|port| runtime_endpoint(&state, &args.surface_port, &port.protocol.to_string()))
        .context("selected Replay Port has no runtime endpoint")?;
    start_unix_surface_relay(&args.surface_relay, selected_endpoint.parse()?)?;
    let exported_ports = computation
        .object()
        .boundary
        .iter()
        .map(|(id, port)| HostedPortReport {
            port_id: id.to_string(),
            protocol: port.protocol.to_string(),
            role: port.role.to_string(),
            local_endpoint: (id.as_str() == args.surface_port)
                .then(|| "unix:surface.sock".to_owned()),
        })
        .collect();
    emit_replay_event(&HostedReplayEvent::Complete {
        cursor: total_records,
        total_records,
        current_head: expected.to_string(),
        surface: HostedSessionReport {
            root_computation_ref: expected.to_string(),
            branch: "replay-session",
            exported_ports,
        },
    })?;
    if args.hold {
        let waited = realization.wait();
        let quiesced = realization.quiesce();
        waited.and(quiesced)?;
    } else {
        realization.quiesce()?;
    }
    Ok(())
}

fn require_external_sandbox() -> Result<()> {
    let profile = std::env::var("ATO_EXTERNAL_SANDBOX_PROFILE").unwrap_or_default();
    if !external_sandbox_profile_supported(&profile) {
        bail!(
            "hosted portable Capsule execution requires an external sandbox profile; refusing host execution"
        );
    }
    Ok(())
}

fn external_sandbox_profile_supported(profile: &str) -> bool {
    profile == "untrusted-v1"
}

fn hosted_session_start(args: HostedSessionStartArgs) -> Result<()> {
    let bytes = fs::read(&args.bundle)?;
    let references = reference_registry()?;
    let verified = verify_bundle(&decode_bundle(&bytes)?, &references)?;
    let expected = ComputationRef::parse(&args.expected_root)?;
    if verified.root() != &expected {
        bail!(
            "portable Capsule root {} does not match expected {expected}",
            verified.root()
        );
    }
    fs::create_dir_all(&args.repository)?;
    let mut session = PortableSession::import(&bytes, &args.repository, &references)?;
    let state = load_runtime_state(
        session.context().parent_root(),
        session.context().repository().objects(),
    )?;
    let bindings: BTreeMap<String, String> = if args.bindings_stdin {
        let mut payload = String::new();
        std::io::stdin().read_to_string(&mut payload)?;
        serde_json::from_str(&payload).context("invalid hosted Binding payload")?
    } else {
        args.bindings.into_iter().collect()
    };
    let missing: Vec<_> = state
        .config
        .binding
        .iter()
        .filter(|binding| !bindings.contains_key(&binding.id))
        .map(|binding| binding.id.clone())
        .collect();
    if !missing.is_empty() {
        bail!("portable Capsule requires Bindings: {}", missing.join(", "));
    }
    preflight(session.context().repository(), &state.config, &bindings)?;
    let replay_descriptor = session
        .context()
        .materializations()
        .iter()
        .find(|candidate| candidate.materializer_id == "ato.replay@1")
        .context("hosted continuation has no Replay Materialization")?;
    let replay_descriptor = ContentRef::parse(&replay_descriptor.descriptor_ref)?;
    session.start(&DurablePortableRuntimeFactory {
        bindings,
        replay_descriptor,
    })?;
    let computation = resolve_computation(
        session.context().repository().objects(),
        session.context().parent_root(),
    )?;
    let selected_endpoint = computation
        .object()
        .boundary
        .get(&ato_computation::PortId::parse(&args.surface_port)?)
        .and_then(|port| runtime_endpoint(&state, &args.surface_port, &port.protocol.to_string()))
        .context("selected exported Port has no runtime endpoint")?;
    start_unix_surface_relay(&args.surface_relay, selected_endpoint.parse()?)?;
    let exported_ports = computation
        .object()
        .boundary
        .iter()
        .map(|(id, port)| HostedPortReport {
            port_id: id.to_string(),
            protocol: port.protocol.to_string(),
            role: port.role.to_string(),
            local_endpoint: if id.as_str() == args.surface_port {
                Some("unix:surface.sock".to_owned())
            } else {
                None
            },
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string(&HostedSessionReport {
            root_computation_ref: expected.to_string(),
            branch: ato_runtime::PORTABLE_SESSION_BRANCH,
            exported_ports,
        })?
    );
    if args.hold {
        session.wait()?;
    }
    Ok(())
}

fn runtime_endpoint(
    state: &authoring::AuthoringState,
    port_id: &str,
    protocol: &str,
) -> Option<String> {
    if protocol == "ato.pty@1" {
        std::env::var("ATO_PTY_GATEWAY_LISTEN").ok()
    } else {
        state
            .config
            .adapter
            .iter()
            .find(|adapter| adapter.port.as_deref() == Some(port_id))
            .and_then(|adapter| adapter.listen.clone())
    }
}

#[cfg(unix)]
fn start_unix_surface_relay(path: &Path, target: SocketAddr) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(client) = connection else {
                break;
            };
            std::thread::spawn(move || relay_surface_connection(client, target));
        }
    });
    Ok(())
}

#[cfg(not(unix))]
fn start_unix_surface_relay(_path: &Path, _target: std::net::SocketAddr) -> Result<()> {
    bail!("untrusted-v1 surface relay requires Unix sockets")
}

#[cfg(unix)]
fn relay_surface_connection(mut client: UnixStream, target: SocketAddr) {
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
        let _ = upstream_write.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut upstream, &mut client);
    let _ = client.shutdown(Shutdown::Write);
    let _ = forward.join();
}

fn hosted_session_capture(args: HostedSessionCaptureArgs) -> Result<()> {
    let repository = LocalCapsuleRepository::open(args.repository)?;
    let lease = capture_active(&repository, ato_runtime::PORTABLE_SESSION_BRANCH)?;
    let imported = decode_bundle(&fs::read(repository.project().join("input.capsule"))?)?;
    let parent_replay = imported
        .index
        .materializations
        .iter()
        .find(|candidate| candidate.materializer_id == "ato.replay@1")
        .context("hosted continuation has no parent Replay Materialization")?;
    let mut records = records_for_descriptor(
        &ContentRef::parse(&parent_replay.descriptor_ref)?,
        repository.objects(),
    )?;
    records.extend(repository.records_for_causal_branch(&lease.branch, Some(lease.record_seq))?);
    let export = encap_target(
        &repository,
        &lease.target,
        &records,
        &EncapArgs {
            selector: String::new(),
            current: true,
            materializers: args.materializers,
            output: args.output,
            presentation_output: None,
        },
    );
    let root = lease.target.clone();
    let presentation = args
        .presentation_output
        .as_deref()
        .map(|output| supervisor::export_capture_presentations(&lease, output))
        .transpose();
    let release = lease.release();
    export?;
    presentation?;
    release?;
    println!("{root}");
    Ok(())
}

fn hosted_session_stop(args: HostedSessionStopArgs) -> Result<()> {
    let repository = LocalCapsuleRepository::open(args.repository)?;
    let stopped = stop_active(&repository)?.context("hosted session has no active Run")?;
    let head = evolve_workspace(&repository, &stopped.branch, &stopped.head)?;
    repository.update_head(&stopped.branch, Some(&stopped.branch_base), &head)?;
    repository.release_active_run(&stopped.token)?;
    println!("{head}");
    Ok(())
}

#[derive(Serialize)]
struct BundleVerificationReport {
    format_version: u32,
    transport_digest: String,
    root_computation_ref: String,
    materializations: Vec<VerifiedMaterialization>,
    exported_ports: Vec<VerifiedPort>,
    structure: CapsuleStructureNode,
    required_bindings: Vec<VerifiedBinding>,
    workspace_file_count: usize,
    object_count: usize,
    decoded_size: u64,
    validation: VerificationResult,
}

#[derive(Serialize)]
struct VerifiedMaterialization {
    id: String,
    restore_capability: &'static str,
}

#[derive(Serialize)]
struct VerifiedPort {
    port_id: String,
    protocol: String,
    role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CapsuleStructureNode {
    computation_ref: String,
    label: Option<String>,
    exported_ports: Vec<StructurePort>,
    children: Vec<CapsuleStructureNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct StructurePort {
    protocol: String,
    role: String,
}

const MAX_STRUCTURE_DEPTH: usize = 16;
const MAX_STRUCTURE_NODES: usize = 512;
const MAX_STRUCTURE_CHILDREN: usize = 128;
const MAX_STRUCTURE_PORTS: usize = 128;

#[derive(Serialize)]
struct VerifiedBinding {
    id: String,
    schema: String,
}

#[derive(Serialize)]
struct VerificationResult {
    status: &'static str,
}

fn verify_bundle_command(args: BundleVerifyArgs) -> Result<()> {
    if !args.json {
        bail!("internal bundle verification requires --json");
    }
    let bytes = fs::read(&args.capsule)?;
    match build_bundle_verification_report(&bytes, args.require_computation.as_deref()) {
        Ok(report) => {
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Err(_) => {
            println!("{{\"validation\":{{\"status\":\"rejected\"}}}}");
            bail!("bundle verification failed")
        }
    }
}

fn compose_bundle_command(args: BundleComposeArgs) -> Result<()> {
    if !args.json {
        bail!("internal bundle composition requires --json");
    }
    if args.output.exists() {
        bail!("compose output already exists: {}", args.output.display());
    }
    let inputs = args
        .inputs
        .iter()
        .map(fs::read)
        .collect::<std::io::Result<Vec<_>>>()?;
    let (root, bytes) = compose_portable_bundles(&inputs)?;
    atomic_write(&args.output, &bytes)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "root_computation_ref": root,
            "input_count": inputs.len(),
        }))?
    );
    Ok(())
}

fn compose_portable_bundles(inputs: &[Vec<u8>]) -> Result<(String, Vec<u8>)> {
    if !(2..=8).contains(&inputs.len()) {
        bail!("Compose requires between 2 and 8 input Capsules");
    }
    let references = reference_registry()?;
    let objects = MemoryObjectStore::default();
    let mut nodes = BTreeMap::new();
    let mut exports = BTreeMap::new();
    let mut boundary = BTreeMap::new();
    for (index, bytes) in inputs.iter().enumerate() {
        let bundle = decode_bundle(bytes)?;
        let root = import_bundle(&bundle, &objects, &references)?;
        let node = NodeId::parse(format!("capsule{:02}", index + 1))?;
        let computation = resolve_computation(&objects, &root)?;
        for (port_index, (port_id, port)) in computation.object().boundary.iter().enumerate() {
            let exported = PortId::parse(format!("c{:02}.p{:03}", index + 1, port_index + 1))?;
            boundary.insert(exported.clone(), port.clone());
            exports.insert(
                exported,
                Endpoint {
                    node: node.clone(),
                    port: port_id.clone(),
                },
            );
        }
        nodes.insert(node, root);
    }
    let residual = objects.put(&encode_composite_residual(&CompositeResidual {
        nodes,
        connections: Vec::new(),
        exports,
    })?)?;
    let computation = ComputationObject {
        semantics: SemanticsId::parse(COMPOSE_SEMANTICS_ID)?,
        boundary,
        residual,
    };
    let root = computation_ref(&computation)?;
    objects.insert(
        root.content_ref(),
        &encode_computation_object(&computation)?,
    )?;
    let bundle = export_bundle(&root, &objects, &references)?;
    Ok((root.to_string(), encode_bundle(&bundle)?))
}

fn build_bundle_verification_report(
    bytes: &[u8],
    required_computation: Option<&str>,
) -> Result<BundleVerificationReport> {
    let bundle = decode_bundle(bytes)?;
    let references = reference_registry()?;
    let verified = verify_bundle(&bundle, &references)?;
    let root = verified.root().clone();
    if let Some(required) = required_computation {
        let required = ComputationRef::parse(required)?;
        resolve_computation(verified.objects(), &required)
            .context("required parent computation is absent from the portable closure")?;
    }
    let adapters = adapter_registry()?;
    let materializers = materializer_registry()?;
    let state = load_runtime_state(&root, verified.objects())?;
    let workspace_snapshot = ContentRef::parse(&state.workspace_snapshot)?;
    let workspace_file_count =
        workspace_snapshot_file_count(&workspace_snapshot, verified.objects())?;
    let policy = workspace_policy(&state.config)?;
    let context = MaterializerContext {
        objects: verified.objects(),
        adapters: &adapters,
        records: &[],
        workspace: Path::new("."),
        workspace_policy: &policy,
        realization: None,
    };
    let mut reported_materializations = Vec::new();
    for entry in &bundle.index.materializations {
        let descriptor = ContentRef::parse(&entry.descriptor_ref)?;
        let materializer = materializers.get(&entry.materializer_id)?;
        let target = materializer.validate(&descriptor, &context)?;
        if target != root {
            bail!(
                "materializer `{}` targets a different computation",
                entry.materializer_id
            );
        }
        reported_materializations.push(VerifiedMaterialization {
            id: entry.materializer_id.clone(),
            restore_capability: match materializer.restore_capability() {
                RestoreCapability::Supported => "supported",
                RestoreCapability::VerifyOnly => "verify_only",
            },
        });
    }
    let computation = resolve_computation(verified.objects(), &root)?;
    let exported_ports = computation
        .object()
        .boundary
        .iter()
        .map(|(id, port)| VerifiedPort {
            port_id: id.to_string(),
            protocol: port.protocol.to_string(),
            role: port.role.to_string(),
        })
        .collect();
    let structure = build_structure_projection(verified.objects(), &root)?;
    let required_bindings = state
        .config
        .binding
        .iter()
        .map(|binding| VerifiedBinding {
            id: binding.id.clone(),
            schema: binding.protocol.clone(),
        })
        .collect();
    Ok(BundleVerificationReport {
        format_version: bundle.index.version,
        transport_digest: format!("sha256:{:x}", Sha256::digest(bytes)),
        root_computation_ref: root.to_string(),
        materializations: reported_materializations,
        exported_ports,
        structure,
        required_bindings,
        workspace_file_count,
        object_count: verified.object_count(),
        decoded_size: verified.decoded_size(),
        validation: VerificationResult { status: "valid" },
    })
}

fn build_structure_projection(
    objects: &dyn ato_objects::ObjectResolver,
    root: &ComputationRef,
) -> Result<CapsuleStructureNode> {
    let mut node_count = 0;
    project_structure_node(objects, root, None, 1, &mut node_count)
}

fn project_structure_node(
    objects: &dyn ato_objects::ObjectResolver,
    reference: &ComputationRef,
    label: Option<String>,
    depth: usize,
    node_count: &mut usize,
) -> Result<CapsuleStructureNode> {
    if depth > MAX_STRUCTURE_DEPTH {
        bail!("Capsule Structure exceeds maximum depth {MAX_STRUCTURE_DEPTH}")
    }
    *node_count = node_count.saturating_add(1);
    if *node_count > MAX_STRUCTURE_NODES {
        bail!("Capsule Structure exceeds maximum node count {MAX_STRUCTURE_NODES}")
    }
    let computation = resolve_computation(objects, reference)?;
    if computation.object().boundary.len() > MAX_STRUCTURE_PORTS {
        bail!("Capsule Structure node exceeds maximum exported port count")
    }
    let exported_ports = computation
        .object()
        .boundary
        .values()
        .map(|port| StructurePort {
            protocol: port.protocol.to_string(),
            role: port.role.to_string(),
        })
        .collect();
    let children = if computation.object().semantics.as_str() == COMPOSE_SEMANTICS_ID {
        if depth == MAX_STRUCTURE_DEPTH {
            bail!("Capsule Structure exceeds maximum depth {MAX_STRUCTURE_DEPTH}")
        }
        let residual = &computation.object().residual;
        let metadata = objects.metadata(residual)?;
        let bytes = read_exact_object(
            objects,
            residual,
            metadata.size,
            MAX_COMPOSITE_RESIDUAL_BYTES,
        )?;
        let composite = decode_composite_residual(&bytes)?;
        if composite.nodes.len() > MAX_STRUCTURE_CHILDREN {
            bail!("Capsule Structure node exceeds maximum child count")
        }
        composite
            .nodes
            .iter()
            .map(|(node, child)| {
                project_structure_node(
                    objects,
                    child,
                    Some(node.to_string()),
                    depth + 1,
                    node_count,
                )
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    Ok(CapsuleStructureNode {
        computation_ref: reference.to_string(),
        label,
        exported_ports,
        children,
    })
}

fn init(args: InitArgs) -> Result<()> {
    let project = project_path(&args.capsule, true)?;
    let repository = LocalCapsuleRepository::open(&project)?;
    if repository.head("main")?.is_some() {
        bail!(
            "Capsule is already initialized at {}",
            repository.root().display()
        );
    }
    let config = load_config(&project)?;
    let bindings: BTreeMap<_, _> = args.bindings.iter().cloned().collect();
    preflight(&repository, &config, &bindings)?;
    let initial = initial_computation(&repository, config)?;
    repository.create_branch("main", &initial, None)?;
    println!("{initial}");
    if !args.initial_only {
        start_durable(&repository, "main", &initial, &bindings, None)?;
    }
    Ok(())
}

fn resume(args: ResumeArgs) -> Result<()> {
    let selector: CapsuleSelector = args.selector.parse()?;
    let project = project_path(&selector.capsule, false)?;
    let repository = LocalCapsuleRepository::open(project)?;
    let selected = repository.resolve(&selector)?;
    let selected_state = load_runtime_state(&selected, repository.objects())?;
    restore_workspace(
        &ContentRef::parse(&selected_state.workspace_snapshot)?,
        repository.project(),
        repository.objects(),
    )?;
    let current = repository
        .head(&selector.branch)?
        .ok_or_else(|| anyhow::anyhow!("unknown branch `{}`", selector.branch))?;
    let branch = match args.branch {
        Some(branch) => {
            if repository.head(&branch)?.is_some() {
                bail!("branch `{branch}` already exists");
            }
            let parent_record = match selector.record {
                Some(seq) => Some(RecordId::new(&selector.branch, seq)),
                None => repository
                    .records_for_stream(&selector.branch, None)?
                    .last()
                    .map(|record| record.id.clone()),
            };
            repository.create_branch(
                &branch,
                &selected,
                Some(&BranchOrigin {
                    computation: selected.clone(),
                    parent_record,
                }),
            )?;
            branch
        }
        None if selected != current => bail!(
            "historical point {}@{}#{} is not the current head; use --branch <name>",
            selector.capsule,
            selector.branch,
            selector.record.expect("historical selection")
        ),
        None => selector.branch,
    };
    let replay_records = repository.records_for_causal_branch(&branch, None)?;
    start_durable(
        &repository,
        &branch,
        &selected,
        &args.bindings.into_iter().collect(),
        Some(&replay_records),
    )?;
    println!("resumed {branch} at {selected}");
    Ok(())
}

fn stop(capsule: &str) -> Result<()> {
    let project = project_path(capsule, false)?;
    let repository = LocalCapsuleRepository::open(project)?;
    repository
        .active_run()?
        .context("Capsule has no active Run")?;
    let stopped = stop_active(&repository)?.context("Capsule has no active Run")?;
    let head = evolve_workspace(&repository, &stopped.branch, &stopped.head)?;
    repository.update_head(&stopped.branch, Some(&stopped.branch_base), &head)?;
    repository.release_active_run(&stopped.token)?;
    println!("sealed {} at {head}", stopped.branch);
    Ok(())
}

fn encap(args: EncapArgs) -> Result<()> {
    let target = encap_impl(args)?;
    println!("{target}");
    Ok(())
}

fn encap_impl(args: EncapArgs) -> Result<ComputationRef> {
    let selector: CapsuleSelector = args.selector.parse()?;
    let project = project_path(&selector.capsule, false)?;
    let repository = LocalCapsuleRepository::open(project)?;
    if args.current && selector.record.is_some() {
        bail!("--current cannot be combined with a historical #record selector");
    }
    let lease = if args.current {
        Some(capture_active(&repository, &selector.branch)?)
    } else {
        None
    };
    let (target, records) = match lease.as_ref() {
        Some(lease) => {
            if lease.branch != selector.branch {
                bail!("capture worker returned a different branch");
            }
            (
                lease.target.clone(),
                repository.records_for_causal_branch(&selector.branch, Some(lease.record_seq))?,
            )
        }
        None => (
            repository.resolve(&selector)?,
            repository.records_for_causal_branch(&selector.branch, selector.record)?,
        ),
    };
    let export_result = encap_target(&repository, &target, &records, &args);
    let presentation_result = match (lease.as_ref(), args.presentation_output.as_deref()) {
        (Some(lease), Some(output)) => supervisor::export_capture_presentations(lease, output),
        (None, Some(_)) => bail!("--presentation-output requires --current"),
        _ => Ok(()),
    };
    let release_result = match lease {
        Some(lease) => lease.release(),
        None => Ok(()),
    };
    export_result?;
    presentation_result?;
    release_result?;
    Ok(target)
}

#[derive(Deserialize)]
struct PreparedShareBundle {
    bundle_id: String,
    upload_url: String,
    upload_direct: bool,
}

#[derive(Deserialize)]
struct ShareBundleEnvelope {
    bundle: ShareBundleStatus,
}

#[derive(Deserialize)]
struct ShareBundleStatus {
    validation_status: String,
    rejection_code: Option<String>,
}

#[derive(Deserialize)]
struct CreatedSharePostEnvelope {
    capsule_post: CreatedSharePost,
}

#[derive(Deserialize)]
struct CreatedSharePost {
    share_path: String,
}

struct ShareTempFile(PathBuf);

impl Drop for ShareTempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn share(args: ShareArgs) -> Result<()> {
    let selector: CapsuleSelector = args.selector.parse()?;
    let project = project_path(&selector.capsule, false)?;
    let repository = LocalCapsuleRepository::open(project)?;
    let active_current = selector.record.is_none()
        && repository
            .active_run()?
            .is_some_and(|run| run.status == "active" && run.branch == selector.branch);
    let temp_dir = repository.root().join("tmp");
    fs::create_dir_all(&temp_dir)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp =
        ShareTempFile(temp_dir.join(format!("share-{}-{nonce}.capsule", std::process::id())));
    let target = encap_impl(EncapArgs {
        selector: args.selector.clone(),
        current: active_current,
        materializers: args.materializers,
        output: temp.0.clone(),
        presentation_output: None,
    })?;
    let bytes = fs::read(&temp.0)?;
    let report = build_bundle_verification_report(&bytes, None)?;

    println!("Share safety summary");
    println!("  Included");
    println!("    {} workspace files", report.workspace_file_count);
    for materialization in &report.materializations {
        println!(
            "    {} ({})",
            materialization.id, materialization.restore_capability
        );
    }
    println!("  Rebound by recipient");
    if report.required_bindings.is_empty() {
        println!("    none");
    } else {
        for binding in &report.required_bindings {
            println!("    {}", binding.id);
        }
    }
    println!("  Excluded by default");
    println!("    .env, SSH keys, cloud credential directories");
    println!(
        "Common credential files are excluded by default. Review included state before publishing."
    );
    if !args.yes {
        print!("Create an unlisted Capsule URL? [y/N] ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("share cancelled");
        }
    }

    let base = args.api_base.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder().build()?;
    let prepared: PreparedShareBundle = client
        .post(format!("{base}/v1/capsule-bundles/prepare"))
        .bearer_auth(&args.device_token)
        .json(&serde_json::json!({
            "transport_digest": report.transport_digest,
            "size_bytes": bytes.len(),
        }))
        .send()?
        .error_for_status()?
        .json()?;
    let mut upload = client
        .put(&prepared.upload_url)
        .header("content-type", "application/vnd.ato.capsule")
        .body(bytes);
    if !prepared.upload_direct {
        upload = upload.bearer_auth(&args.device_token);
    }
    upload.send()?.error_for_status()?;
    if prepared.upload_direct {
        client
            .post(format!(
                "{base}/v1/capsule-bundles/{}/complete",
                prepared.bundle_id
            ))
            .bearer_auth(&args.device_token)
            .send()?
            .error_for_status()?;
    }
    println!("Capsule root: {target}");
    println!("Upload status: validating");

    let ready = loop {
        let status: ShareBundleEnvelope = client
            .get(format!("{base}/v1/capsule-bundles/{}", prepared.bundle_id))
            .bearer_auth(&args.device_token)
            .send()?
            .error_for_status()?
            .json()?;
        match status.bundle.validation_status.as_str() {
            "ready" => break status.bundle,
            "rejected" => bail!(
                "bundle validation rejected: {}",
                status
                    .bundle
                    .rejection_code
                    .unwrap_or_else(|| "unspecified".to_owned())
            ),
            _ => std::thread::sleep(std::time::Duration::from_secs(2)),
        }
    };
    debug_assert_eq!(ready.validation_status, "ready");
    let post: CreatedSharePostEnvelope = client
        .post(format!("{base}/v1/capsule-posts"))
        .bearer_auth(&args.device_token)
        .json(&serde_json::json!({
            "bundle_id": prepared.bundle_id,
            "title": args.title,
            "description": args.description,
        }))
        .send()?
        .error_for_status()?
        .json()?;
    println!("Upload status: ready");
    println!(
        "Share URL: {}{}",
        args.share_base.trim_end_matches('/'),
        post.capsule_post.share_path
    );
    Ok(())
}

fn encap_target(
    repository: &LocalCapsuleRepository,
    target: &ComputationRef,
    records: &[ato_objects::RecordEnvelope],
    args: &EncapArgs,
) -> Result<()> {
    let state = load_runtime_state(target, repository.objects())?;
    let adapters = adapter_registry()?;
    let materializers = materializer_registry()?;
    let capture_policy = workspace_policy(&state.config)?;
    let selected = if args.materializers.is_empty() {
        if state.config.encap.materializers.is_empty() {
            vec!["ato.replay@1".to_owned()]
        } else {
            state.config.encap.materializers.clone()
        }
    } else {
        args.materializers.clone()
    };
    let context = MaterializerContext {
        objects: repository.objects(),
        adapters: &adapters,
        records,
        workspace: repository.project(),
        workspace_policy: &capture_policy,
        realization: None,
    };
    let mut entries = Vec::new();
    for id in selected {
        let materializer = materializers.get(&id)?;
        let descriptor = materializer.encode(target, &context)?;
        let verified = materializer.verify(&descriptor, &context)?;
        if &verified != target {
            bail!("materializer `{id}` verified a different computation {verified}");
        }
        entries.push(BundleMaterialization {
            materializer_id: id,
            descriptor_ref: descriptor.to_string(),
        });
    }
    let references = reference_registry()?;
    let bundle =
        export_bundle_with_materializations(target, &entries, repository.objects(), &references)?;
    atomic_write(&args.output, &encode_bundle(&bundle)?)?;
    Ok(())
}

fn run_capsule(args: RunArgs) -> Result<()> {
    if args.capsule.extension().and_then(|value| value.to_str()) != Some("capsule")
        || !args.capsule.is_file()
    {
        bail!(
            "`ato run` accepts only a portable .capsule file; author repositories with `ato init`"
        );
    }
    let cache = ato_home()?.join("cache");
    fs::create_dir_all(&cache)?;
    let runtime = tempfile::Builder::new()
        .prefix("portable-run-")
        .tempdir_in(cache)?;
    let project = runtime.path().join("workspace");
    fs::create_dir_all(&project)?;
    let references = reference_registry()?;
    let mut session = PortableSession::import(&fs::read(&args.capsule)?, &project, &references)?;
    let state = load_runtime_state(
        session.context().parent_root(),
        session.context().repository().objects(),
    )?;
    let bindings: BTreeMap<_, _> = args.bindings.into_iter().collect();
    let missing: Vec<_> = state
        .config
        .binding
        .iter()
        .filter(|binding| !bindings.contains_key(&binding.id))
        .map(|binding| binding.id.clone())
        .collect();
    if !missing.is_empty() {
        bail!("portable Capsule requires Bindings: {}", missing.join(", "));
    }
    let repository = session.context().repository().clone();
    let parent_root = session.context().parent_root().clone();
    session.start(&CliPortableRuntimeFactory { bindings })?;
    let waited = session.wait();
    let stopped = session.stop();
    waited?;
    stopped?;
    let continued = repository
        .head(PortableContinuationCapture::BRANCH)?
        .context("portable continuation branch is missing")?;
    if continued != parent_root {
        let retained = runtime.keep();
        eprintln!("continued computation: {continued}");
        eprintln!(
            "continuation workspace: {}",
            retained.join("workspace").display()
        );
    }
    Ok(())
}

struct CliPortableRuntimeFactory {
    bindings: BTreeMap<String, String>,
}

/// Hosted sessions use the same durable supervisor as authored local Runs.
/// Only the lifecycle wrapper is reusable; lineage remains in ato-objects.
struct DurablePortableRuntimeFactory {
    bindings: BTreeMap<String, String>,
    replay_descriptor: ContentRef,
}

impl PortableRuntimeFactory for DurablePortableRuntimeFactory {
    fn create(
        &self,
        session: &PortableSessionContext,
    ) -> Result<Box<dyn PortableSessionRuntime>, PortableSessionError> {
        Ok(Box::new(DurablePortableRuntime {
            repository: session.repository().clone(),
            root: session.parent_root().clone(),
            bindings: self.bindings.clone(),
            replay_descriptor: self.replay_descriptor.clone(),
            started: false,
        }))
    }
}

struct DurablePortableRuntime {
    repository: LocalCapsuleRepository,
    root: ComputationRef,
    bindings: BTreeMap<String, String>,
    replay_descriptor: ContentRef,
    started: bool,
}

impl PortableSessionRuntime for DurablePortableRuntime {
    fn start(&mut self) -> Result<(), PortableSessionError> {
        start_durable_with_descriptor(
            &self.repository,
            ato_runtime::PORTABLE_SESSION_BRANCH,
            &self.root,
            &self.bindings,
            Some(&self.replay_descriptor),
        )
        .map_err(portable_runtime_error)?;
        self.started = true;
        Ok(())
    }

    fn wait(&mut self) -> Result<(), PortableSessionError> {
        while self
            .repository
            .active_run()
            .map_err(portable_runtime_error)?
            .is_some_and(|run| run.status == "active")
        {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(())
    }

    fn current_head(&self) -> Result<ComputationRef, PortableSessionError> {
        if let Some(run) = self
            .repository
            .active_run()
            .map_err(portable_runtime_error)?
        {
            return Ok(run.head);
        }
        self.repository
            .head(ato_runtime::PORTABLE_SESSION_BRANCH)
            .map_err(portable_runtime_error)?
            .ok_or(PortableSessionError::MissingBranch)
    }

    fn encap_current(&mut self, output: &Path) -> Result<ComputationRef, PortableSessionError> {
        let lease = capture_active(&self.repository, ato_runtime::PORTABLE_SESSION_BRANCH)
            .map_err(portable_runtime_error)?;
        let records = self
            .repository
            .records_for_causal_branch(&lease.branch, Some(lease.record_seq))
            .map_err(portable_runtime_error)?;
        let target = lease.target.clone();
        let export = encap_target(
            &self.repository,
            &target,
            &records,
            &EncapArgs {
                selector: String::new(),
                current: true,
                materializers: Vec::new(),
                output: output.to_path_buf(),
                presentation_output: None,
            },
        );
        let release = lease.release();
        export.map_err(portable_runtime_error)?;
        release.map_err(portable_runtime_error)?;
        Ok(target)
    }

    fn stop(&mut self) -> Result<(), PortableSessionError> {
        if !self.started {
            return Ok(());
        }
        let stopped = stop_active(&self.repository)
            .map_err(portable_runtime_error)?
            .ok_or_else(|| PortableSessionError::Runtime("active Run disappeared".to_owned()))?;
        let head = evolve_workspace(&self.repository, &stopped.branch, &stopped.head)
            .map_err(portable_runtime_error)?;
        self.repository
            .update_head(&stopped.branch, Some(&stopped.branch_base), &head)
            .map_err(portable_runtime_error)?;
        self.repository
            .release_active_run(&stopped.token)
            .map_err(portable_runtime_error)?;
        self.started = false;
        Ok(())
    }
}

impl PortableRuntimeFactory for CliPortableRuntimeFactory {
    fn create(
        &self,
        session: &PortableSessionContext,
    ) -> Result<Box<dyn PortableSessionRuntime>, PortableSessionError> {
        let repository = session.repository();
        let root = session.parent_root();
        let state =
            load_runtime_state(root, repository.objects()).map_err(portable_runtime_error)?;
        let adapters = adapter_registry().map_err(portable_runtime_error)?;
        let materializers = materializer_registry().map_err(portable_runtime_error)?;
        let capture_policy = workspace_policy(&state.config).map_err(portable_runtime_error)?;
        let capture = PortableContinuationCapture::begin(repository, root, &self.bindings)
            .map_err(portable_runtime_error)?;
        let context = MaterializerContext {
            objects: repository.objects(),
            adapters: &adapters,
            records: &[],
            workspace: repository.project(),
            workspace_policy: &capture_policy,
            realization: Some(capture.driver()),
        };
        let mut candidates = session.materializations().to_vec();
        candidates.sort_by(|left, right| left.materializer_id.cmp(&right.materializer_id));
        let mut diagnostics = Vec::new();
        let mut restored = None;
        for candidate in candidates {
            let descriptor =
                ContentRef::parse(&candidate.descriptor_ref).map_err(portable_runtime_error)?;
            let materializer = match materializers.get(&candidate.materializer_id) {
                Ok(materializer) => materializer,
                Err(_) => {
                    diagnostics.push(format!(
                        "{}: implementation missing",
                        candidate.materializer_id
                    ));
                    continue;
                }
            };
            if materializer.restore_capability() != RestoreCapability::Supported {
                diagnostics.push(format!("{}: verify-only", candidate.materializer_id));
                continue;
            }
            if materializer.compatibility(&descriptor, &context) != Compatibility::Compatible {
                diagnostics.push(format!("{}: incompatible", candidate.materializer_id));
                continue;
            }
            restored = Some(
                materializer
                    .restore(&descriptor, &context)
                    .map_err(portable_runtime_error)?,
            );
            break;
        }
        let realization = restored.ok_or_else(|| {
            PortableSessionError::Runtime(format!(
                "no compatible restore-capable Materialization: {}",
                diagnostics.join("; ")
            ))
        })?;
        if realization.target() != root {
            return Err(PortableSessionError::Runtime(format!(
                "Materialization restored {}, expected bundle root {root}",
                realization.target()
            )));
        }
        if realization.verification() == RealizationVerification::AppliedUnverified {
            eprintln!(
                "replay applied all Records for {root}; target state remains unverified without an independent Contract"
            );
        }
        Ok(Box::new(CliPortableRuntime {
            realization,
            capture: Some(capture),
            repository: repository.clone(),
            parent_root: root.clone(),
        }))
    }
}

struct CliPortableRuntime {
    realization: Box<dyn ato_materializer_api::Realization>,
    capture: Option<PortableContinuationCapture>,
    repository: LocalCapsuleRepository,
    parent_root: ComputationRef,
}

impl PortableSessionRuntime for CliPortableRuntime {
    fn start(&mut self) -> Result<(), PortableSessionError> {
        self.realization.activate().map_err(portable_runtime_error)
    }

    fn wait(&mut self) -> Result<(), PortableSessionError> {
        self.realization.wait().map_err(portable_runtime_error)
    }

    fn current_head(&self) -> Result<ComputationRef, PortableSessionError> {
        if let Some(run) = self
            .repository
            .active_run()
            .map_err(portable_runtime_error)?
        {
            return Ok(run.head);
        }
        self.repository
            .head(PortableContinuationCapture::BRANCH)
            .map_err(portable_runtime_error)?
            .ok_or(PortableSessionError::MissingBranch)
    }

    fn encap_current(&mut self, _output: &Path) -> Result<ComputationRef, PortableSessionError> {
        Err(PortableSessionError::Runtime(
            "ephemeral `ato run` does not retain an authored session".to_owned(),
        ))
    }

    fn stop(&mut self) -> Result<(), PortableSessionError> {
        let quiesced = self.realization.quiesce().map_err(portable_runtime_error);
        let observed = self
            .capture
            .take()
            .ok_or_else(|| {
                PortableSessionError::Runtime(
                    "portable continuation capture was already finalized".to_owned(),
                )
            })?
            .finish()
            .map_err(portable_runtime_error);
        quiesced?;
        let observed = observed?;
        let continued = evolve_workspace(
            &self.repository,
            PortableContinuationCapture::BRANCH,
            &observed,
        )
        .map_err(portable_runtime_error)?;
        self.repository
            .update_head(
                PortableContinuationCapture::BRANCH,
                Some(&self.parent_root),
                &continued,
            )
            .map_err(portable_runtime_error)
    }
}

fn portable_runtime_error(error: impl std::fmt::Display) -> PortableSessionError {
    PortableSessionError::Runtime(error.to_string())
}

pub(crate) fn adapter_registry() -> Result<AdapterRegistry> {
    let mut registry = AdapterRegistry::default();
    registry.register(Arc::new(ProcessLifecycleAdapter))?;
    registry.register(Arc::new(PtyAdapter))?;
    registry.register(Arc::new(WorkspaceAdapter))?;
    registry.register(Arc::new(BindingAdapter))?;
    registry.register(Arc::new(HttpAdapter))?;
    registry.register(Arc::new(BrowserAdapter))?;
    Ok(registry)
}

fn materializer_registry() -> Result<MaterializerRegistry> {
    let mut registry = MaterializerRegistry::default();
    registry.register(Arc::new(ReplayMaterializer))?;
    registry.register(Arc::new(SnapshotMaterializer))?;
    Ok(registry)
}

fn reference_registry() -> Result<ReferenceRegistry> {
    let mut registry = ReferenceRegistry::default();
    registry.register(Arc::new(AuthoringReferences::new()))?;
    registry.register(Arc::new(ComposeReferences::default()))?;
    registry.register_materializer(Arc::new(ReplayReferences))?;
    registry.register_materializer(Arc::new(SnapshotReferences))?;
    Ok(registry)
}

fn preflight(
    repository: &LocalCapsuleRepository,
    config: &authoring::AuthoringConfig,
    bindings: &BTreeMap<String, String>,
) -> Result<()> {
    let registry = adapter_registry()?;
    let context = AdapterContext {
        workspace: repository.project(),
        objects: repository.objects(),
    };
    for instance in authoring::adapter_instances(config, bindings, false, false)? {
        registry
            .get(&instance.adapter_id)?
            .preflight(&instance, &context)?;
    }
    Ok(())
}

fn project_path(value: &str, create: bool) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if create {
        fs::create_dir_all(&path)?;
    }
    if !path.is_dir() {
        bail!("local Capsule project does not exist: {}", path.display());
    }
    Ok(path.canonicalize()?)
}

fn ato_home() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("ATO_HOME") {
        return Ok(PathBuf::from(value));
    }
    Ok(dirs::home_dir()
        .context("home directory is unavailable")?
        .join(".ato"))
}

fn parse_binding(value: &str) -> Result<(String, String), String> {
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| "expected BINDING_ID=VALUE".to_owned())?;
    if name.is_empty() || value.is_empty() {
        return Err("binding id and value must be non-empty".to_owned());
    }
    Ok((name.to_owned(), value.to_owned()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.new",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("capsule"),
        std::process::id()
    ));
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use ato_computation::{
        ComputationObject, NodeId, PortDef, PortId, ProtocolId, RoleId, SemanticsId,
        computation_ref, encode_computation_object,
    };
    use ato_objects::{MemoryObjectStore, ObjectStore};

    use super::*;

    fn store_computation(
        objects: &MemoryObjectStore,
        computation: &ComputationObject,
    ) -> ComputationRef {
        let reference = computation_ref(computation).unwrap();
        objects
            .insert(
                reference.content_ref(),
                &encode_computation_object(computation).unwrap(),
            )
            .unwrap();
        reference
    }

    fn leaf(objects: &MemoryObjectStore) -> ComputationRef {
        let residual = objects.put(b"leaf-state").unwrap();
        store_computation(
            objects,
            &ComputationObject {
                semantics: SemanticsId::parse("example.leaf@1").unwrap(),
                boundary: BTreeMap::from([(
                    PortId::parse("surface").unwrap(),
                    PortDef {
                        protocol: ProtocolId::parse("ato.http@1").unwrap(),
                        role: RoleId::parse("server").unwrap(),
                    },
                )]),
                residual,
            },
        )
    }

    fn composite(
        objects: &MemoryObjectStore,
        nodes: BTreeMap<NodeId, ComputationRef>,
    ) -> ComputationRef {
        let bytes = ato_compose::encode_composite_residual(&ato_compose::CompositeResidual {
            nodes,
            connections: Vec::new(),
            exports: BTreeMap::new(),
        })
        .unwrap();
        let residual = objects.put(&bytes).unwrap();
        store_computation(
            objects,
            &ComputationObject {
                semantics: SemanticsId::parse(COMPOSE_SEMANTICS_ID).unwrap(),
                boundary: BTreeMap::new(),
                residual,
            },
        )
    }

    #[test]
    fn structure_projection_is_deterministic_and_contains_only_interfaces() {
        let objects = MemoryObjectStore::default();
        let child = leaf(&objects);
        let root = composite(
            &objects,
            BTreeMap::from([
                (NodeId::parse("zeta").unwrap(), child.clone()),
                (NodeId::parse("alpha").unwrap(), child),
            ]),
        );
        let first = build_structure_projection(&objects, &root).unwrap();
        let second = build_structure_projection(&objects, &root).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first
                .children
                .iter()
                .map(|node| node.label.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert_eq!(first.children[0].exported_ports[0].protocol, "ato.http@1");
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains("leaf-state"));
    }

    #[test]
    fn structure_projection_rejects_more_than_the_product_node_budget() {
        let objects = MemoryObjectStore::default();
        let child = leaf(&objects);
        let group_nodes = (0..MAX_STRUCTURE_CHILDREN)
            .map(|index| {
                (
                    NodeId::parse(format!("node{index:03}")).unwrap(),
                    child.clone(),
                )
            })
            .collect();
        let group = composite(&objects, group_nodes);
        let root = composite(
            &objects,
            (0..4)
                .map(|index| {
                    (
                        NodeId::parse(format!("group{index}")).unwrap(),
                        group.clone(),
                    )
                })
                .collect(),
        );
        assert!(
            build_structure_projection(&objects, &root)
                .unwrap_err()
                .to_string()
                .contains("maximum node count")
        );
    }

    #[test]
    fn portable_compose_uses_canonical_compose_semantics_without_mutating_inputs() {
        let objects = MemoryObjectStore::default();
        let child = composite(&objects, BTreeMap::new());
        let references = reference_registry().unwrap();
        let input = encode_bundle(&export_bundle(&child, &objects, &references).unwrap()).unwrap();
        let input_before = input.clone();
        let (root, bytes) = compose_portable_bundles(&[input.clone(), input.clone()]).unwrap();
        let verified = verify_bundle(&decode_bundle(&bytes).unwrap(), &references).unwrap();
        assert_eq!(verified.root().to_string(), root);
        assert_eq!(
            resolve_computation(verified.objects(), verified.root())
                .unwrap()
                .object()
                .semantics
                .as_str(),
            COMPOSE_SEMANTICS_ID
        );
        let structure = build_structure_projection(verified.objects(), verified.root()).unwrap();
        assert_eq!(structure.children.len(), 2);
        assert_eq!(structure.children[0].label.as_deref(), Some("capsule01"));
        assert_eq!(structure.children[1].label.as_deref(), Some("capsule02"));
        assert_eq!(input, input_before, "input bytes remain immutable");
    }

    #[test]
    fn public_cli_has_only_the_capsule_lifecycle() {
        for command in ["init", "resume", "stop", "encap", "run"] {
            assert!(Cli::try_parse_from(["ato", command, "value"]).is_ok());
        }
        for removed in ["lock", "decap", "snapshot"] {
            assert!(Cli::try_parse_from(["ato", removed]).is_err());
        }
    }

    #[test]
    fn run_rejects_repository_shaped_inputs_before_execution() {
        let args = RunArgs {
            capsule: PathBuf::from("."),
            bindings: Vec::new(),
        };
        assert!(
            run_capsule(args)
                .unwrap_err()
                .to_string()
                .contains("portable .capsule")
        );
    }

    #[test]
    fn hosted_execution_fails_closed_without_an_external_sandbox_profile() {
        assert!(!external_sandbox_profile_supported(""));
        assert!(!external_sandbox_profile_supported("host"));
        assert!(!external_sandbox_profile_supported("linux-bwrap"));
        assert!(!external_sandbox_profile_supported("firecracker"));
        assert!(external_sandbox_profile_supported("untrusted-v1"));
    }

    #[test]
    fn encap_current_is_part_of_encap_not_a_capture_command() {
        let cli = Cli::try_parse_from([
            "ato",
            "encap",
            "demo@main",
            "--current",
            "-o",
            "state.capsule",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Commands::Encap(EncapArgs { current: true, .. })
        ));
        assert!(Cli::try_parse_from(["ato", "capture", "demo@main"]).is_err());
    }

    #[test]
    fn share_is_a_product_command_using_an_existing_device_credential() {
        let cli = Cli::try_parse_from([
            "ato",
            "share",
            "demo@main",
            "--device-token",
            "ato_dev_test",
            "--yes",
        ])
        .unwrap();
        let Commands::Share(args) = cli.command else {
            panic!("share command was not parsed");
        };
        assert_eq!(args.selector, "demo@main");
        assert_eq!(args.device_token, "ato_dev_test");
        assert!(args.yes);
    }

    #[test]
    fn snapshot_id_uses_materializer_vocabulary() {
        assert_eq!(
            ato_materializer_snapshot::SNAPSHOT_MATERIALIZER_ID,
            "ato.snapshot@1"
        );
    }
}
