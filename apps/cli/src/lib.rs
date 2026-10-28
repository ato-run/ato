//! Product assembly for the Capsule lifecycle.

#![deny(unsafe_op_in_unsafe_fn)]

mod desktop_control;
mod formation_exploration;
mod formation_input;
mod formation_session;
mod formation_verify;
mod mcp_stdio;
mod model_cache;
mod object_transport;
mod portable_dependency;

pub mod activity_client;
pub mod activity_mcp;
pub mod auth_device;
pub mod formation_session_mcp;

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ato_adapter_api::AdapterContext;
#[cfg(test)]
use ato_adapter_browser::{
    BROWSER_CLICK_OPERATION, BROWSER_KEYBOARD_OPERATION, BROWSER_PROTOCOL_ID,
};
use ato_adapter_process::terminate_process_tree;
use ato_adapter_workspace::restore_workspace;
use ato_computation::{ComputationRef, ContentRef};
use ato_formation::authoring::STATE_FILESYSTEM_PROTOCOL;
use ato_formation::verify::{ContractVerificationReceipt, VerificationTargetKind};
use ato_materializer_api::{
    ContractContext, MaterializerContext, MaterializerRegistry, accept_candidate,
};
use ato_materializer_vm_snapshot::{
    FirecrackerBackend, FirecrackerBackendConfig, FirecrackerRecordCaptureBarrier,
    FirecrackerRecordCaptureLease, SealedRecordFrontierVerifier, VmSnapshotError,
    VmSnapshotMaterializer,
};
use ato_objects::{
    BranchOrigin, BundleMaterialization, CapsuleBundle, CapsuleBundleDocument, CapsuleSelector,
    GraphMaterialization, GraphRestoreCapability, LocalCapsuleRepository,
    PortableApplicationBundle, PortableDependencyProfile, PortableOciArchive, RecordId,
    ReferenceRegistry, decode_capsule_bundle_document, encode_bundle,
    export_bundle_with_materializations, export_object_graph, import_bundle, resolve_computation,
};
use ato_portable_application::local_instance::{
    LocalApplicationStore, LocalInstanceRun, LocalInstanceRunStatus, LocalRunActivation,
};
use ato_portable_application::portability_export::repack_portable_dependencies_with_archives;
use ato_portable_application::portability_plan::{PortableExportProfile, plan_portable_export};
use ato_portable_application::{
    PortableRealizationKind, StaticApplicationAsset, StaticApplicationState,
    ValidatedPortableApplication, build_authored_bundle_v2, bundle_sha256,
    resolve_application_bindings, validate_bundle_for_derivation,
};
use ato_realization_planner::{
    MaterializationCandidate, Placement, PlannerPolicy, RealizationPlanner, TargetEnvironment,
    TrustBoundary,
};
use ato_record_writer::{
    CaptureBarrier, PausedCapture, load_frontier, records_for_frontier, verify_frontier_object,
};
use ato_runtime_object_graph::standard_reference_registry;
use ato_sandbox::{SandboxPolicy, apply_sandbox};
use clap::{Args, Parser, Subcommand};

pub use crate::object_transport::{
    ExportedPort, HttpObjectTransportApi, ObjectGraphIndexV1, ObjectUploadReceipt, RequiredBinding,
    UploadConfig, VisibilityPolicy, upload_http_object_graph,
    upload_staging_negative_test_object_graph, vm_capture_receipt_refs,
};
use ato_local_execution::authoring::{
    initial_computation, load_config, load_runtime_state, workspace_policy,
};
use ato_local_execution::registry::{adapter_registry, contract_verifier_registry};
use ato_local_execution::supervisor::{
    LocalRealizationDriver, preflight_actuator_provider_registry, start_durable,
};
use ato_local_execution::{
    OwnedProcessIdentity, configure_detached_process, terminate_owned_process,
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
    /// Compile an ato.capsule/2 source directory into a portable Application.
    Pack(PackArgs),
    /// Experimental: compile an ato.capsule/1 route that declares a Terminal
    /// native Surface into a portable wire v3 `.capsule` bundle.
    #[command(name = "__native-bundle", hide = true)]
    NativeBundle(NativeBundleArgs),
    /// Experimental: compile an ato.capsule/1 route that declares a browser
    /// (Static Web) Surface into a portable wire v3 `.capsule` bundle from the
    /// existing source tree, without running a build.
    #[command(name = "__static-bundle", hide = true)]
    StaticBundle(StaticBundleArgs),
    /// Consume a portable .capsule ephemerally.
    Run(RunArgs),
    /// Derive and verify a candidate Derivation against an existing .capsule's
    /// frozen Contract K. Writes local candidate artifacts; never publishes.
    Reform(ReformArgs),
    /// Import immutable model objects into this host's verified cache.
    ModelSet {
        #[command(subcommand)]
        command: model_cache::ModelSetCommands,
    },
    /// Import and operate a durable local portable Application Instance.
    App {
        #[command(subcommand)]
        command: AppCommands,
    },
    /// Preview dependency size and guarantees before a portable export.
    ExportPlan(ExportPlanArgs),
    /// Repack an existing portable application without changing K or D.
    Export(ExportArgs),
    /// Authenticate this CLI with an Ato API using the browser device flow.
    Login(LoginArgs),
    /// Upload a content-addressed Capsule object graph.
    Upload(UploadArgs),
    /// Form a local directory into a Capsule: build each candidate, observe it
    /// satisfying its Contract, and keep the verified artifact.
    Form(Box<FormArgs>),
    /// Inspect repository execution evidence without building or launching it.
    FormInspect {
        path: PathBuf,
        /// Optional canonical D JSON, inspected without changing or executing it.
        #[arg(long, requires = "contract")]
        derivation: Option<PathBuf>,
        /// The unchanged K whose observations the selected D must satisfy.
        #[arg(long, requires = "derivation")]
        contract: Option<PathBuf>,
    },
    /// Inspect or supply scoped input to an existing Formation Search.
    FormInput(formation_input::InputArgs),
    /// Produce bounded Formation proposals through a scoped Session bridge.
    FormSession(formation_session::SessionArgs),
    /// Explicitly authorize a retained Source result for functional verification.
    FormVerify(formation_verify::VerifyArgs),
    /// Take part in the Runtime Network: advertise this host's execution
    /// environments and run the Formation attempts addressed to it.
    #[command(subcommand)]
    RuntimeNetwork(RuntimeNetworkCommand),
    /// Report this binary's build identity (version, commit, profile).
    ///
    /// `--version` stays exactly as it was — a human-readable release-line
    /// string. This is the machine-readable form, and the only way to learn
    /// which commit a given `ato` artifact was built from.
    Version {
        /// Emit a single JSON object instead of human-readable lines.
        #[arg(long)]
        json: bool,
    },
    #[command(name = "__worker", hide = true)]
    Worker {
        project: PathBuf,
        branch: String,
        head: String,
        token: String,
        descriptor: Option<String>,
    },
    #[command(name = "__desktop", hide = true)]
    Desktop {
        #[command(subcommand)]
        command: DesktopCommands,
    },
    #[command(name = "__portable-sandbox-exec", hide = true)]
    PortableSandboxExec(PortableSandboxExecArgs),
    #[command(name = "__portable-instance-worker", hide = true)]
    PortableInstanceWorker(PortableInstanceWorkerArgs),
    /// 6b-D2 experimental: build a frozen source's root Dockerfile with a
    /// private builder into a verified OCI archive and an authored OCI route.
    #[command(name = "__source-oci-build", hide = true)]
    SourceOciBuild(SourceOciBuildArgs),
    /// 6b-D2 experimental: freeze one docker.io base image for one platform
    /// through an egress gate limited to the registry, auth and blob hosts.
    #[command(name = "__source-oci-acquire-base", hide = true)]
    SourceOciAcquireBase(SourceOciAcquireBaseArgs),
}

#[derive(Debug, Args)]
struct SourceOciAcquireBaseArgs {
    /// `docker.io/<namespace>/<name>:<tag>` or `<name>:<tag>`.
    #[arg(long)]
    reference: String,
    #[arg(long)]
    platform: String,
    /// Content bound for the whole acquisition (at most 200 MiB).
    #[arg(long)]
    max_bytes: u64,
    /// New archive path.
    #[arg(long)]
    out: PathBuf,
    /// Where to write the acquisition provenance JSON.
    #[arg(long)]
    provenance: PathBuf,
}

#[derive(Debug, Args)]
struct SourceOciBuildArgs {
    /// `ato.source-oci-request/1` JSON.
    #[arg(long)]
    request: PathBuf,
    /// New, short absolute directory for this job's private builder session
    /// (daemon, cgroup, network namespace, bounded filesystem). No socket is
    /// ever accepted from the caller.
    #[arg(long)]
    work_root: PathBuf,
    /// New output directory.
    #[arg(long)]
    out: PathBuf,
}

#[derive(Subcommand)]
enum AppCommands {
    /// Import an immutable portable bundle as a new independent Instance.
    Import(AppImportArgs),
    /// Start a new durable Run for an imported Instance.
    Start(AppStartArgs),
    /// Stop the active Run without deleting the Instance.
    Stop { instance: String },
    /// Print Instance metadata and its active Run, if any.
    Inspect { instance: String },
    /// Export the immutable Application and this Instance's saved snapshot.
    Export(AppExportArgs),
}

#[derive(Subcommand)]
enum DesktopCommands {
    /// Inspect the active Run of a Capsule project as a single JSON object.
    Inspect { project: String },
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
    #[arg(long = "materialize")]
    materializers: Vec<String>,
    #[arg(short, long, default_value = "computation.capsule")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct PackArgs {
    #[arg(default_value = ".")]
    source: PathBuf,
    #[arg(short, long, default_value = "application.capsule")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct RunArgs {
    capsule: PathBuf,
    /// Select one declared DerivationRef. Required when the bundle has more than one route.
    #[arg(long)]
    derivation: Option<String>,
    #[arg(long = "bind", value_parser = parse_binding)]
    bindings: Vec<(String, String)>,
    /// Verify the local realization without opening a browser, then exit.
    #[arg(long)]
    no_open: bool,
    /// Write the shared Contract verification receipt as canonical JSON.
    #[arg(long)]
    verification_receipt: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ReformArgs {
    /// Existing portable .capsule whose canonical Contract K is frozen.
    capsule: PathBuf,
    /// New source directory to build the candidate Derivation from.
    source: PathBuf,
    /// New directory for candidate.capsule, receipt.json and report.json.
    /// Refused when it already exists; nothing outside it is written.
    #[arg(long)]
    out: PathBuf,
    /// Select one declared candidate DerivationRef when the new source yields
    /// several. Without it, one derivation is used and several are an error.
    #[arg(long)]
    derivation: Option<String>,
}

#[derive(Debug, Args)]
struct AppImportArgs {
    capsule: PathBuf,
    /// Select one declared DerivationRef. Required when the bundle has more than one route.
    #[arg(long)]
    derivation: Option<String>,
}

#[derive(Debug, Args)]
struct AppStartArgs {
    instance: String,
    /// Rebind one declared runtime connection for this Run. Values are not persisted.
    #[arg(long = "bind", value_parser = parse_binding)]
    bindings: Vec<(String, String)>,
    /// Leave the durable Run active without opening its Surface.
    #[arg(long)]
    no_open: bool,
    /// Copy the Run-scoped canonical verification receipt to this path.
    #[arg(long)]
    verification_receipt: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct AppExportArgs {
    instance: String,
    #[arg(short, long, default_value = "application.capsule")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct NativeBundleArgs {
    /// The source directory whose tree the bundle carries.
    #[arg(default_value = ".")]
    source: PathBuf,
    /// The `ato.capsule/1` manifest. Defaults to `<source>/capsule.toml`.
    #[arg(long)]
    manifest: Option<PathBuf>,
    /// Application title, part of the bundled Application object.
    #[arg(long)]
    title: String,
    /// New bundle file.
    #[arg(short, long, default_value = "native.capsule")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct StaticBundleArgs {
    /// The source directory whose tree the bundle carries.
    #[arg(default_value = ".")]
    source: PathBuf,
    /// Application title, part of the bundled Application object.
    #[arg(long)]
    title: String,
    /// New bundle file.
    #[arg(short, long, default_value = "static.capsule")]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct PortableInstanceWorkerArgs {
    instance: String,
    run_id: String,
    token: String,
}

#[derive(Debug, Args)]
struct ExportPlanArgs {
    capsule: PathBuf,
    #[arg(long, value_enum, default_value_t = PortableExportProfile::Cached)]
    portability: PortableExportProfile,
    #[arg(long)]
    json: bool,
    /// Verified Docker 29 OCI-layout archive for an offline image.
    #[arg(long)]
    oci_archive: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ExportArgs {
    capsule: PathBuf,
    #[arg(long, value_enum)]
    portability: PortableExportProfile,
    #[arg(short, long)]
    output: PathBuf,
    /// Verified Docker 29 OCI-layout archive for an offline image.
    #[arg(long)]
    oci_archive: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct PortableSandboxExecArgs {
    #[arg(long)]
    policy: PathBuf,
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct UploadArgs {
    selector: String,
    #[arg(long = "materialize")]
    materializers: Vec<String>,
    #[arg(long, env = "ATO_API_URL")]
    api_url: String,
    /// Bearer token; defaults to ATO_API_TOKEN, then a credential saved by `ato login`.
    #[arg(long, env = "ATO_API_TOKEN", hide_env_values = true)]
    auth_token: Option<String>,
    #[arg(long, value_enum, default_value = "private")]
    visibility: VisibilityPolicy,
    #[arg(long)]
    idempotency_key: Option<String>,
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=32))]
    concurrency: u8,
    #[arg(long, default_value_t = 4)]
    retry_attempts: usize,
    #[arg(long, default_value_t = 120)]
    validation_poll_attempts: usize,
    #[arg(long, default_value_t = 1_000)]
    validation_poll_ms: u64,
    #[arg(long, default_value = "object-upload-receipt.json")]
    receipt: PathBuf,
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// Ato API origin to authenticate against.
    #[arg(long, env = "ATO_API_URL", default_value = auth_device::DEFAULT_API_URL)]
    api_url: String,
    /// Print the approval URL and wait instead of opening a browser.
    #[arg(long)]
    headless: bool,
}

/// The request IS the whole statement: which directory, which Runtime
/// ('local' only in Phase 1), whether build steps may reach the network, and
/// how many candidates to try. A 'capsule.toml' inside the directory is
/// honored strictly; without one, every preset the source honestly fits is
/// tried in turn.
#[derive(Debug, Args)]
struct FormArgs {
    /// The directory to form.
    path: PathBuf,
    /// The Runtime to form on. Phase 1 admits exactly 'local'.
    #[arg(long, default_value = "local")]
    runtime: String,
    /// Whether build steps may reach the network.
    #[arg(long, default_value = "denied", value_parser = ["denied", "dependency-resolution"])]
    network: String,
    /// Candidate Derivations to try before giving up.
    #[arg(long, default_value_t = 8)]
    max_attempts: usize,
    /// Where verified artifacts are written, content-addressed.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Scratch space for attempts: staged workspaces, build caches.
    #[arg(long)]
    work_root: Option<PathBuf>,
    /// Also verify the running candidate in a browser against the
    /// '--accept' prompt. Only a browser PASS forms the candidate.
    #[arg(long, requires = "accept")]
    verify_browser: bool,
    /// The acceptance prompt, in plain language. Kept verbatim in the
    /// browser Contract.
    #[arg(long, requires = "verify_browser")]
    accept: Option<String>,
    /// The browser verifier helper (apps/formation-browser-verifier). Without
    /// one, a browser verification is recorded as unavailable — never passed.
    #[arg(long, env = "ATO_BROWSER_VERIFIER")]
    browser_verifier: Option<PathBuf>,
    /// The Node binary that runs the browser verifier.
    #[arg(long, env = "ATO_NODE", default_value = "node")]
    node: String,
    /// The Chrome/Chromium binary the verifier drives.
    #[arg(long, env = "ATO_BROWSER_CHROME_PATH")]
    browser_chrome: Option<PathBuf>,
    /// Development only: run the browser verifier and its browser directly on
    /// this host, with the host filesystem visible to them, when the verifier
    /// sandbox (Linux + bubblewrap) is not available. Recorded as
    /// `containment: none` in the receipt; never available on a Runtime
    /// Network Runtime.
    #[arg(long, requires = "verify_browser")]
    allow_uncontained_browser_verifier: bool,
    /// Satisfy the Contract on the Runtime Network instead of this machine:
    /// every authorized route × every available owned Runtime, hard-filtered,
    /// executed and verified. `--runtime local` is unaffected.
    #[arg(long)]
    runtime_network: bool,
    /// The coordinator (ato-api) base URL.
    #[arg(long, env = "ATO_RUNTIME_NETWORK_API")]
    api: Option<String>,
    /// A file holding the bearer token that acts for the requesting account.
    #[arg(long, env = "ATO_RUNTIME_NETWORK_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    /// An authorized route (capsule.toml). Repeatable; default: the
    /// directory's own capsule.toml. Every route must bind to the same K.
    #[arg(long = "route")]
    routes: Vec<PathBuf>,
    /// Run only on this Runtime (runner id). No fallback elsewhere.
    #[arg(long)]
    exact_runtime: Option<String>,
    /// `first_pass` stops at the first verified route; `all` attempts every
    /// admissible candidate.
    #[arg(long, default_value = "first_pass", value_parser = ["first_pass", "all"])]
    mode: String,
    /// May control-plane-managed Runtimes be used?
    #[arg(long)]
    allow_managed: bool,
    /// Continue this Runtime Network search instead of starting a new one.
    /// The search keeps the budget its first request stated: every budget
    /// option, `--max-attempts` and `--mode` included, must state it exactly,
    /// and what earlier requests spent stays spent.
    #[arg(long)]
    search_id: Option<String>,
    /// The search's lifetime from its first request, in seconds (at most
    /// 7 days). Never extended.
    #[arg(long, default_value_t = 30 * 60)]
    deadline_seconds: u64,
    /// Logical source bytes the search may hand to Runtimes, over all of its
    /// attempts (at most 10 GiB).
    #[arg(long, default_value_t = ato_formation_worker::runtime_network::MAX_SEARCH_TRANSFER_BYTES)]
    max_transfer_bytes: u64,
    /// Logical bytes the search may expand on Runtimes (at most 10 GiB).
    #[arg(long, default_value_t = ato_formation_worker::runtime_network::MAX_SEARCH_EXPANDED_BYTES)]
    max_expanded_bytes: u64,
    /// Logical artifact bytes the search may have Runtimes keep (at most
    /// 10 GiB).
    #[arg(long, default_value_t = ato_formation_worker::runtime_network::MAX_SEARCH_STORED_BYTES)]
    max_stored_bytes: u64,
    /// Let a decision provider pick the next attempt among those the search
    /// offers (Stage 5a). `jev` reads ATO_DECISION_JEV_API_KEY (never the
    /// browser judge's key). Without a usable answer the search takes its
    /// deterministic default. Frozen with the search.
    #[arg(long, value_parser = ["jev"], requires = "runtime_network")]
    decision_provider: Option<String>,
    /// Decision points the provider may answer over the whole search (1–64).
    #[arg(long, default_value_t = 8, requires = "decision_provider")]
    max_decisions: u32,
    /// How long a decision point waits for the provider, in milliseconds
    /// (1000–120000).
    #[arg(long, default_value_t = 30_000, requires = "decision_provider")]
    decision_timeout_ms: u64,
    /// Generate one bounded typed Python draft after known D exhaustion.
    /// Uses ATO_GENERATION_JEV_API_KEY, separately from decision/browser keys.
    #[arg(long, value_parser = ["jev"], requires_all = ["runtime_network", "generation_entrypoints"])]
    generation_provider: Option<String>,
    /// Explicit source entrypoint authorization: opaque-id=relative-file.py.
    /// Repeat for up to 16 files. Model sees IDs, never the paths or source.
    #[arg(long = "generation-entrypoint", requires = "generation_provider")]
    generation_entrypoints: Vec<String>,
    /// External exploration ceiling, frozen K, source authorization and
    /// pinned provider/spend config (JSON or TOML). No normal Run approval.
    #[arg(long, requires = "runtime_network", conflicts_with_all = ["generation_provider", "verify_browser"])]
    exploration_config: Option<PathBuf>,
    /// Publish a scoped connection for a logged-in CandidateProducer agent.
    /// Only the trusted requester may access its own token and private journal.
    #[arg(long, requires = "exploration_config")]
    session_bridge: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum RuntimeNetworkCommand {
    /// Serve Formation attempts on this host until stopped.
    Serve(RuntimeNetworkServeArgs),
}

#[derive(Debug, Args)]
struct RuntimeNetworkServeArgs {
    /// The coordinator (ato-api) base URL.
    #[arg(long, env = "ATO_RUNTIME_NETWORK_API")]
    api: String,
    /// A file holding this Runtime's runner token.
    #[arg(long, env = "ATO_RUNTIME_NETWORK_TOKEN_FILE")]
    token_file: PathBuf,
    /// Scratch for attempts.
    #[arg(long)]
    work_root: Option<PathBuf>,
    /// Where verified artifacts are written.
    #[arg(long)]
    out: Option<PathBuf>,
    /// The browser verifier helper (apps/formation-browser-verifier). It is
    /// advertised only when it runs contained (Linux + bubblewrap).
    #[arg(long, env = "ATO_BROWSER_VERIFIER")]
    browser_verifier: Option<PathBuf>,
    /// The Node binary that runs the browser verifier.
    #[arg(long, env = "ATO_NODE", default_value = "node")]
    node: String,
    /// The Chrome/Chromium binary the verifier drives.
    #[arg(long, env = "ATO_BROWSER_CHROME_PATH")]
    browser_chrome: Option<PathBuf>,
    /// Stop after handling this many attempts.
    #[arg(long)]
    max_attempts: Option<u32>,
    /// External exploration sandbox ceiling as JSON; no production bindings.
    #[arg(long)]
    exploration_sandbox: Option<PathBuf>,
}

/// The build's own identity: what this binary is, not merely which release
/// line it belongs to.
///
/// `--version` reports the crate version, which every build of a release line
/// shares. Desktop assembly pins its Ato support crates and its bundled Runner
/// to one Ato revision and must be able to VERIFY that from the artifact, so
/// the commit is baked in at build time (see build.rs) and reported here.
#[derive(serde::Serialize)]
pub struct BuildIdentity {
    /// Crate version — the same string `--version` prints.
    pub version: &'static str,
    /// Full git commit the binary was built from, or `"unknown"`.
    pub git_commit: &'static str,
    /// Whether the working tree was dirty. `"true"` / `"false"` / `"unknown"`;
    /// tri-state on purpose, since "could not tell" is not "clean".
    pub git_dirty: &'static str,
    /// Cargo profile, so a release claim is checkable.
    pub profile: &'static str,
}

impl BuildIdentity {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            git_commit: env!("ATO_BUILD_GIT_SHA"),
            git_dirty: env!("ATO_BUILD_GIT_DIRTY"),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
        }
    }
}

fn version(json: bool) -> Result<()> {
    let identity = BuildIdentity::current();
    if json {
        println!("{}", serde_json::to_string(&identity)?);
    } else {
        println!("ato {}", identity.version);
        println!("commit: {}", identity.git_commit);
        println!("dirty: {}", identity.git_dirty);
        println!("profile: {}", identity.profile);
    }
    Ok(())
}

pub fn run() -> Result<()> {
    // Re-entry from INSIDE the build sandbox (Linux): bwrap execs this same
    // binary with sandbox-exec as argv[1]. Not a user-facing subcommand, so
    // it is handled before clap ever sees argv.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).is_some_and(|arg| arg == "sandbox-exec") {
        return ato_formation_worker::sandbox_exec::sandbox_exec(&argv[2..]);
    }
    match Cli::parse().command {
        Commands::Version { json } => version(json),
        Commands::Init(args) => init(args),
        Commands::Resume(args) => resume(args),
        Commands::Stop { capsule } => stop(&capsule),
        Commands::Encap(args) => encap(args),
        Commands::Pack(args) => pack(args),
        Commands::NativeBundle(args) => native_bundle(args),
        Commands::StaticBundle(args) => static_bundle(args),
        Commands::Run(args) => run_capsule(args),
        Commands::Reform(args) => run_reform(args),
        Commands::ModelSet { command } => model_cache::execute(command),
        Commands::App { command } => match command {
            AppCommands::Import(args) => import_local_application(args),
            AppCommands::Start(args) => start_local_instance(args),
            AppCommands::Stop { instance } => stop_local_instance(&instance),
            AppCommands::Inspect { instance } => inspect_local_instance(&instance),
            AppCommands::Export(args) => export_local_instance(args),
        },
        Commands::ExportPlan(args) => export_plan(args),
        Commands::Export(args) => export_portable(args),
        Commands::Login(args) => login(args),
        Commands::Upload(args) => upload(args),
        Commands::Form(args) if args.runtime_network => form_on_runtime_network(*args),
        Commands::Form(args) => form(*args),
        Commands::FormInspect {
            path,
            derivation,
            contract,
        } => {
            let evidence = ato_formation::repository_evidence::inspect(&path)?;
            let declared = match (derivation, contract) {
                (Some(d), Some(k)) => {
                    let d: ato_formation::authoring::BoundDerivation = read_inspection_json(&d)?;
                    let k: ato_formation::authoring::BoundContract = read_inspection_json(&k)?;
                    Some(serde_json::to_value(
                        ato_formation::requirements::inspection::inspect(&d, &k),
                    )?)
                }
                (None, None) => None,
                _ => bail!("--derivation and --contract must be supplied together"),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "repository": evidence, "declared_execution": declared,
                }))?
            );
            Ok(())
        }
        Commands::FormInput(args) => formation_input::run(args),
        Commands::FormSession(args) => formation_session::run(args),
        Commands::FormVerify(args) => formation_verify::run(args),
        Commands::RuntimeNetwork(RuntimeNetworkCommand::Serve(args)) => runtime_network_serve(args),
        Commands::Worker {
            project,
            branch,
            head,
            token,
            descriptor,
        } => ato_local_execution::supervisor::worker(
            &project,
            &branch,
            &ComputationRef::parse(head)?,
            &token,
            descriptor.map(ContentRef::parse).transpose()?.as_ref(),
            &cli_materializers,
        ),
        Commands::Desktop { command } => match command {
            DesktopCommands::Inspect { project } => desktop_inspect(&project),
        },
        Commands::PortableSandboxExec(args) => portable_sandbox_exec(args),
        Commands::PortableInstanceWorker(args) => portable_instance_worker(args),
        Commands::SourceOciBuild(args) => source_oci_build(args),
        Commands::SourceOciAcquireBase(args) => source_oci_acquire_base(args),
    }
}

fn source_oci_build(args: SourceOciBuildArgs) -> Result<()> {
    use ato_portable_application::source_oci::{PROVENANCE_FILE, SourceOciRequest, prepare};
    let request: SourceOciRequest = serde_json::from_slice(
        &fs::read(&args.request).with_context(|| format!("read {}", args.request.display()))?,
    )
    .context("source-oci request is malformed")?;
    // Every input is verified before a builder session exists.
    let prepared = prepare(&request)?;
    let materialized = source_oci_session_build(&prepared, &args)?;
    let provenance = serde_json::to_vec_pretty(&materialized.provenance)?;
    fs::write(args.out.join(PROVENANCE_FILE), &provenance)?;
    let authored = args.out.join("authored");
    fs::create_dir(&authored)?;
    fs::write(authored.join("capsule.toml"), &materialized.capsule_toml)?;
    fs::write(authored.join(PROVENANCE_FILE), &provenance)?;
    println!("image_reference={}", materialized.image_reference);
    println!("archive={}", materialized.archive.display());
    println!("authored={}", authored.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn source_oci_session_build(
    prepared: &ato_portable_application::source_oci::Prepared,
    args: &SourceOciBuildArgs,
) -> Result<ato_portable_application::source_oci::Materialized> {
    use ato_portable_application::source_oci::materialize;
    use ato_portable_application::source_oci::session::{
        DockerCliBuilder, PrivateDockerSession, SessionTools,
    };
    let egress = prepared.request().policy.egress.as_ref().map(|e| {
        ato_portable_application::source_oci::session::SessionEgress {
            hosts: e.hosts.clone(),
            ports: e.ports.clone(),
            max_transfer_bytes: e.max_transfer_bytes,
        }
    });
    let session = PrivateDockerSession::start(
        &args.work_root,
        &prepared.request().policy.build,
        SessionTools::default(),
        egress,
    )?;
    Ok(materialize(
        prepared,
        &DockerCliBuilder::new(session),
        &args.out,
    )?)
}

#[cfg(not(target_os = "linux"))]
fn source_oci_session_build(
    _: &ato_portable_application::source_oci::Prepared,
    _: &SourceOciBuildArgs,
) -> Result<ato_portable_application::source_oci::Materialized> {
    bail!("source_oci_session_refused: private builder sessions require Linux")
}

fn source_oci_acquire_base(args: SourceOciAcquireBaseArgs) -> Result<()> {
    use ato_portable_application::source_oci::acquire::acquire_base;
    if args.out.exists() || args.provenance.exists() {
        bail!("source_oci_output_invalid: --out and --provenance must not exist");
    }
    let scratch = tempfile::tempdir().context("acquisition scratch")?;
    let acquired = acquire_base(
        &args.reference,
        &args.platform,
        args.max_bytes,
        &args.out,
        scratch.path(),
    )?;
    fs::write(&args.provenance, serde_json::to_vec_pretty(&acquired)?)?;
    println!("root_digest={}", acquired.root_digest);
    println!(
        "platform_manifest_digest={}",
        acquired.platform_manifest_digest
    );
    println!("config_digest={}", acquired.config_digest);
    println!("archive_sha256={}", acquired.archive_sha256);
    println!("transferred_bytes={}", acquired.egress.transferred_bytes);
    Ok(())
}

fn pack(args: PackArgs) -> Result<()> {
    let (bytes, bundle) = build_authored_bundle_v2(&args.source).with_context(|| {
        format!(
            "compile portable Application from {}",
            args.source.display()
        )
    })?;
    fs::write(&args.output, &bytes)
        .with_context(|| format!("write portable Application {}", args.output.display()))?;
    println!("Packed: {}", args.output.display());
    println!("bundle_sha256={}", bundle_sha256(&bytes));
    println!("contract_ref={}", bundle.index.root_contract_ref);
    for derivation in &bundle.index.derivations {
        println!("derivation_ref={derivation}");
    }
    Ok(())
}

fn native_bundle(args: NativeBundleArgs) -> Result<()> {
    let manifest = args.manifest.unwrap_or_else(|| {
        args.source
            .join(ato_formation::capsule_toml::CAPSULE_FILE_NAME)
    });
    let (bytes, bundle) = ato_portable_application::build_native_terminal_bundle(
        &args.source,
        &manifest,
        &args.title,
    )
    .with_context(|| {
        format!(
            "compile native bundle from {} and {}",
            manifest.display(),
            args.source.display()
        )
    })?;
    fs::write(&args.output, &bytes)
        .with_context(|| format!("write native bundle {}", args.output.display()))?;
    println!("Packed: {}", args.output.display());
    println!("bundle_sha256={}", bundle_sha256(&bytes));
    println!("contract_ref={}", bundle.index.root_contract_ref);
    for derivation in &bundle.index.derivations {
        println!("derivation_ref={derivation}");
    }
    Ok(())
}

/// Compile an `ato.capsule/1` static Web route into a portable wire v3 bundle
/// with no build: the packer hashes the tree this route already declares.
fn static_bundle(args: StaticBundleArgs) -> Result<()> {
    let (bytes, bundle) = ato_portable_application::build_static_bundle(&args.source, &args.title)
        .with_context(|| format!("compile static bundle from {}", args.source.display()))?;
    fs::write(&args.output, &bytes)
        .with_context(|| format!("write static bundle {}", args.output.display()))?;
    println!("Packed: {}", args.output.display());
    println!("bundle_sha256={}", bundle_sha256(&bytes));
    println!("contract_ref={}", bundle.index.root_contract_ref);
    for derivation in &bundle.index.derivations {
        println!("derivation_ref={derivation}");
    }
    Ok(())
}

fn portable_sandbox_exec(args: PortableSandboxExecArgs) -> Result<()> {
    let bytes = fs::read(&args.policy)
        .with_context(|| format!("read portable sandbox policy {}", args.policy.display()))?;
    let policy: SandboxPolicy =
        serde_json::from_slice(&bytes).context("portable sandbox policy is malformed")?;
    let applied = apply_sandbox(&policy).context("apply portable process sandbox")?;
    if !applied.fully_enforced {
        bail!(
            "portable process sandbox admission failed: {}",
            applied.message
        );
    }
    let (program, arguments) = args
        .command
        .split_first()
        .context("portable sandbox command is empty")?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = Command::new(program).args(arguments).exec();
        Err(error).with_context(|| format!("execute sandboxed workload {program}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (program, arguments);
        bail!("portable process sandbox is unavailable on this platform")
    }
}

fn desktop_inspect(project: &str) -> Result<()> {
    let path = project_path(project, false)?;
    let view = desktop_control::inspect(&path)?;
    println!("{}", serde_json::to_string(&view)?);
    Ok(())
}

/// Run a Formation request against the local Runtime and print the result.
///
/// The JSON result goes to stdout either way — the attempts are the
/// evidence. Exit status says which variant came back: 'formed' is success,
/// 'no_verified_route' is not.
fn read_token(path: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(path)
        .with_context(|| format!("cannot read the token file {}", path.display()))?
        .trim()
        .to_owned())
}

/// The browser verifier to use: the helper inside the verifier sandbox, or
/// — only when a developer asks for it — the helper on the host.
fn browser_verifier_command(
    dir: Option<PathBuf>,
    node: &str,
    chrome: Option<PathBuf>,
    allow_uncontained: bool,
) -> Result<Option<ato_formation_worker::browser_verify::BrowserVerifierCommand>> {
    use ato_formation_worker::browser_sandbox::BrowserVerifierSandboxSpec;
    use ato_formation_worker::browser_verify::BrowserVerifierCommand;
    let Some(dir) = dir else {
        return Ok(None);
    };
    let dir = dir
        .canonicalize()
        .with_context(|| format!("cannot read the browser verifier at {}", dir.display()))?;
    let contained = chrome
        .as_deref()
        .context("the browser verifier needs --browser-chrome (or ATO_BROWSER_CHROME_PATH)")
        .and_then(|chrome| BrowserVerifierSandboxSpec::node_helper(&dir, node, chrome));
    match contained {
        Ok(spec) => {
            let command = BrowserVerifierCommand::Contained(spec);
            if command.usable() || !allow_uncontained {
                return Ok(Some(command));
            }
        }
        Err(error) if !allow_uncontained => return Err(error),
        Err(_) => {}
    }
    eprintln!(
        "warning: the browser verifier runs UNCONTAINED on this host \
         (--allow-uncontained-browser-verifier); development only"
    );
    Ok(Some(BrowserVerifierCommand::uncontained_node_helper(
        node.to_owned(),
        dir,
    )))
}

/// Submit a SatisfyRequest and wait for the coordinator to settle it.
fn form_on_runtime_network(args: FormArgs) -> Result<()> {
    if args.exploration_config.is_some() {
        return formation_exploration::run(args);
    }
    use ato_formation_worker::runtime_network::{
        Client, RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, Settlement, new_search_id,
        prepare_submission,
    };
    let api = args.api.context("--runtime-network needs --api")?;
    let token = read_token(
        &args
            .token_file
            .context("--runtime-network needs --token-file")?,
    )?;
    let browser_contract = args
        .accept
        .as_deref()
        .map(ato_formation::browser::BrowserContractV0::from_prompt)
        .transpose()
        .context("--accept is not a usable acceptance prompt")?;
    let work_root = args.work_root.clone().unwrap_or_else(|| {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("ato/runtime-network/submit")
    });
    // An invocation starts a new search unless told to continue one. Either
    // way it makes one request of that search, which spends from the
    // search's budget, not from a budget of its own.
    let search_id = match args.search_id.clone() {
        Some(search_id) => search_id,
        None => {
            let mut entropy = [0_u8; 16];
            getrandom::fill(&mut entropy)
                .map_err(|error| anyhow::anyhow!("cannot draw a search id: {error}"))?;
            new_search_id(entropy)
        }
    };
    let decision_policy =
        args.decision_provider
            .as_ref()
            .map(|_| ato_formation::decision::DecisionPolicy {
                provider: ato_formation::decision::ProviderLocation::Requester,
                max_decisions: args.max_decisions,
                decision_timeout_ms: args.decision_timeout_ms,
            });
    if let Some(policy) = &decision_policy {
        policy.validate().map_err(|_| {
            anyhow::anyhow!("--max-decisions must be 1–64 and --decision-timeout-ms 1000–120000")
        })?;
    }
    // The provider must answer before the Coordinator's deadline for the point.
    let provider = match &decision_policy {
        Some(policy) => Some(
            ato_formation_worker::decision_provider::JevDecisionProvider::from_env(
                std::time::Duration::from_millis(policy.decision_timeout_ms * 3 / 4),
            )?,
        ),
        None => None,
    };
    let mut answered = std::collections::BTreeSet::new();
    let mut submission = prepare_submission(
        &args.path,
        &args.routes,
        browser_contract,
        &work_root,
        match args.exact_runtime {
            Some(runtime_id) => RuntimeConstraintWire::Exact {
                runtime_id,
                environment_id: None,
            },
            None => RuntimeConstraintWire::Any,
        },
        SatisfyPolicy {
            network: args.network.clone(),
            allow_managed: args.allow_managed,
            decision: decision_policy.clone(),
            generation: None,
            proposal: None,
            exploration: None,
        },
        SatisfyBudget {
            max_attempts: u32::try_from(args.max_attempts)
                .context("--max-attempts is too large")?,
            mode: args.mode.clone(),
            deadline_seconds: args.deadline_seconds,
            max_transfer_bytes: args.max_transfer_bytes,
            max_expanded_bytes: args.max_expanded_bytes,
            max_stored_bytes: args.max_stored_bytes,
        },
        &search_id,
    )?;
    let generation_provider = if args.generation_provider.is_some() {
        let mut entries = std::collections::BTreeMap::new();
        for entry in &args.generation_entrypoints {
            let (id, path) = entry
                .split_once('=')
                .context("--generation-entrypoint expects ID=PATH")?;
            anyhow::ensure!(
                entries.insert(id.to_owned(), path.to_owned()).is_none(),
                "duplicate entrypoint ID"
            );
        }
        submission.authorize_generation(entries, 30_000)?;
        submission.enable_generation_context()?;
        Some(
            ato_formation_worker::generation_provider::JevGenerationProvider::from_env_v2(
                std::time::Duration::from_secs(20),
            )?,
        )
    } else {
        None
    };
    let client = Client::new(&api, &token)?;
    let accepted = client.submit(&submission)?;
    let id = accepted["satisfy_id"]
        .as_str()
        .context("the coordinator returned no satisfy_id")?
        .to_owned();
    eprintln!(
        "satisfy {id} (search {search_id}): {} candidate(s) admissible",
        accepted["admissible"]
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60 * 30);
    loop {
        let status = client.satisfy_status(&id)?;
        submission.accept_generated_candidate(&status)?;
        if let Some(provider) = &generation_provider {
            ato_formation_worker::runtime_network::serve_generation(
                &mut submission,
                &client,
                &id,
                &status,
                provider,
            )?;
        }

        if let Some(provider) = &provider {
            ato_formation_worker::decision_provider::serve_decision(
                &client,
                &id,
                &status,
                provider,
                &mut answered,
            );
        }
        let settlement = Settlement::of(&status)?;
        if settlement != Settlement::Running {
            println!("{}", serde_json::to_string_pretty(&status)?);
            if settlement.terminal_reason().is_some() {
                // UNKNOWN is its own reason: an attempt may have run, so it
                // is never shown as a failure or as inconclusive.
                bail!("the Runtime Network produced no verified route — {settlement}");
            }
            // A route counts only when its receipt is acceptable as the
            // result of the attempt that reported it, checked here against
            // the K this requester froze.
            let (accepted, refused) = ato_formation_worker::runtime_network::accept_verified_routes(
                &submission,
                &id,
                &status,
            );
            for reason in &refused {
                eprintln!("verified route refused: {reason}");
            }
            if accepted.is_empty() {
                bail!("no verified route carries an acceptable receipt");
            }
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            bail!("satisfy {id} did not settle within 30 minutes");
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

fn runtime_network_serve(args: RuntimeNetworkServeArgs) -> Result<()> {
    let base = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ato/runtime-network");
    ato_formation_worker::runtime_network::serve(
        &ato_formation_worker::runtime_network::ServeConfig {
            exploration: args
                .exploration_sandbox
                .map(|p| -> Result<_> { Ok(serde_json::from_slice(&std::fs::read(p)?)?) })
                .transpose()?,
            api: args.api,
            token: read_token(&args.token_file)?,
            work_root: args.work_root.unwrap_or_else(|| base.join("work")),
            out_dir: args.out.unwrap_or_else(|| base.join("out")),
            shim: std::env::current_exe().context("cannot locate this binary")?,
            // Contained or nothing: a Runtime never verifies uncontained.
            browser_verifier: browser_verifier_command(
                args.browser_verifier,
                &args.node,
                args.browser_chrome,
                false,
            )
            .unwrap_or_else(|error| {
                eprintln!("[runtime-network] no browser verifier: {error:#}");
                None
            }),
            poll: std::time::Duration::from_secs(2),
            max_tickets: args.max_attempts,
        },
    )
}

fn read_inspection_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("inspection JSON exceeds 1 MiB");
    }
    serde_json::from_slice(&bytes).context("invalid inspection JSON")
}

fn form(args: FormArgs) -> Result<()> {
    use ato_formation::request::{
        ContractSource, FormationNetworkPolicy, FormationPolicy, FormationRequest, FormationResult,
        InitialCondition, RuntimeConstraint, SearchBudget,
    };

    let network = match args.network.as_str() {
        "denied" => FormationNetworkPolicy::Denied,
        "dependency-resolution" => FormationNetworkPolicy::DependencyResolution,
        other => bail!("--network must be denied or dependency-resolution (got {other})"),
    };
    let out_dir = args.out.unwrap_or_else(|| {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("ato")
            .join("formation")
    });
    let work_root = args.work_root.unwrap_or_else(|| out_dir.join("work"));
    let request = FormationRequest {
        initial_condition: InitialCondition::LocalDirectory { path: args.path },
        contract: ContractSource::Infer,
        runtime: RuntimeConstraint::Exact {
            runtime_id: args.runtime,
        },
        policy: FormationPolicy { network },
        budget: SearchBudget {
            max_attempts: args.max_attempts,
        },
        browser_contract: args
            .accept
            .as_deref()
            .map(ato_formation::browser::BrowserContractV0::from_prompt)
            .transpose()
            .context("--accept is not a usable acceptance prompt")?,
    };
    let browser_verifier = if args.verify_browser {
        browser_verifier_command(
            args.browser_verifier,
            &args.node,
            args.browser_chrome,
            args.allow_uncontained_browser_verifier,
        )?
    } else {
        None
    };
    let env = ato_formation_worker::local::LocalFormation {
        work_root,
        out_dir,
        // This binary, re-exec'd inside the build sandbox as sandbox-exec.
        shim: std::env::current_exe().context("cannot locate this binary")?,
        limits: ato_formation_worker::sandbox::BuildLimits::default(),
        source_limits: ato_formation::source::SourceLimits::default(),
        browser_verifier,
        browser_budget: ato_formation::browser::BrowserBudget::default(),
    };
    let result = ato_formation_worker::local::run(&request, &env)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    match result {
        FormationResult::Formed { .. } => Ok(()),
        FormationResult::NoVerifiedRoute { .. } => {
            bail!("formation produced no verified route")
        }
    }
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
        start_durable(
            &repository,
            "main",
            &initial,
            &bindings,
            None,
            &cli_materializers,
        )?;
        // The CLI exits here, so the run is reparented to init and reaped by
        // it. Nothing to wait on.
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
        &cli_materializers,
    )?;
    println!("resumed {branch} at {selected}");
    Ok(())
}

fn stop(capsule: &str) -> Result<()> {
    let project = project_path(capsule, false)?;
    let repository = LocalCapsuleRepository::open(project)?;
    let sealed =
        ato_local_execution::stop_and_seal(&repository)?.context("Capsule has no active Run")?;
    println!("sealed {} at {}", sealed.run.branch, sealed.head);
    Ok(())
}

fn encap(args: EncapArgs) -> Result<()> {
    let selector: CapsuleSelector = args.selector.parse()?;
    let project = project_path(&selector.capsule, false)?;
    let repository = LocalCapsuleRepository::open(project)?;
    let target = repository.resolve(&selector)?;
    let state = load_runtime_state(&target, repository.objects())?;
    let selected = if args.materializers.is_empty() {
        if state.config.encap.materializers.is_empty() {
            vec!["ato.replay@1".to_owned()]
        } else {
            state.config.encap.materializers.clone()
        }
    } else {
        args.materializers
    };
    let entries = encode_materializations(&repository, &selector, &target, &state, selected)?;
    let references = reference_registry()?;
    let bundle =
        export_bundle_with_materializations(&target, &entries, repository.objects(), &references)?;
    ato_local_execution::atomic_write(&args.output, &encode_bundle(&bundle)?)?;
    println!("{target}");
    Ok(())
}

fn encode_materializations(
    repository: &LocalCapsuleRepository,
    selector: &CapsuleSelector,
    target: &ComputationRef,
    state: &ato_local_execution::authoring::AuthoringState,
    selected: Vec<String>,
) -> Result<Vec<BundleMaterialization>> {
    let records = repository.records_for_causal_branch(&selector.branch, selector.record)?;
    let adapters = adapter_registry()?;
    let materializers = materializer_registry()?;
    let capture_policy = workspace_policy(&state.config)?;
    let (records_v2, replay_anchor, record_frontier_ref) = if selected
        .iter()
        .any(|materializer| materializer == "ato.replay@2")
    {
        let (records, anchor, frontier) =
            load_run_record_frontier(repository, &selector.branch, target)?;
        (records, Some(anchor), Some(frontier))
    } else {
        (Vec::new(), None, None)
    };
    let context = MaterializerContext {
        objects: repository.objects(),
        adapters: &adapters,
        records: &records,
        records_v2: &records_v2,
        replay_anchor: replay_anchor
            .as_ref()
            .or_else(|| records.first().map(|record| &record.head_before)),
        record_frontier_ref: record_frontier_ref.as_ref(),
        workspace: repository.project(),
        workspace_policy: &capture_policy,
        realization: None,
        contracts: &[],
        runner_capabilities: None,
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
    Ok(entries)
}

fn upload(args: UploadArgs) -> Result<()> {
    let token = auth_device::resolve_token(args.auth_token, &args.api_url)?;
    let selector: CapsuleSelector = args.selector.parse()?;
    let project = project_path(&selector.capsule, false)?;
    let repository = LocalCapsuleRepository::open(project)?;
    let target = repository.resolve(&selector)?;
    let state = load_runtime_state(&target, repository.objects())?;
    let selected = if args.materializers.is_empty() {
        if state.config.encap.materializers.is_empty() {
            vec!["ato.replay@2".to_owned()]
        } else {
            state.config.encap.materializers.clone()
        }
    } else {
        args.materializers
    };
    let entries = encode_materializations(&repository, &selector, &target, &state, selected)?;
    let graph_materializations = entries
        .iter()
        .map(|entry| GraphMaterialization {
            id: entry.materializer_id.clone(),
            descriptor_ref: entry.descriptor_ref.clone(),
            restore_capability: if entry.materializer_id == "ato.snapshot@1" {
                GraphRestoreCapability::VerifyOnly
            } else {
                GraphRestoreCapability::Supported
            },
        })
        .collect::<Vec<_>>();
    let references = reference_registry()?;
    let closure = export_object_graph(
        &target,
        &graph_materializations,
        repository.objects(),
        &references,
    )?;
    let exported_ports = resolve_computation(repository.objects(), &target)?
        .object()
        .boundary
        .iter()
        .map(|(port, definition)| ExportedPort {
            port_id: port.to_string(),
            protocol: definition.protocol.to_string(),
            role: definition.role.to_string(),
        })
        .collect();
    let required_bindings = state
        .config
        .binding
        .iter()
        .map(|binding| RequiredBinding {
            id: binding.id.clone(),
            schema: binding.protocol.clone(),
        })
        .collect();
    let index =
        ObjectGraphIndexV1::new(closure, exported_ports, required_bindings, args.visibility);
    let idempotency_key = args.idempotency_key.unwrap_or_else(|| {
        format!(
            "ato-object-upload-v1-{}",
            index.digest().expect("JCS index")
        )
    });
    if !(16..=160).contains(&idempotency_key.len()) {
        bail!("idempotency key must contain between 16 and 160 bytes");
    }
    let api = HttpObjectTransportApi::new(&args.api_url, token)?;
    let (vm_materialization_descriptor_ref, record_frontier_ref) =
        object_transport::vm_capture_receipt_refs(&index, repository.objects())?;
    let mut receipt = upload_http_object_graph(
        &api,
        &index,
        repository.objects(),
        &references,
        &idempotency_key,
        UploadConfig {
            concurrency: usize::from(args.concurrency),
            retry_attempts: args.retry_attempts,
            validation_poll_attempts: args.validation_poll_attempts,
            validation_poll_interval: Duration::from_millis(args.validation_poll_ms),
        },
    )?;
    receipt.vm_materialization_descriptor_ref = vm_materialization_descriptor_ref;
    receipt.record_frontier_ref = record_frontier_ref;
    ato_local_execution::atomic_write(&args.receipt, &serde_jcs::to_vec(&receipt)?)?;
    println!("{} {}", receipt.bundle_id, receipt.root_computation_ref);
    Ok(())
}

fn login(args: LoginArgs) -> Result<()> {
    let credential = auth_device::device_login(
        &args.api_url,
        &format!("ato/{}", env!("CARGO_PKG_VERSION")),
        |prompt| {
            eprintln!(
                "Open this URL in a browser to approve the login:\n{}\nVerification code: {}",
                prompt.activate_url, prompt.user_code
            );
            if !args.headless
                && let Err(error) = open_browser(&prompt.activate_url)
            {
                eprintln!("could not open a browser: {error:#}");
            }
        },
    )?;
    let stored = auth_device::StoredCredential::from(credential);
    let path = auth_device::save_credential(&args.api_url, &stored)?;
    match stored.handle.as_deref().filter(|handle| !handle.is_empty()) {
        Some(handle) => println!("Logged in as {handle} ({})", stored.registry_url),
        None => println!("Logged in ({})", stored.registry_url),
    }
    println!("Credential saved to {}", path.display());
    Ok(())
}

fn load_run_record_frontier(
    repository: &LocalCapsuleRepository,
    branch: &str,
    target: &ComputationRef,
) -> Result<(
    Vec<ato_objects::RecordEnvelopeV2>,
    ComputationRef,
    ContentRef,
)> {
    let mut matches = Vec::new();
    for entry in fs::read_dir(repository.root().join("runs"))? {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.ends_with(".sealed-record-frontier.json"))
        {
            continue;
        }
        let bytes = fs::read(&path)?;
        let association: ato_local_execution::SealedRunRecordFrontier =
            serde_json::from_slice(&bytes)?;
        if association.version != 1 || serde_jcs::to_vec(&association)? != bytes {
            bail!(
                "non-canonical sealed Run/RecordFrontier association at {}",
                path.display()
            );
        }
        if association.branch == branch && association.target_computation_ref == target.to_string()
        {
            matches.push(association);
        }
    }
    let [association] = matches.as_slice() else {
        bail!(
            "ato.replay@2 requires exactly one sealed RecordFrontier for {target}; found {}",
            matches.len()
        );
    };
    let reference = ContentRef::parse(&association.record_frontier_ref)?;
    let frontier = load_frontier(
        &repository.root().join("records"),
        &association.run_id,
        &reference,
    )?;
    let records = records_for_frontier(
        &repository.root().join("records"),
        &frontier,
        repository.objects(),
    )?;
    Ok((
        records,
        ComputationRef::parse(&association.anchor_computation_ref)?,
        reference,
    ))
}

fn run_capsule(args: RunArgs) -> Result<()> {
    if args.capsule.extension().and_then(|value| value.to_str()) != Some("capsule")
        || !args.capsule.is_file()
    {
        bail!(
            "`ato run` accepts only a portable .capsule file; author repositories with `ato init`"
        );
    }
    let bytes = fs::read(&args.capsule)?;
    match decode_capsule_bundle_document(&bytes)? {
        CapsuleBundleDocument::ComputationV2(bundle) => run_computation_bundle(args, bundle),
        CapsuleBundleDocument::PortableApplicationV3(bundle) => {
            run_portable_application(args, &bytes, bundle)
        }
        CapsuleBundleDocument::PortableApplicationV4(bundle) => {
            run_portable_application(args, &bytes, bundle)
        }
    }
}

fn local_application_store() -> Result<LocalApplicationStore> {
    LocalApplicationStore::open(ato_home()?).map_err(Into::into)
}

fn import_local_application(args: AppImportArgs) -> Result<()> {
    if args.capsule.extension().and_then(|value| value.to_str()) != Some("capsule")
        || !args.capsule.is_file()
    {
        bail!("`ato app import` accepts only a portable .capsule file");
    }
    let bytes =
        fs::read(&args.capsule).with_context(|| format!("read {}", args.capsule.display()))?;
    let instance = local_application_store()?.import(&bytes, args.derivation.as_deref())?;
    println!("{}", serde_json::to_string_pretty(&instance)?);
    Ok(())
}

fn start_local_instance(args: AppStartArgs) -> Result<()> {
    let store = local_application_store()?;
    let instance = store.instance(&args.instance)?;
    let bindings = runtime_binding_values(args.bindings)?;
    let bundle_bytes = store.bundle_bytes(&instance)?;
    let bundle = portable_export_bundle(&bundle_bytes)?;
    let selected = validate_bundle_for_derivation(&bundle, &instance.selected_derivation_ref)?;
    resolve_application_bindings(&selected.application, &bindings)?;
    let binding_payload = serde_json::to_vec(&bindings)?;
    let starting = store.claim_run(&instance.instance_id)?;
    let run_root = store.run_root(&instance.instance_id, &starting.run_id)?;
    if matches!(
        selected.realization,
        PortableRealizationKind::OciContainer | PortableRealizationKind::OciServiceGroup
    ) {
        mark_stop_pending(&run_root)?;
    }
    let log_path = run_root.join("output.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open local Instance log {}", log_path.display()))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__portable-instance-worker")
        .arg(&instance.instance_id)
        .arg(&starting.run_id)
        .arg(&starting.token)
        .env("ATO_HOME", ato_home()?)
        .stdin(Stdio::piped())
        .stdout(stdout.try_clone()?)
        .stderr(stdout);
    configure_detached_process(&mut command);
    prevent_worker_from_inheriting_parent_stdio()?;
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            clear_stop_pending(&run_root)?;
            let _ = store.release_run(&instance.instance_id, &starting.token);
            return Err(error).context("start durable local Instance worker");
        }
    };
    if let Some(mut stdin) = child.stdin.take()
        && let Err(error) = stdin.write_all(&binding_payload)
    {
        terminate_unactivated_worker(&mut child);
        let _ = store.release_run(&instance.instance_id, &starting.token);
        return Err(error).context("deliver runtime Bindings to local Instance worker");
    }
    let wait_started = Instant::now();
    let active = loop {
        if let Some(active) = store.active_run(&instance.instance_id)?
            && active.token == starting.token
            && active.status == LocalInstanceRunStatus::Active
        {
            break active;
        }
        if child.try_wait()?.is_some() {
            let _ = store.release_run(&instance.instance_id, &starting.token);
            bail!(
                "local Instance worker exited before becoming active; see {}",
                log_path.display()
            );
        }
        if wait_started.elapsed() > Duration::from_secs(60) {
            terminate_unactivated_worker(&mut child);
            let _ = store.release_run(&instance.instance_id, &starting.token);
            bail!(
                "local Instance worker did not become active within 60 seconds; see {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let receipt_name = active
        .receipt_path
        .as_deref()
        .context("active local Instance Run omitted its receipt")?;
    let receipt_path = run_root.join(receipt_name);
    if let Some(output) = args.verification_receipt {
        let receipt = fs::read(&receipt_path)
            .with_context(|| format!("read local Instance receipt {}", receipt_path.display()))?;
        ato_local_execution::atomic_write(&output, &receipt)
            .with_context(|| format!("write verification receipt {}", output.display()))?;
    }
    let url = active
        .url
        .as_deref()
        .context("active local Instance Run omitted its Surface URL")?;
    println!("Instance: {}", instance.instance_id);
    println!("Run: {}", active.run_id);
    println!("Capsule: {}", instance.contract_ref);
    println!("Route: {}", instance.selected_derivation_ref);
    println!("URL: {url}");
    if !args.no_open {
        open_browser(url)?;
    }
    Ok(())
}

fn terminate_unactivated_worker(child: &mut Child) {
    let pid = child.id();
    if terminate_process_tree(pid, pid).is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(windows)]
fn prevent_worker_from_inheriting_parent_stdio() -> Result<()> {
    use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
    use windows::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    for (name, kind) in [
        ("stdin", STD_INPUT_HANDLE),
        ("stdout", STD_OUTPUT_HANDLE),
        ("stderr", STD_ERROR_HANDLE),
    ] {
        // SAFETY: GetStdHandle returns a borrowed process handle. We neither
        // close it nor change its access; we only clear the inheritance bit
        // before spawning a detached worker with explicitly configured stdio.
        let handle =
            unsafe { GetStdHandle(kind) }.with_context(|| format!("read Windows {name} handle"))?;
        if handle.0.is_null() {
            continue;
        }
        // SAFETY: `handle` is the live standard handle returned above, and
        // both flag arguments are defined by SetHandleInformation.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
            .with_context(|| format!("make Windows {name} handle non-inheritable"))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn prevent_worker_from_inheriting_parent_stdio() -> Result<()> {
    Ok(())
}

fn inspect_local_instance(instance_id: &str) -> Result<()> {
    let store = local_application_store()?;
    let instance = store.instance(instance_id)?;
    let active = store.active_run(instance_id)?.map(|run| {
        serde_json::json!({
            "schema": run.schema,
            "run_id": run.run_id,
            "instance_id": run.instance_id,
            "status": run.status,
            "pid": run.pid,
            "url": run.url,
            "receipt_path": run.receipt_path,
            "created_at": run.created_at,
        })
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "instance": instance,
            "active_run": active,
        }))?
    );
    Ok(())
}

fn export_local_instance(args: AppExportArgs) -> Result<()> {
    let store = local_application_store()?;
    let instance = store.instance(&args.instance)?;
    let bytes = store.export_instance(&args.instance)?;
    ato_local_execution::atomic_write(&args.output, &bytes)
        .with_context(|| format!("write portable Instance export {}", args.output.display()))?;
    println!("Exported: {}", args.output.display());
    println!("Bundle: {}", instance.bundle_sha256);
    println!("Capsule: {}", instance.contract_ref);
    println!("Route: {}", instance.selected_derivation_ref);
    if let Some(snapshot_ref) = instance.data_snapshot_ref {
        println!("Snapshot: {snapshot_ref}");
    }
    Ok(())
}

fn stop_local_instance(instance_id: &str) -> Result<()> {
    let store = local_application_store()?;
    let active = store
        .active_run(instance_id)?
        .context("local Instance has no active Run")?;
    if active.status == LocalInstanceRunStatus::Quarantined {
        bail!(
            "local Instance Run is quarantined: confirm its recorded OCI resources stopped before recovery; worker exit is not stop evidence"
        );
    }
    if active.status != LocalInstanceRunStatus::Active {
        bail!("local Instance Run is still preparing and cannot be stopped");
    }
    let identity = local_run_process_identity(&active)?;
    if !identity.matches_live_process()? {
        bail!(
            "local Instance Run process identity no longer matches; refusing to stop PID {}",
            identity.pid
        );
    }
    let request = store.stop_request_path(instance_id, &active.run_id)?;
    let ack = store.stop_ack_path(instance_id, &active.run_id)?;
    if ack.exists() {
        fs::remove_file(&ack)?;
    }
    fs::write(&request, b"stop")?;
    let mut acknowledged = None;
    let mut worker_exited = false;
    for _ in 0..250 {
        if let Ok(value) = fs::read_to_string(&ack) {
            acknowledged = Some(value);
            break;
        }
        if !worker_exited && !identity.matches_live_process()? {
            // The worker writes the acknowledgement immediately before it
            // exits. File visibility can lag process observation briefly,
            // especially on Windows, so keep polling within the same bound.
            worker_exited = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let acknowledged = acknowledged.with_context(|| {
        if worker_exited {
            "local Instance Run exited before cleanup acknowledgement became readable"
        } else {
            "timed out waiting for local Instance cleanup"
        }
    })?;
    if acknowledged.trim() != "ok" {
        bail!("local Instance cleanup failed: {acknowledged}");
    }
    // The worker normally exits itself after persisting state and publishing
    // the acknowledgement. Keep the identity-checked termination as a
    // bounded fallback for the small race where it is still unwinding.
    if identity.matches_live_process()? {
        terminate_owned_process(&identity)?;
    }
    store.release_run(instance_id, &active.token)?;
    let _ = fs::remove_file(request);
    let _ = fs::remove_file(ack);
    println!("Stopped Run {} for Instance {instance_id}", active.run_id);
    Ok(())
}

// Written before a worker can start OCI. Worker exit cannot prove Docker stopped.
fn mark_stop_pending(root: &Path) -> Result<()> {
    ato_local_execution::atomic_write(
        &root.join(ato_portable_application::local_instance::LOCAL_RUN_STOP_PENDING_FILE),
        br#"{"reason":"execution may exist; explicit stop confirmation required"}"#,
    )?;
    Ok(())
}

fn clear_stop_pending(root: &Path) -> Result<()> {
    let path = root.join(ato_portable_application::local_instance::LOCAL_RUN_STOP_PENDING_FILE);
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn record_stop(root: Option<&Path>, stopped: &Result<()>) -> Result<()> {
    use ato_runtime_attempt::realize::StopClass;
    let class = StopClass::of(stopped);
    let record = serde_json::json!({
        "schema": "ato.local-run-lifecycle/1",
        "stop": if stopped.is_ok() { "succeeded" } else { "failed" },
        "stop_confirmed": class.is_confirmed(),
        "scratch_removed": matches!(class, StopClass::Confirmed),
        "reason": stopped.as_ref().err().map(|e| format!("{e:#}")),
        "resources": match &class { StopClass::Unconfirmed { resources, .. } => resources.clone(), _ => Vec::new() },
    });
    if let Some(root) = root {
        ato_local_execution::atomic_write(
            &root.join("lifecycle.json"),
            &serde_json::to_vec_pretty(&record)?,
        )?;
    }
    Ok(())
}

/// The only gate from candidate cessation to durable state and Run release.
fn finish_instance_stop(
    store: &LocalApplicationStore,
    run: &LocalInstanceRun,
    root: &Path,
    stopped: Result<()>,
    local_storage: Option<BTreeMap<String, String>>,
    ack: Option<&Path>,
) -> Result<()> {
    use ato_runtime_attempt::realize::StopClass;
    let class = StopClass::of(&stopped);
    if let StopClass::Unconfirmed { reason, resources } = &class {
        // The pre-launch marker also fences release if writing quarantine fails.
        store.quarantine_run(&run.instance_id, &run.token, reason, resources)?;
        bail!("Run {} retained: {reason}", run.run_id);
    }
    clear_stop_pending(root)?;
    if let Some(storage) = local_storage {
        store.save_browser_state_for_run(&run.instance_id, &run.token, &storage)?;
    }
    store.save_filesystem_state_for_run(&run.instance_id, &run.token)?;
    store.release_run(&run.instance_id, &run.token)?;
    if let Some(ack) = ack {
        // Scratch failure is not an unconfirmed workload, but is still an error.
        fs::write(
            ack,
            if stopped.is_ok() {
                "ok"
            } else {
                "scratch_cleanup_failed"
            },
        )?;
    }
    stopped
}

fn portable_instance_worker(args: PortableInstanceWorkerArgs) -> Result<()> {
    let mut binding_payload = Vec::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_end(&mut binding_payload)
        .context("read runtime Bindings from parent")?;
    let bindings = if binding_payload.is_empty() {
        BTreeMap::new()
    } else {
        serde_json::from_slice(&binding_payload).context("decode runtime Bindings from parent")?
    };
    let store = local_application_store()?;
    let claimed = store
        .active_run(&args.instance)?
        .context("local Instance Run claim is missing")?;
    if claimed.run_id != args.run_id
        || claimed.token != args.token
        || claimed.status != LocalInstanceRunStatus::Starting
    {
        bail!("local Instance Run claim does not match this worker");
    }
    let result = portable_instance_worker_claimed(&store, &claimed, &bindings);
    if let Err(error) = &result
        && let Some(ato_runtime_attempt::realize::CandidateStopFailure::Unconfirmed {
            reason,
            resources,
        }) = error.downcast_ref::<ato_runtime_attempt::realize::CandidateStopFailure>()
    {
        store.quarantine_run(&claimed.instance_id, &claimed.token, reason, resources)?;
    }
    if result.is_err()
        && let Err(error) = store.release_run(&claimed.instance_id, &claimed.token)
    {
        eprintln!("Run retained after worker failure: {error}");
    }
    result
}

fn portable_instance_worker_claimed(
    store: &LocalApplicationStore,
    claimed: &LocalInstanceRun,
    bindings: &BTreeMap<String, String>,
) -> Result<()> {
    #[cfg(unix)]
    let shutdown = Some({
        let flag = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&flag))?;
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&flag))?;
        flag
    });
    #[cfg(not(unix))]
    let shutdown: Option<Arc<AtomicBool>> = None;

    let instance = store.instance(&claimed.instance_id)?;
    let bundle_bytes = store.bundle_bytes(&instance)?;
    let bundle = portable_export_bundle(&bundle_bytes)?;
    let selected = validate_bundle_for_derivation(&bundle, &instance.selected_derivation_ref)?;
    let restored_snapshot = store.restored_snapshot(&claimed.instance_id)?;
    if selected.realization != PortableRealizationKind::StaticWeb
        && restored_snapshot.as_ref().is_some_and(|snapshot| {
            !snapshot.assets.is_empty()
                || snapshot.resources.iter().any(|resource| {
                    resource.protocol != STATE_FILESYSTEM_PROTOCOL
                        || !selected
                            .derivation
                            .state
                            .iter()
                            .any(|state| state.id == resource.slot)
                })
        })
    {
        bail!(
            "local dynamic Instance snapshot restore supports only its declared filesystem state"
        );
    }
    let restored_snapshot_ref = instance.data_snapshot_ref.clone();
    let static_state = if selected.realization == PortableRealizationKind::StaticWeb {
        store
            .local_surface_snapshot(&claimed.instance_id)?
            .map(|snapshot| {
                let persistence_token =
                    bundle_sha256(format!("{}\0local-state", claimed.token).as_bytes());
                StaticApplicationState {
                    persistence_token,
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
                }
            })
    } else {
        None
    };
    let filesystem_state = store.filesystem_state_paths(&claimed.instance_id)?;
    let run_root = store.run_root(&claimed.instance_id, &claimed.run_id)?;
    let mut started = start_and_verify_portable_application(
        &bundle_bytes,
        bundle,
        &instance.selected_derivation_ref,
        &run_root,
        shutdown.as_deref(),
        PortableRuntimeState {
            restored_snapshot_ref: restored_snapshot_ref.as_deref(),
            static_state,
            filesystem_state: Some(&filesystem_state),
            bindings: Some(bindings),
            run_id: Some(&claimed.run_id),
        },
    )?;
    let result = (|| -> Result<_> {
        if started.receipt.bundle_sha256.as_deref() != Some(instance.bundle_sha256.as_str())
            || started.receipt.contract_ref != instance.contract_ref
            || started.receipt.derivation_ref != instance.selected_derivation_ref
        {
            bail!("local Instance verification receipt does not match imported metadata");
        }
        let receipt = started.receipt.canonical_bytes()?;
        let process = OwnedProcessIdentity::current()?;
        let active = store.activate_run(
            claimed,
            LocalRunActivation {
                pid: process.pid,
                process_start_time: process.process_start_time,
                process_group: process.process_group,
                boot_session: process.boot_session,
                url: started.runtime.base_url().to_owned(),
                receipt: &receipt,
            },
        )?;
        let request = store.stop_request_path(&active.instance_id, &active.run_id)?;
        loop {
            if request.exists()
                || shutdown
                    .as_deref()
                    .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                return Ok((started.runtime.local_storage()?, request.exists()));
            }
            if started.runtime.stops_when_a_service_exits {
                started.runtime.try_wait()?;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    // Every return after launch passes through explicit stop, including receipt,
    // activation, browser-state read and service-exit failures.
    let stopped = started.runtime.stop_recorded(Some(&run_root));
    match result {
        Ok((storage, requested)) => {
            let ack = store.stop_ack_path(&claimed.instance_id, &claimed.run_id)?;
            finish_instance_stop(
                store,
                claimed,
                &run_root,
                stopped,
                storage,
                requested.then_some(ack.as_path()),
            )
        }
        Err(original) => {
            if let ato_runtime_attempt::realize::StopClass::Unconfirmed { reason, resources } =
                ato_runtime_attempt::realize::StopClass::of(&stopped)
            {
                store.quarantine_run(&claimed.instance_id, &claimed.token, &reason, &resources)?;
            } else {
                clear_stop_pending(&run_root)?;
                store.release_run(&claimed.instance_id, &claimed.token)?;
            }
            match stopped {
                Err(cleanup) => Err(cleanup.context(format!("Run failed: {original:#}"))),
                Ok(()) => Err(original),
            }
        }
    }
}

fn local_run_process_identity(run: &LocalInstanceRun) -> Result<OwnedProcessIdentity> {
    Ok(OwnedProcessIdentity {
        pid: run.pid.context("active local Instance Run omitted pid")?,
        process_start_time: run
            .process_start_time
            .clone()
            .context("active local Instance Run omitted process start time")?,
        process_group: run
            .process_group
            .context("active local Instance Run omitted process group")?,
        boot_session: run
            .boot_session
            .clone()
            .context("active local Instance Run omitted boot session")?,
    })
}

fn export_plan(args: ExportPlanArgs) -> Result<()> {
    let bytes =
        fs::read(&args.capsule).with_context(|| format!("read {}", args.capsule.display()))?;
    let bundle = portable_export_bundle(&bytes)?;
    let mut plan = plan_portable_export(&bundle, bytes.len(), args.portability)?;
    if args.oci_archive.is_some() && args.portability != PortableExportProfile::Offline {
        bail!("--oci-archive is currently supported only for offline export");
    }
    let archives =
        portable_export_archives(&bundle, args.portability, args.oci_archive.as_deref())?;
    let source = portable_export_source(&bundle, args.portability);
    let resolved_sources = match args.portability {
        PortableExportProfile::Thin => portable_dependency::discover_wheel_sources(&bundle),
        PortableExportProfile::Cached | PortableExportProfile::Offline => Ok(BTreeMap::new()),
    };
    match source
        .and_then(|source| resolved_sources.map(|sources| (source, sources)))
        .and_then(|(source, sources)| {
            let profile = match args.portability {
                PortableExportProfile::Thin => PortableDependencyProfile::Thin,
                PortableExportProfile::Cached => PortableDependencyProfile::Cached,
                PortableExportProfile::Offline => PortableDependencyProfile::Offline,
            };
            repack_portable_dependencies_with_archives(&source, profile, &sources, &archives)
                .map(|(output, _)| output.len())
                .map_err(Into::into)
        }) {
        Ok(size) => {
            plan.estimated_export_bytes = Some(size);
            plan.blockers.clear();
            if args.portability == PortableExportProfile::Offline {
                plan.requires_network_on_clean_host = Some(false);
                plan.embedded_objects += archives.len();
                plan.external_objects = 0;
                plan.embedded_dependency_bytes += archives
                    .iter()
                    .map(|archive| {
                        (archive.bytes.len() / 4 * 3
                            - archive
                                .bytes
                                .as_bytes()
                                .iter()
                                .rev()
                                .take_while(|byte| **byte == b'=')
                                .count()) as u64
                    })
                    .sum::<u64>();
            }
        }
        Err(error) => {
            plan.estimated_export_bytes = None;
            if plan.blockers.is_empty() {
                plan.blockers.push(error.to_string());
            }
        }
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        println!("Contract: {}", plan.contract_ref);
        println!("Application input: {} bytes", plan.application_input_bytes);
        println!("Python wheels: {} bytes", plan.python_wheel_bytes);
        println!("OCI images: {}", plan.oci_images.len());
        println!("Embedded objects: {}", plan.embedded_objects);
        println!("External objects: {}", plan.external_objects);
        println!(
            "Embedded dependency bytes: {}",
            plan.embedded_dependency_bytes
        );
        println!(
            "Known external dependency bytes: {}",
            plan.external_dependency_bytes
        );
        match plan.estimated_export_bytes {
            Some(size) => println!("Estimated export: {size} bytes"),
            None => println!("Estimated export: unavailable until dependencies are resolved"),
        }
        println!(
            "Host capabilities: {}",
            plan.required_host_capabilities.join(", ")
        );
        println!(
            "Requires network on a clean host: {:?}",
            plan.requires_network_on_clean_host
        );
        for blocker in &plan.blockers {
            println!("Blocked: {blocker}");
        }
    }
    Ok(())
}

fn export_portable(args: ExportArgs) -> Result<()> {
    let bytes =
        fs::read(&args.capsule).with_context(|| format!("read {}", args.capsule.display()))?;
    let bundle = portable_export_bundle(&bytes)?;
    if args.output.exists() {
        bail!("export output already exists: {}", args.output.display());
    }
    if args.oci_archive.is_some() && args.portability != PortableExportProfile::Offline {
        bail!("--oci-archive is currently supported only for offline export");
    }
    let archives =
        portable_export_archives(&bundle, args.portability, args.oci_archive.as_deref())?;
    let source = portable_export_source(&bundle, args.portability)?;
    let (profile, sources) = match args.portability {
        PortableExportProfile::Thin => (
            PortableDependencyProfile::Thin,
            portable_dependency::discover_wheel_sources(&bundle)?,
        ),
        PortableExportProfile::Cached => (PortableDependencyProfile::Cached, BTreeMap::new()),
        PortableExportProfile::Offline => (PortableDependencyProfile::Offline, BTreeMap::new()),
    };
    let (output, repacked) =
        repack_portable_dependencies_with_archives(&source, profile, &sources, &archives)?;
    ato_local_execution::atomic_write(&args.output, &output)?;
    println!("file={}", args.output.display());
    println!("bundle_sha256={}", bundle_sha256(&output));
    println!("contract_ref={}", repacked.index.root_contract_ref);
    for reference in &repacked.index.derivations {
        println!("derivation_ref={reference}");
    }
    println!("bytes={}", output.len());
    Ok(())
}

fn portable_export_bundle(bytes: &[u8]) -> Result<PortableApplicationBundle> {
    match decode_capsule_bundle_document(bytes)? {
        CapsuleBundleDocument::PortableApplicationV3(bundle)
        | CapsuleBundleDocument::PortableApplicationV4(bundle) => Ok(bundle),
        CapsuleBundleDocument::ComputationV2(_) => {
            bail!("dependency export requires a portable application v3 or v4 bundle")
        }
    }
}

fn portable_export_source(
    bundle: &PortableApplicationBundle,
    profile: PortableExportProfile,
) -> Result<PortableApplicationBundle> {
    match profile {
        PortableExportProfile::Thin => Ok(bundle.clone()),
        PortableExportProfile::Cached | PortableExportProfile::Offline => {
            portable_dependency::hydrate_external_objects(bundle).map(|(hydrated, _)| hydrated)
        }
    }
}

fn portable_export_archives(
    bundle: &PortableApplicationBundle,
    profile: PortableExportProfile,
    archive_path: Option<&std::path::Path>,
) -> Result<Vec<PortableOciArchive>> {
    if profile != PortableExportProfile::Offline {
        return Ok(Vec::new());
    }
    match archive_path {
        Some(path) => portable_dependency::oci_archive_from_file(bundle, path),
        None => Ok(bundle
            .portability
            .as_ref()
            .map(|portability| portability.oci_archives.clone())
            .unwrap_or_default()),
    }
}

fn run_computation_bundle(args: RunArgs, bundle: CapsuleBundle) -> Result<()> {
    if args.no_open || args.verification_receipt.is_some() {
        bail!(
            "--no-open and --verification-receipt are available only for portable application v3 bundles"
        );
    }
    let cache = ato_home()?.join("cache");
    fs::create_dir_all(&cache)?;
    let runtime = tempfile::Builder::new()
        .prefix("portable-run-")
        .tempdir_in(cache)?;
    let project = runtime.path().join("workspace");
    fs::create_dir_all(&project)?;
    let repository = LocalCapsuleRepository::open(&project)?;
    let references = reference_registry()?;
    let root = import_bundle(&bundle, repository.objects(), &references)?;
    let state = load_runtime_state(&root, repository.objects())?;
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
    let adapters = adapter_registry()?;
    let materializers = materializer_registry()?;
    let actuator_providers = preflight_actuator_provider_registry()?;
    let contract_verifiers = contract_verifier_registry()?;
    let runner_capabilities = FirecrackerBackend::new(FirecrackerBackendConfig::default()).probe();
    let capture_policy = workspace_policy(&state.config)?;
    let driver = LocalRealizationDriver::new(&project, &bindings);
    let context = MaterializerContext {
        objects: repository.objects(),
        adapters: &adapters,
        records: &[],
        records_v2: &[],
        replay_anchor: None,
        record_frontier_ref: None,
        workspace: &project,
        workspace_policy: &capture_policy,
        realization: Some(&driver),
        contracts: &[],
        runner_capabilities: Some(&runner_capabilities),
    };
    let target_environment = TargetEnvironment {
        id: "local".to_owned(),
        placement: Placement::Local,
        trust_boundary: TrustBoundary::Local,
    };
    let candidates = bundle
        .index
        .materializations
        .iter()
        .map(|candidate| {
            Ok(MaterializationCandidate {
                materializer_id: candidate.materializer_id.clone(),
                descriptor_ref: ContentRef::parse(&candidate.descriptor_ref)?,
                environment: target_environment.clone(),
                context: MaterializerContext {
                    objects: context.objects,
                    adapters: context.adapters,
                    records: context.records,
                    records_v2: context.records_v2,
                    replay_anchor: context.replay_anchor,
                    record_frontier_ref: context.record_frontier_ref,
                    workspace: context.workspace,
                    workspace_policy: context.workspace_policy,
                    realization: context.realization,
                    contracts: context.contracts,
                    runner_capabilities: context.runner_capabilities,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let port_bindings = Vec::new();
    let policy = PlannerPolicy::default();
    let plan = RealizationPlanner {
        target: &root,
        materializers: &materializers,
        actuator_providers: &actuator_providers,
        contract_verifiers: &contract_verifiers,
        port_bindings: &port_bindings,
        policy: &policy,
    }
    .plan(candidates)
    .map_err(|error| anyhow::anyhow!("no acceptable Realization path: {error}"))?;
    let selected = plan
        .candidates
        .first()
        .context("Realization Planner returned no candidate")?;
    let materializer = materializers.get(&selected.materializer_id)?;
    let contracts = materializer.contracts(&selected.descriptor_ref, &context)?;
    let realization = materializer.restore(&selected.descriptor_ref, &context)?;
    if realization.target() != &root {
        bail!(
            "Materialization restored {}, expected bundle root {root}",
            realization.target()
        );
    }
    let contract_context = ContractContext {
        objects: repository.objects(),
        workspace: &project,
    };
    let accepted = accept_candidate(
        realization,
        &contracts,
        &contract_verifiers,
        &contract_context,
    )?;
    accepted.run().map_err(Into::into)
}

/// Where a Run's scenario transcript sidecars live beside its receipt.
fn scenario_sidecar_directory(receipt: &Path) -> PathBuf {
    let mut name = receipt
        .file_name()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "receipt.json".into());
    name.push(".scenarios");
    receipt.with_file_name(name)
}

/// Copy the transcript sidecars a Run wrote into the caller's evidence
/// directory. The runtime scratch is disposable, so a receipt kept without its
/// transcripts would name evidence nobody can read.
fn copy_scenario_sidecars(runtime_root: &Path, destination: &Path) -> Result<()> {
    let source = runtime_root.join("scenario-transcripts");
    if !source.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(destination).with_context(|| {
        format!(
            "create scenario transcript directory {}",
            destination.display()
        )
    })?;
    for entry in fs::read_dir(&source)
        .with_context(|| format!("read scenario transcript directory {}", source.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json")
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        fs::copy(&path, destination.join(name)).with_context(|| {
            format!(
                "copy scenario transcript {} to {}",
                path.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

/// The refusal a receipt states, including the verifier's typed code.
///
/// `"did not fully satisfy observation <id>"` alone says *which* requirement
/// failed but not *why*, even though the receipt already carries the deciding
/// code (`scenario_step_failed: <step>: <reason>`,
/// `input_identity_mismatch: ...`). Dropping it made the reform/transformation
/// report state less than the evidence it was built from and forced the reader
/// to re-run the candidate to learn the reason.
fn contract_refusal_message(receipt: &ContractVerificationReceipt) -> String {
    let Some(observation) = receipt.observations.iter().find(|observation| {
        !matches!(
            observation.outcome,
            ato_formation::verify::ReceiptOutcome::Satisfied
        )
    }) else {
        return "runtime Contract verification did not fully satisfy an unrecorded observation"
            .to_owned();
    };
    match observation
        .failure
        .as_deref()
        .filter(|failure| !failure.is_empty())
    {
        Some(failure) => format!(
            "runtime Contract verification did not fully satisfy observation {}: {failure}",
            observation.id
        ),
        None => format!(
            "runtime Contract verification did not fully satisfy observation {}",
            observation.id
        ),
    }
}

fn run_portable_application(
    args: RunArgs,
    bundle_bytes: &[u8],
    bundle: ato_objects::PortableApplicationBundle,
) -> Result<()> {
    #[cfg(unix)]
    let shutdown = Some({
        let flag = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&flag))?;
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&flag))?;
        flag
    });
    #[cfg(not(unix))]
    let shutdown: Option<Arc<AtomicBool>> = None;
    let bindings = runtime_binding_values(args.bindings)?;
    let selected_derivation = match args.derivation.as_deref() {
        Some(reference) => reference.to_owned(),
        None if bundle.index.derivations.len() == 1 => bundle.index.derivations[0].clone(),
        None => bail!(
            "portable Capsule declares {} derivations; select one with --derivation <sha256:...>",
            bundle.index.derivations.len()
        ),
    };
    let cache = ato_home()?.join("cache");
    fs::create_dir_all(&cache)?;
    let runtime = tempfile::Builder::new()
        .prefix("ato-portable-run-")
        .tempdir_in(cache)?;
    let runtime_root = runtime.keep();
    // A Run that fails verification publishes nothing: `--verification-receipt`
    // names the receipt of a Run that was accepted. The attempt record (and,
    // for a scenario, the transcript under the runtime scratch) is the failure
    // evidence; writing a receipt here would make a refused Run look published.
    let started = start_and_verify_portable_application(
        bundle_bytes,
        bundle,
        &selected_derivation,
        &runtime_root,
        shutdown.as_deref(),
        PortableRuntimeState {
            bindings: Some(&bindings),
            ..PortableRuntimeState::default()
        },
    );
    // A Run that fails verification publishes nothing, exactly as it did
    // before the Formation Generalization line was integrated: the attempt
    // record (and, for a scenario, the transcript under the runtime scratch)
    // is the failure evidence. Copying a receipt persisted by the common
    // attempt into `--verification-receipt` made a refused Run look published
    // and broke `scenario_verification`'s failure contract.
    let started = started?;
    let mut runtime = started.runtime;
    let receipt = started.receipt;
    let result = (|| -> Result<()> {
        if let Some(path) = &args.verification_receipt {
            fs::write(path, receipt.canonical_bytes()?)
                .with_context(|| format!("write verification receipt {}", path.display()))?;
            copy_scenario_sidecars(&runtime_root, &scenario_sidecar_directory(path))?;
        }
        println!("Capsule: {}", receipt.contract_ref);
        println!("Route: {}", receipt.derivation_ref);
        println!("Runtime: {}", runtime.label());
        if let Some(bundle) = &receipt.bundle_sha256 {
            println!("Bundle: {bundle}");
        }
        println!("URL: {}", runtime.base_url());
        if !receipt.fully_satisfied {
            bail!("{}", contract_refusal_message(&receipt));
        }
        if args.no_open {
            return Ok(());
        }
        open_browser(runtime.base_url())?;
        println!("Press Ctrl-C to stop the local realization.");
        loop {
            if shutdown
                .as_deref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                return Ok(());
            }
            // A service group is one Application: when any service exits, the
            // whole group must be explicitly stopped, even when polling fails.
            if runtime.stops_when_a_service_exits {
                runtime.try_wait()?;
            }
            #[cfg(not(unix))]
            std::thread::park_timeout(Duration::from_millis(500));
            #[cfg(unix)]
            std::thread::sleep(Duration::from_millis(100));
        }
        #[allow(unreachable_code)]
        Ok(())
    })();
    let stopped = runtime.stop_recorded(Some(&runtime_root));
    match (result, stopped) {
        (Err(original), Err(cleanup)) => Err(cleanup.context(format!("Run failed: {original:#}"))),
        (Err(original), Ok(())) => Err(original),
        (Ok(()), stopped) => stopped,
    }
}

/// One HTTP observation, reduced to the facts a transformation report may
/// repeat. Full evidence stays in the receipt.
#[derive(Debug, serde::Serialize)]
struct TransformationObservationV1 {
    id: String,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_sha256: Option<String>,
}

/// What the receipt's existing execution evidence says about the runtime.
#[derive(Debug, serde::Serialize)]
struct TransformationRuntimeV1 {
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_executable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_version: Option<String>,
}

/// Sidecar report for one transformation candidate. This schema is ours; the
/// Contract verification receipt keeps its existing schema and authority.
#[derive(Debug, serde::Serialize)]
struct TransformationReportV1 {
    schema: &'static str,
    /// `candidate` (K preserved, not yet run), `accepted` (fully satisfied) or
    /// `rejected` (K mismatch/source-bound/verification failure).
    status: &'static str,
    frozen_bundle_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    frozen_contract_ref: Option<String>,
    old_derivation_refs: Vec<String>,
    old_source_closure_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_bundle_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_derivation_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_source_closure_ref: Option<String>,
    k_preserved: bool,
    fully_satisfied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_versions: Option<TransformationRuntimeV1>,
    observations: Vec<TransformationObservationV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    started_at_unix_ms: u64,
    finished_at_unix_ms: u64,
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn transformation_observations(
    receipt: &ContractVerificationReceipt,
) -> Vec<TransformationObservationV1> {
    receipt
        .observations
        .iter()
        .map(|observation| TransformationObservationV1 {
            id: observation.id.clone(),
            outcome: match observation.outcome {
                ato_formation::verify::ReceiptOutcome::Satisfied => "satisfied",
                ato_formation::verify::ReceiptOutcome::Deferred => "deferred",
                ato_formation::verify::ReceiptOutcome::Failed => "failed",
            },
            status: observation
                .evidence
                .as_ref()
                .and_then(|evidence| evidence.status),
            body_sha256: observation
                .evidence
                .as_ref()
                .and_then(|evidence| evidence.body_sha256.clone()),
        })
        .collect()
}

fn write_transformation_report(out: &Path, report: &TransformationReportV1) -> Result<()> {
    let path = out.join("report.json");
    fs::write(&path, serde_json::to_vec_pretty(report)?)
        .with_context(|| format!("write transformation report {}", path.display()))
}

/// `ato reform <existing.capsule> <new-source> --out <dir>`: build a candidate
/// Derivation from a NEW source under the OLD bundle's frozen Contract, then
/// run it through the existing portable attempt so the existing verifier is the
/// only judge of K. Nothing is published, activated or selected implicitly.
fn run_reform(args: ReformArgs) -> Result<()> {
    use ato_portable_application::transformation::{
        TRANSFORMATION_REPORT_SCHEMA, build_transformation_candidate,
        inspect_frozen_transform_target,
    };

    if args.out.exists() {
        bail!(
            "reform output already exists: {} (refusing to overwrite a candidate directory)",
            args.out.display()
        );
    }
    let frozen_bytes = fs::read(&args.capsule)
        .with_context(|| format!("read frozen Capsule {}", args.capsule.display()))?;
    let frozen_transport = bundle_sha256(&frozen_bytes);
    fs::create_dir_all(&args.out)
        .with_context(|| format!("create reform output {}", args.out.display()))?;
    let started_at_unix_ms = unix_millis();

    // The frozen bundle is inspected separately so a refusal can still be
    // reported with the old K/D identity. Inspection never executes anything.
    let frozen = inspect_frozen_transform_target(&frozen_bytes).ok();
    let candidate = match build_transformation_candidate(
        &frozen_bytes,
        &args.source,
        args.derivation.as_deref(),
    ) {
        Ok(candidate) => candidate,
        Err(error) => {
            // K mismatch / source-bound / selection refusals: no
            // candidate.capsule is written and nothing is executed.
            let report = TransformationReportV1 {
                schema: TRANSFORMATION_REPORT_SCHEMA,
                status: "rejected",
                frozen_bundle_sha256: frozen_transport,
                frozen_contract_ref: frozen.as_ref().map(|f| f.contract_ref.clone()),
                old_derivation_refs: frozen
                    .as_ref()
                    .map(|f| f.derivation_refs.clone())
                    .unwrap_or_default(),
                old_source_closure_refs: frozen
                    .as_ref()
                    .map(|f| f.source_closure_refs.clone())
                    .unwrap_or_default(),
                new_bundle_sha256: None,
                new_derivation_ref: None,
                new_source_closure_ref: None,
                k_preserved: false,
                fully_satisfied: false,
                receipt_ref: None,
                runtime_versions: None,
                observations: Vec::new(),
                error: Some(error.to_string()),
                started_at_unix_ms,
                finished_at_unix_ms: unix_millis(),
            };
            write_transformation_report(&args.out, &report)?;
            return Err(error.into());
        }
    };

    // K is preserved (build_transformation_candidate proved it); only now does
    // a candidate artifact exist.
    let candidate_path = args.out.join("candidate.capsule");
    fs::write(&candidate_path, &candidate.bytes)
        .with_context(|| format!("write candidate capsule {}", candidate_path.display()))?;
    let candidate_transport = bundle_sha256(&candidate.bytes);

    // The report can be read while the candidate is still running: write the
    // pre-execution state first (K preserved, not yet run) and replace it with
    // the final verdict below. No receipt exists at this point.
    write_transformation_report(
        &args.out,
        &TransformationReportV1 {
            schema: TRANSFORMATION_REPORT_SCHEMA,
            status: "candidate",
            frozen_bundle_sha256: frozen_transport.clone(),
            frozen_contract_ref: Some(candidate.contract_ref.clone()),
            old_derivation_refs: candidate.old_derivation_refs.clone(),
            old_source_closure_refs: candidate.old_source_closure_refs.clone(),
            new_bundle_sha256: Some(candidate_transport.clone()),
            new_derivation_ref: Some(candidate.derivation_ref.clone()),
            new_source_closure_ref: Some(candidate.source_closure_ref.clone()),
            k_preserved: candidate.k_preserved,
            fully_satisfied: false,
            receipt_ref: None,
            runtime_versions: None,
            observations: Vec::new(),
            error: None,
            started_at_unix_ms,
            finished_at_unix_ms: unix_millis(),
        },
    )?;

    // The candidate is an ordinary portable bundle; the existing attempt and
    // verifier decide K from real observations.
    let runtime_dir = tempfile::Builder::new()
        .prefix("ato-reform-")
        .tempdir_in(&args.out)
        .with_context(|| format!("create reform runtime directory in {}", args.out.display()))?;
    let runtime_root = runtime_dir.path().to_path_buf();
    let bindings = BTreeMap::new();
    let attempt = start_and_verify_portable_application(
        &candidate.bytes,
        candidate.bundle.clone(),
        &candidate.derivation_ref,
        &runtime_root,
        None,
        PortableRuntimeState {
            bindings: Some(&bindings),
            ..PortableRuntimeState::default()
        },
    );

    // The candidate directory survives the reform runtime; keep the transcript
    // sidecars there before the tempdir is dropped.
    if let Err(error) =
        copy_scenario_sidecars(&runtime_root, &args.out.join("scenario-transcripts"))
    {
        eprintln!("[reform] could not keep scenario transcripts: {error:#}");
    }
    let mut receipt_bytes: Option<Vec<u8>> = None;
    let mut receipt: Option<ContractVerificationReceipt> = None;
    let mut run_error: Option<String> = None;
    match attempt {
        Ok(started) => {
            let runtime = started.runtime;
            match started.receipt.canonical_bytes() {
                Ok(bytes) => {
                    receipt_bytes = Some(bytes);
                    receipt = Some(started.receipt);
                }
                Err(error) => run_error = Some(format!("{error:#}")),
            }
            let stopped = runtime.stop_recorded(Some(&runtime_root));
            if let Err(error) = stopped {
                run_error = Some(match run_error {
                    Some(previous) => format!("{previous}; stop failed: {error:#}"),
                    None => format!("stop failed: {error:#}"),
                });
            }
        }
        Err(error) => {
            run_error = Some(format!("{error:#}"));
            // A verification failure still leaves the canonical receipt the
            // attempt wrote before refusing the hand-off; keep it as evidence.
            let path = runtime_root.join("verification-receipt.json");
            if let Ok(bytes) = fs::read(&path) {
                receipt = serde_json::from_slice(&bytes).ok();
                receipt_bytes = Some(bytes);
            }
        }
    }
    drop(runtime_dir);

    if let Some(bytes) = &receipt_bytes {
        let path = args.out.join("receipt.json");
        fs::write(&path, bytes)
            .with_context(|| format!("write candidate receipt {}", path.display()))?;
    }
    let fully_satisfied = receipt
        .as_ref()
        .is_some_and(|receipt| receipt.fully_satisfied);
    let accepted = fully_satisfied && run_error.is_none();
    let report = TransformationReportV1 {
        schema: TRANSFORMATION_REPORT_SCHEMA,
        status: if accepted { "accepted" } else { "rejected" },
        frozen_bundle_sha256: frozen_transport,
        frozen_contract_ref: Some(candidate.contract_ref.clone()),
        old_derivation_refs: candidate.old_derivation_refs.clone(),
        old_source_closure_refs: candidate.old_source_closure_refs.clone(),
        new_bundle_sha256: Some(candidate_transport),
        new_derivation_ref: Some(candidate.derivation_ref.clone()),
        new_source_closure_ref: Some(candidate.source_closure_ref.clone()),
        k_preserved: candidate.k_preserved,
        fully_satisfied,
        receipt_ref: receipt_bytes.as_deref().map(bundle_sha256),
        runtime_versions: receipt.as_ref().and_then(|receipt| {
            receipt
                .execution
                .as_ref()
                .map(|execution| TransformationRuntimeV1 {
                    runtime_executable: execution.runtime_executable.clone(),
                    runtime_version: execution.runtime_version.clone(),
                })
        }),
        observations: receipt
            .as_ref()
            .map(transformation_observations)
            .unwrap_or_default(),
        error: run_error.clone(),
        started_at_unix_ms,
        finished_at_unix_ms: unix_millis(),
    };
    write_transformation_report(&args.out, &report)?;

    println!("Frozen K: {}", candidate.contract_ref);
    println!("Candidate D: {}", candidate.derivation_ref);
    println!("New source closure: {}", candidate.source_closure_ref);
    println!(
        "Status: {} (fully_satisfied: {fully_satisfied})",
        report.status
    );
    println!("Candidate: {}", candidate_path.display());
    if receipt_bytes.is_some() {
        println!("Receipt: {}", args.out.join("receipt.json").display());
    }
    println!("Report: {}", args.out.join("report.json").display());

    if !accepted {
        if let Some(error) = run_error {
            bail!("candidate verification did not pass: {error}");
        }
        bail!("candidate verification did not fully satisfy the frozen Contract");
    }
    Ok(())
}

struct StartedPortableApplication {
    runtime: PortableLocalRuntime,
    receipt: ContractVerificationReceipt,
}

/// A LocalProcess or StaticWeb route, through the Runtime's common attempt:
/// admission, the start record, the HTTP observation, K's verdicts and the
/// receipt are the Runtime's; this only supplies what to run and keeps the
/// verified candidate that is handed off.
fn start_portable_attempt(
    bundle_bytes: &[u8],
    bundle: &ato_objects::PortableApplicationBundle,
    validated: &ValidatedPortableApplication,
    runtime_root: &Path,
    shutdown: Option<&AtomicBool>,
    runtime_state: PortableRuntimeState<'_>,
    binding_environment: &BTreeMap<String, String>,
) -> Result<StartedPortableApplication> {
    use ato_runtime_attempt::admission::EffectAuthorization;
    use ato_runtime_attempt::attempt::{AttemptRequest, Continuation, ReceiptContext, run_attempt};
    use ato_runtime_attempt::build_sandbox::NetworkPolicy;
    use ato_runtime_attempt::journal::AttemptJournal;

    // What this host, not the bundle, decides: where declared external
    // objects are cached, and which executable applies a portable sandbox
    // policy. The executor is shared with the hosted Formation worker, so
    // these no longer come from the CLI's own globals.
    let cache_root = ato_home()?.join("cache");
    let sandbox_shim = std::env::current_exe()
        .context("locate ato sandbox shim")?
        .canonicalize()
        .context("canonicalize ato sandbox shim")?;
    let portable_run_context = ato_portable_application::portable_run::PortableRunContext {
        cache_root: &cache_root,
        sandbox_shim: &sandbox_shim,
        sandbox_subcommand: "__portable-sandbox-exec",
    };
    let spec = validated.attempt_spec(runtime_state.restored_snapshot_ref);
    // A local Instance's Run is the attempt; `ato run` starts a new one.
    let fresh = format!(
        "run-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    );
    let attempt_id = runtime_state.run_id.unwrap_or(&fresh);
    let transport = bundle_sha256(bundle_bytes);
    let portability_profile =
        bundle
            .portability
            .as_ref()
            .map(|portability| match portability.profile {
                PortableDependencyProfile::Thin => "thin",
                PortableDependencyProfile::Cached => "cached",
                PortableDependencyProfile::Offline => "offline",
            });
    let outcome = run_attempt(
        &AttemptRequest {
            request_id: attempt_id,
            attempt_id,
            label: "portable",
            spec: &spec,
            contract_ref: validated.contract_ref.as_str(),
            runtime_id: "local",
            profile: &ato_formation::request::RuntimeProfile::default(),
            // The person ran exactly this Derivation. That authorizes running
            // it; it confirms no effect beyond the disposable ones, and the
            // validator admits only pure single-step routes here.
            authorization: EffectAuthorization::UserInvoked {
                derivation_ref: validated.derivation_ref.as_str(),
            },
            // A Run builds nothing.
            network: NetworkPolicy::Denied,
            browser: None,
            attempt_root: runtime_root,
            continuation: Continuation::HandOff,
            receipt: ReceiptContext {
                target: VerificationTargetKind::CliLocal,
                bundle_sha256: Some(&transport),
                portability_profile,
                run_id: runtime_state.run_id,
            },
            interrupt: shutdown,
            control: None,
        },
        &ato_portable_application::portable_run::PortableBundleExecutor {
            bundle,
            validated,
            runtime_root,
            static_state: runtime_state.static_state,
            filesystem_state: runtime_state.filesystem_state,
            binding_environment,
            context: &portable_run_context,
        },
        &AttemptJournal::new(ato_home()?.join("attempt-records")),
    );
    if let Some(receipt) = &outcome.attempt.receipt {
        let saved = (|| -> Result<()> {
            ato_local_execution::atomic_write(
                &runtime_root.join("verification-receipt.json"),
                &receipt.canonical_bytes()?,
            )?;
            Ok(())
        })();
        if let Err(original) = saved {
            if let Some(live) = outcome.live {
                let stopped = live.stop();
                let _ = record_stop(Some(runtime_root), &stopped);
                if ato_runtime_attempt::realize::StopClass::of(&stopped).is_confirmed() {
                    clear_stop_pending(runtime_root)?;
                }
                return match stopped {
                    Err(cleanup) => {
                        Err(cleanup.context(format!("receipt persistence failed: {original:#}")))
                    }
                    Ok(()) => Err(original),
                };
            }
            return Err(original);
        }
    }
    if let Some(class) = &outcome.stop {
        use ato_runtime_attempt::realize::{CandidateStopFailure, StopClass};
        let stopped = match class {
            StopClass::Confirmed => Ok(()),
            StopClass::ScratchKept { reason } => {
                Err(anyhow::Error::new(CandidateStopFailure::ScratchKept {
                    reason: reason.clone(),
                }))
            }
            StopClass::Unconfirmed { reason, resources } => {
                Err(anyhow::Error::new(CandidateStopFailure::Unconfirmed {
                    reason: reason.clone(),
                    resources: resources.clone(),
                }))
            }
        };
        record_stop(Some(runtime_root), &stopped)?;
        if class.is_confirmed() {
            clear_stop_pending(runtime_root)?;
        } else {
            return Err(stopped.unwrap_err());
        }
    } else if outcome.live.is_none() && !outcome.attempt_record.execution_started() {
        clear_stop_pending(runtime_root)?;
    }
    let Some(live) = outcome.live else {
        // No candidate is handed off. The recorded stop class above, not
        // this absence, determines whether effects and scratch were reclaimed.
        if let Some(receipt) = &outcome.attempt.receipt
            && !receipt.fully_satisfied
        {
            bail!("{}", contract_refusal_message(receipt));
        }
        return Err(outcome.error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "the Run did not start: {}",
                outcome
                    .attempt
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.as_str())
                    .unwrap_or("no reason recorded")
            )
        }));
    };
    let receipt = outcome
        .attempt
        .receipt
        .context("a verified Run carries its receipt")?;
    let label = match validated.realization {
        PortableRealizationKind::StaticWeb => "static web",
        PortableRealizationKind::LocalProcess => "local process",
        PortableRealizationKind::OciContainer => "OCI container",
        PortableRealizationKind::OciServiceGroup => "OCI service group",
    };
    let base_url = live
        .endpoint()
        .map(|endpoint| endpoint.trim_end_matches('/').to_owned())
        .context("the verified candidate answers nowhere")?;
    Ok(StartedPortableApplication {
        runtime: PortableLocalRuntime {
            live,
            base_url,
            label,
            stops_when_a_service_exits: validated.realization
                == PortableRealizationKind::OciServiceGroup,
        },
        receipt,
    })
}

#[derive(Default)]
struct PortableRuntimeState<'a> {
    restored_snapshot_ref: Option<&'a str>,
    static_state: Option<StaticApplicationState>,
    filesystem_state: Option<&'a BTreeMap<String, PathBuf>>,
    bindings: Option<&'a BTreeMap<String, String>>,
    /// The Run this start belongs to, when the caller keeps one (a local
    /// Instance). The receipt names it from the start.
    run_id: Option<&'a str>,
}

fn start_and_verify_portable_application(
    bundle_bytes: &[u8],
    bundle: ato_objects::PortableApplicationBundle,
    selected_derivation: &str,
    runtime_root: &Path,
    shutdown: Option<&AtomicBool>,
    runtime_state: PortableRuntimeState<'_>,
) -> Result<StartedPortableApplication> {
    let validated = validate_bundle_for_derivation(&bundle, selected_derivation)?;
    let empty_bindings = BTreeMap::new();
    let binding_environment = resolve_application_bindings(
        &validated.application,
        runtime_state.bindings.unwrap_or(&empty_bindings),
    )?;
    start_portable_attempt(
        bundle_bytes,
        &bundle,
        &validated,
        runtime_root,
        shutdown,
        runtime_state,
        &binding_environment,
    )
}

/// A Run the common attempt verified and handed off. Dropping it stops it
/// and removes the runtime scratch its realization owns.
struct PortableLocalRuntime {
    live: ato_runtime_attempt::realize::LiveCandidate,
    base_url: String,
    label: &'static str,
    /// A service group is one Application: any service exiting ends the Run.
    stops_when_a_service_exits: bool,
}

impl PortableLocalRuntime {
    /// Stop the Run and record how that went — the Run owner's lifecycle
    /// record, separate from the attempt, whose cleanup stays
    /// `not_attempted (handed_off)`. Written beside the Run when it has a
    /// directory of its own (a local Instance), reported otherwise.
    fn stop_recorded(self, run_root: Option<&Path>) -> Result<()> {
        let stopped = self.live.stop();
        let recorded = record_stop(run_root, &stopped);
        // Preserve cessation uncertainty even when persisting the record fails.
        stopped.and(recorded)
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn local_storage(&self) -> Result<Option<BTreeMap<String, String>>> {
        match self
            .live
            .downcast_ref::<ato_portable_application::portable_run::PortableStaticCandidate>()
        {
            Some(candidate) => candidate.local_storage(),
            None => Ok(None),
        }
    }

    fn label(&self) -> &'static str {
        self.label
    }

    fn try_wait(&mut self) -> Result<()> {
        match self.live.exited()? {
            Some(exit) => bail!("{exit}; the Run was stopped"),
            None => Ok(()),
        }
    }
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    command
        .arg(url)
        .spawn()
        .context("open the application URL")?;
    Ok(())
}

/// The CLI's materializer set, injected into `ato-local-execution`.
///
/// This is the composition root's answer to "what can this host realize". The
/// library never builds this itself: VM Snapshot is an Ato SEMANTIC capability
/// whose physical backend is platform-specific (Firecracker on Linux today),
/// and welding it into the library would force every consumer — including a
/// Desktop runtime that only realizes source/replay — to link a hypervisor.
///
/// The CLI's answer is unchanged from before the extraction, so its behaviour
/// is too.
pub(crate) fn cli_materializers() -> Result<MaterializerRegistry> {
    materializer_registry()
}

fn materializer_registry() -> Result<MaterializerRegistry> {
    // The shared core, plus the one materializer whose backend this host
    // actually has. Identical contents to before the split — Replay,
    // ReplayV2, Snapshot, WorkspaceSnapshot, VmSnapshot — just no longer
    // duplicated with the library.
    let mut registry = ato_local_execution::core_materializer_registry()?;
    registry.register(Arc::new(VmSnapshotMaterializer::new(
        Arc::new(FirecrackerBackend::new(FirecrackerBackendConfig::default())),
        Arc::new(RecordWriterFrontierVerifier),
    )))?;
    Ok(registry)
}

struct RecordWriterFrontierVerifier;

/// Application-layer bridge between the independently layered Record Writer
/// and VM Materializer capture capability.
pub struct RecordWriterCaptureBarrier {
    inner: CaptureBarrier,
}

impl RecordWriterCaptureBarrier {
    pub fn new(inner: CaptureBarrier) -> Self {
        Self { inner }
    }
}

struct RecordWriterCaptureLease {
    frontier: ContentRef,
    _paused: PausedCapture,
}

impl FirecrackerRecordCaptureLease for RecordWriterCaptureLease {
    fn frontier_ref(&self) -> &ContentRef {
        &self.frontier
    }
}

impl FirecrackerRecordCaptureBarrier for RecordWriterCaptureBarrier {
    fn pause_and_seal(
        &self,
    ) -> std::result::Result<Box<dyn FirecrackerRecordCaptureLease>, VmSnapshotError> {
        let paused = self
            .inner
            .pause_and_seal()
            .map_err(|error| VmSnapshotError::Backend(error.to_string()))?;
        Ok(Box::new(RecordWriterCaptureLease {
            frontier: paused.frontier.frontier_digest.clone(),
            _paused: paused,
        }))
    }
}

impl SealedRecordFrontierVerifier for RecordWriterFrontierVerifier {
    fn verify(
        &self,
        reference: &ContentRef,
        objects: &dyn ato_objects::ObjectResolver,
    ) -> std::result::Result<(), VmSnapshotError> {
        verify_frontier_object(reference, objects)
            .map(|_| ())
            .map_err(|error| VmSnapshotError::InvalidDescriptor(error.to_string()))
    }
}

fn reference_registry() -> Result<ReferenceRegistry> {
    standard_reference_registry()
}

fn preflight(
    repository: &LocalCapsuleRepository,
    config: &ato_local_execution::authoring::AuthoringConfig,
    bindings: &BTreeMap<String, String>,
) -> Result<()> {
    let registry = adapter_registry()?;
    let context = AdapterContext {
        workspace: repository.project(),
        objects: repository.objects(),
    };
    for instance in
        ato_local_execution::authoring::adapter_instances(config, bindings, false, false)?
    {
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

fn runtime_binding_values(bindings: Vec<(String, String)>) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for (id, value) in bindings {
        if values.insert(id.clone(), value).is_some() {
            bail!("runtime Binding `{id}` was supplied more than once");
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt_with(
        observations: Vec<ato_formation::verify::ReceiptObservation>,
    ) -> ContractVerificationReceipt {
        ContractVerificationReceipt {
            schema: ato_formation::verify::CONTRACT_VERIFICATION_RECEIPT_SCHEMA.to_owned(),
            bundle_sha256: None,
            contract_ref: format!("sha256:{}", "a".repeat(64)),
            derivation_ref: format!("sha256:{}", "b".repeat(64)),
            target: ato_formation::verify::VerificationTarget {
                kind: VerificationTargetKind::CliLocal,
            },
            execution: None,
            observations,
            fully_satisfied: false,
        }
    }

    fn observation(
        id: &str,
        outcome: ato_formation::verify::ReceiptOutcome,
        failure: Option<&str>,
    ) -> ato_formation::verify::ReceiptObservation {
        ato_formation::verify::ReceiptObservation {
            id: id.to_owned(),
            outcome,
            evidence: None,
            deferred_by: None,
            failure: failure.map(ToOwned::to_owned),
        }
    }

    /// A transformation report is built from the receipt; the verifier's typed
    /// code (`scenario_step_failed: step: reason`) is the reason the reader
    /// needs, and the generic sentence alone loses it.
    #[test]
    fn a_failed_observation_contributes_its_typed_code_to_the_refusal() {
        let receipt = receipt_with(vec![
            observation(
                "crud",
                ato_formation::verify::ReceiptOutcome::Failed,
                Some(
                    "scenario_step_failed: unauthorized-read: status_mismatch: status 200; expected 401",
                ),
            ),
            observation(
                "root",
                ato_formation::verify::ReceiptOutcome::Satisfied,
                None,
            ),
        ]);
        let message = contract_refusal_message(&receipt);
        assert!(
            message.contains("observation crud:"),
            "the failing observation is named: {message}"
        );
        assert!(
            message.contains(
                "scenario_step_failed: unauthorized-read: status_mismatch: status 200; expected 401"
            ),
            "the typed failure code travels with the refusal: {message}"
        );
        assert!(
            !message.contains("observation root"),
            "a satisfied observation is not the refusal: {message}"
        );
    }

    /// A deferred requirement has no failure code; the sentence must stay the
    /// same as before rather than growing an empty `: ` suffix.
    #[test]
    fn a_deferred_observation_keeps_the_generic_refusal() {
        let receipt = receipt_with(vec![observation(
            "crud",
            ato_formation::verify::ReceiptOutcome::Deferred,
            None,
        )]);
        assert_eq!(
            contract_refusal_message(&receipt),
            "runtime Contract verification did not fully satisfy observation crud"
        );
    }

    #[test]
    fn confirmed_stop_with_scratch_failure_releases_but_does_not_ack_success() {
        let home = tempfile::tempdir().unwrap();
        let store = LocalApplicationStore::open(home.path()).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
        let (bytes, _) =
            ato_portable_application::build_static_bundle(&fixture, "scratch failure").unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let run = store.claim_run(&instance.instance_id).unwrap();
        let root = store.run_root(&instance.instance_id, &run.run_id).unwrap();
        mark_stop_pending(&root).unwrap();
        let ack = root.join("stop.ack");
        let stopped = Err(
            ato_runtime_attempt::realize::CandidateStopFailure::ScratchKept {
                reason: "injected scratch removal failure".to_owned(),
            }
            .into(),
        );
        record_stop(Some(&root), &stopped).unwrap();
        assert!(finish_instance_stop(&store, &run, &root, stopped, None, Some(&ack)).is_err());
        assert_eq!(fs::read_to_string(ack).unwrap(), "scratch_cleanup_failed");
        assert!(store.active_run(&instance.instance_id).unwrap().is_none());
        assert!(store.claim_run(&instance.instance_id).is_ok());
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("lifecycle.json")).unwrap()).unwrap();
        assert_eq!(record["stop_confirmed"], true);
        assert_eq!(record["scratch_removed"], false);
    }

    #[test]
    fn explicit_stop_result_gates_state_ack_and_next_run() {
        use ato_runtime_attempt::realize::CandidateStopFailure;
        for confirmed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let store = LocalApplicationStore::open(home.path()).unwrap();
            let fixture =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
            let (bytes, _) =
                ato_portable_application::build_static_bundle(&fixture, "stop gate").unwrap();
            let instance = store.import(&bytes, None).unwrap();
            let run = store.claim_run(&instance.instance_id).unwrap();
            let root = store.run_root(&instance.instance_id, &run.run_id).unwrap();
            mark_stop_pending(&root).unwrap();
            // A dead worker alone cannot release a Run that may own OCI.
            assert!(
                store
                    .release_run(&instance.instance_id, &run.token)
                    .is_err()
            );
            assert!(store.claim_run(&instance.instance_id).is_err());
            let scratch = root.join("oci");
            fs::create_dir_all(&scratch).unwrap();
            let receipt = root.join("verification-receipt.json");
            let receipt_bytes = br#"{"fully_satisfied":true}"#;
            fs::write(&receipt, receipt_bytes).unwrap();
            let ack = root.join("stop.ack");
            let stopped = if confirmed {
                fs::remove_dir_all(&scratch).unwrap();
                Ok(())
            } else {
                Err(CandidateStopFailure::Unconfirmed {
                    reason: "injected OCI stop failure".to_owned(),
                    resources: vec!["container:fixture".to_owned()],
                }
                .into())
            };
            record_stop(Some(&root), &stopped).unwrap();
            let result = finish_instance_stop(&store, &run, &root, stopped, None, Some(&ack));
            assert_eq!(result.is_ok(), confirmed);
            assert_eq!(fs::read(&receipt).unwrap(), receipt_bytes);
            assert_eq!(scratch.exists(), !confirmed);
            let lifecycle: serde_json::Value =
                serde_json::from_slice(&fs::read(root.join("lifecycle.json")).unwrap()).unwrap();
            assert_eq!(lifecycle["stop_confirmed"], confirmed);
            if confirmed {
                assert_eq!(fs::read_to_string(&ack).unwrap(), "ok");
                assert!(store.active_run(&instance.instance_id).unwrap().is_none());
                assert!(store.claim_run(&instance.instance_id).is_ok());
            } else {
                assert!(!ack.exists());
                assert_eq!(
                    store
                        .active_run(&instance.instance_id)
                        .unwrap()
                        .unwrap()
                        .status,
                    LocalInstanceRunStatus::Quarantined
                );
                assert!(
                    store
                        .save_filesystem_state_for_run(&instance.instance_id, &run.token)
                        .is_err()
                );
                assert!(
                    store
                        .save_browser_state_for_run(
                            &instance.instance_id,
                            &run.token,
                            &BTreeMap::new()
                        )
                        .is_err()
                );
                assert!(
                    store
                        .release_run(&instance.instance_id, &run.token)
                        .is_err()
                );
                assert!(store.claim_run(&instance.instance_id).is_err());
            }
        }
    }

    #[test]
    fn public_cli_has_only_the_capsule_lifecycle() {
        for command in ["init", "resume", "stop", "encap", "run"] {
            assert!(Cli::try_parse_from(["ato", command, "value"]).is_ok());
        }
        for removed in ["lock", "decap", "snapshot"] {
            assert!(Cli::try_parse_from(["ato", removed]).is_err());
        }
        assert!(
            Cli::try_parse_from([
                "ato",
                "upload",
                "value",
                "--api-url",
                "https://staging.api.ato.run",
                "--auth-token",
                "redacted",
            ])
            .is_ok()
        );
    }

    #[test]
    fn login_supplies_the_upload_token_when_no_flag_is_given() {
        assert!(Cli::try_parse_from(["ato", "login"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "ato",
                "login",
                "--api-url",
                "https://staging.api.ato.run",
                "--headless",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["ato", "upload", "value", "--api-url", "https://api.ato.run"])
                .is_ok()
        );
    }

    #[test]
    fn formation_inspection_requires_a_paired_contract_and_derivation() {
        assert!(Cli::try_parse_from(["ato", "form-inspect", "."]).is_ok());
        assert!(
            Cli::try_parse_from(["ato", "form-inspect", ".", "--derivation", "d.json"]).is_err()
        );
        assert!(Cli::try_parse_from(["ato", "form-inspect", ".", "--contract", "k.json"]).is_err());
        assert!(
            Cli::try_parse_from([
                "ato",
                "form-inspect",
                ".",
                "--derivation",
                "d.json",
                "--contract",
                "k.json"
            ])
            .is_ok()
        );
    }

    #[test]
    fn formation_inspection_bounds_canonical_json_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inspection.json");
        fs::write(&path, "{\"schema\":\"example\"}").unwrap();
        assert_eq!(
            read_inspection_json::<serde_json::Value>(&path).unwrap()["schema"],
            "example"
        );
        fs::write(&path, vec![b' '; 1024 * 1024 + 1]).unwrap();
        assert!(
            read_inspection_json::<serde_json::Value>(&path)
                .unwrap_err()
                .to_string()
                .contains("exceeds 1 MiB")
        );
    }

    #[test]
    fn session_commands_require_a_scoped_connection_and_have_valid_help() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
        for operation in ["status", "next", "cancel"] {
            assert!(
                Cli::try_parse_from([
                    "ato",
                    "form-session",
                    "--connection",
                    "session.json",
                    operation,
                ])
                .is_ok()
            );
            assert!(Cli::try_parse_from(["ato", "form-session", operation]).is_err());
        }
        assert!(
            Cli::try_parse_from([
                "ato",
                "form-session",
                "--connection",
                "session.json",
                "submit",
                "--exchange-id",
                "search_r1_s1",
                "--input-sha256",
                "digest",
            ])
            .is_ok()
        );
    }

    #[test]
    fn hidden_machine_commands_do_not_appear_in_help() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("__worker"));
        assert!(!help.contains("__desktop"));
        for public in ["init", "resume", "stop", "encap", "run", "login"] {
            assert!(help.contains(public));
        }
    }

    #[test]
    fn durable_instance_export_is_a_separate_app_operation() {
        assert!(
            Cli::try_parse_from([
                "ato",
                "app",
                "export",
                "linst_example",
                "--output",
                "saved.capsule",
            ])
            .is_ok()
        );
    }

    #[test]
    fn run_rejects_repository_shaped_inputs_before_execution() {
        let args = RunArgs {
            capsule: PathBuf::from("."),
            derivation: None,
            bindings: Vec::new(),
            no_open: false,
            verification_receipt: None,
        };
        assert!(
            run_capsule(args)
                .unwrap_err()
                .to_string()
                .contains("portable .capsule")
        );
    }

    #[test]
    fn snapshot_id_uses_materializer_vocabulary() {
        assert_eq!(
            ato_materializer_snapshot::SNAPSHOT_MATERIALIZER_ID,
            "ato.snapshot@1"
        );
    }

    #[test]
    fn browser_record_schema_is_operation_specific() {
        let payload =
            ato_adapter_browser::encode_event(&ato_adapter_browser::BrowserEvent::Click {
                x_normalized: 0.5,
                y_normalized: 0.5,
                button: 0,
            })
            .unwrap();
        let candidate = |operation_id: &str| ato_objects::RecordCandidate {
            protocol_id: ato_computation::ProtocolId::parse(BROWSER_PROTOCOL_ID).unwrap(),
            operation_id: ato_computation::OperationId::parse(operation_id).unwrap(),
            port_id: ato_computation::PortId::parse("ui.main").unwrap(),
            payload: payload.clone(),
            payload_version: 1,
            required_features: Default::default(),
            recorded_by: Some("example.browser-adapter@1".to_owned()),
            stream: "browser.test".to_owned(),
            local_seq: 1,
            caused_by: Vec::new(),
            observed_at: "0".to_owned(),
        };
        let schemas = ato_local_execution::registry::record_schema_registry().unwrap();
        schemas
            .validate_candidate(&candidate(BROWSER_CLICK_OPERATION))
            .expect("click payload should match the click operation");
        assert!(
            schemas
                .validate_candidate(&candidate(BROWSER_KEYBOARD_OPERATION))
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
    }
}
