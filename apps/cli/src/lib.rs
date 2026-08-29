//! Product assembly for the Capsule lifecycle.

#![deny(unsafe_op_in_unsafe_fn)]

mod desktop_control;
mod object_transport;

pub mod activity_client;
pub mod activity_mcp;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_adapter_api::AdapterContext;
#[cfg(test)]
use ato_adapter_browser::{
    BROWSER_CLICK_OPERATION, BROWSER_KEYBOARD_OPERATION, BROWSER_PROTOCOL_ID,
};
use ato_adapter_workspace::restore_workspace;
use ato_computation::{ComputationRef, ContentRef};
use ato_materializer_api::{
    ContractContext, MaterializerContext, MaterializerRegistry, accept_candidate,
};
use ato_materializer_vm_snapshot::{
    FirecrackerBackend, FirecrackerBackendConfig, FirecrackerRecordCaptureBarrier,
    FirecrackerRecordCaptureLease, SealedRecordFrontierVerifier, VmSnapshotError,
    VmSnapshotMaterializer,
};
use ato_objects::{
    BranchOrigin, BundleMaterialization, CapsuleSelector, GraphMaterialization,
    GraphRestoreCapability, LocalCapsuleRepository, RecordId, ReferenceRegistry, decode_bundle,
    encode_bundle, export_bundle_with_materializations, export_object_graph, import_bundle,
    resolve_computation,
};
use ato_realization_planner::{
    MaterializationCandidate, Placement, PlannerPolicy, RealizationPlanner, TargetEnvironment,
    TrustBoundary,
};
use ato_record_writer::{
    CaptureBarrier, PausedCapture, load_frontier, records_for_frontier, verify_frontier_object,
};
use ato_runtime_object_graph::standard_reference_registry;
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
    /// Upload a content-addressed Capsule object graph.
    Upload(UploadArgs),
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
struct RunArgs {
    capsule: PathBuf,
    #[arg(long = "bind", value_parser = parse_binding)]
    bindings: Vec<(String, String)>,
}

#[derive(Debug, Args)]
struct UploadArgs {
    selector: String,
    #[arg(long = "materialize")]
    materializers: Vec<String>,
    #[arg(long, env = "ATO_API_URL")]
    api_url: String,
    #[arg(long, env = "ATO_API_TOKEN", hide_env_values = true)]
    auth_token: String,
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
    match Cli::parse().command {
        Commands::Version { json } => version(json),
        Commands::Init(args) => init(args),
        Commands::Resume(args) => resume(args),
        Commands::Stop { capsule } => stop(&capsule),
        Commands::Encap(args) => encap(args),
        Commands::Run(args) => run_capsule(args),
        Commands::Upload(args) => upload(args),
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
    }
}

fn desktop_inspect(project: &str) -> Result<()> {
    let path = project_path(project, false)?;
    let view = desktop_control::inspect(&path)?;
    println!("{}", serde_json::to_string(&view)?);
    Ok(())
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
    let api = HttpObjectTransportApi::new(&args.api_url, args.auth_token)?;
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
    let cache = ato_home()?.join("cache");
    fs::create_dir_all(&cache)?;
    let runtime = tempfile::Builder::new()
        .prefix("portable-run-")
        .tempdir_in(cache)?;
    let project = runtime.path().join("workspace");
    fs::create_dir_all(&project)?;
    let repository = LocalCapsuleRepository::open(&project)?;
    let bundle = decode_bundle(&fs::read(&args.capsule)?)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn hidden_machine_commands_do_not_appear_in_help() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("__worker"));
        assert!(!help.contains("__desktop"));
        for public in ["init", "resume", "stop", "encap", "run"] {
            assert!(help.contains(public));
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
