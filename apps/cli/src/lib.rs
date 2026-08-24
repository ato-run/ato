//! Product assembly for the Capsule lifecycle.

#![deny(unsafe_op_in_unsafe_fn)]

mod authoring;
mod object_transport;
mod supervisor;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_adapter_api::{
    ADAPTER_ADD_OPERATION, ADAPTER_CONFIGURE_OPERATION, ADAPTER_PROTOCOL_ID,
    ADAPTER_REMOVE_OPERATION, AdapterContext, AdapterControlPayload, AdapterRegistry,
    SupportedOperation, decode_adapter_control_payload,
};
use ato_adapter_binding::{
    BINDING_ATTACH_OPERATION, BINDING_DETACH_OPERATION, BINDING_PROTOCOL_ID,
    BINDING_REPLACE_OPERATION, BindingAdapter, BindingEvent, decode_event as decode_binding_event,
};
use ato_adapter_browser::{
    BrowserAdapter, register_record_schemas as register_browser_record_schemas,
};
use ato_adapter_http::{
    HTTP_PROTOCOL_ID, HTTP_REQUEST_OPERATION, HttpAdapter, HttpEvent,
    decode_event as decode_http_event,
};
use ato_adapter_process::ProcessLifecycleAdapter;
use ato_adapter_pty::{
    PTY_INPUT_OPERATION, PTY_PROTOCOL_ID, PTY_RESIZE_OPERATION, PTY_SIGNAL_OPERATION, PtyAdapter,
    PtyEvent, decode_event as decode_pty_event,
};
use ato_adapter_workspace::{
    WORKSPACE_DELETE_OPERATION, WORKSPACE_PROTOCOL_ID, WORKSPACE_PUT_OPERATION,
    WORKSPACE_RENAME_OPERATION, WorkspaceAdapter, WorkspaceMutation, decode_mutation,
    restore_workspace,
};
use ato_computation::{ComputationRef, ContentRef};
use ato_contracts::{HttpEndpointVerifier, WorkspaceContentVerifier};
use ato_materializer_api::{
    ContractContext, ContractVerifierRegistry, MaterializerContext, MaterializerRegistry,
    accept_candidate,
};
use ato_materializer_replay::{ReplayMaterializer, ReplayMaterializerV2};
use ato_materializer_snapshot::{SnapshotMaterializer, WorkspaceSnapshotMaterializer};
use ato_materializer_vm_snapshot::{
    FirecrackerBackend, FirecrackerBackendConfig, FirecrackerRecordCaptureBarrier,
    FirecrackerRecordCaptureLease, SealedRecordFrontierVerifier, VmSnapshotError,
    VmSnapshotMaterializer,
};
use ato_objects::{
    BranchOrigin, BundleMaterialization, CapsuleSelector, GraphMaterialization,
    GraphRestoreCapability, LocalCapsuleRepository, RecordId, ReferenceRegistry, decode_bundle,
    encode_bundle, export_bundle_with_materializations, export_object_graph, import_bundle,
};
use ato_realization_planner::{
    MaterializationCandidate, Placement, PlannerPolicy, RealizationPlanner, TargetEnvironment,
    TrustBoundary,
};
use ato_record_writer::RecordSchemaRegistry;
use ato_record_writer::{
    CaptureBarrier, PausedCapture, load_frontier, records_for_frontier, verify_frontier_object,
};
use ato_runtime_object_graph::standard_reference_registry;
use clap::{Args, Parser, Subcommand};

use crate::authoring::{
    evolve_workspace, initial_computation, load_config, load_runtime_state, workspace_policy,
};
pub use crate::object_transport::{
    ExportedPort, HttpObjectTransportApi, ObjectGraphIndexV1, ObjectUploadReceipt, RequiredBinding,
    UploadConfig, VisibilityPolicy, upload_http_object_graph,
    upload_staging_negative_test_object_graph, vm_capture_receipt_refs,
};
use crate::supervisor::{
    CliRealizationDriver, preflight_actuator_provider_registry, start_durable, stop_active,
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
    #[command(name = "__worker", hide = true)]
    Worker {
        project: PathBuf,
        branch: String,
        head: String,
        token: String,
        descriptor: Option<String>,
    },
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

pub fn run() -> Result<()> {
    match Cli::parse().command {
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
        } => supervisor::worker(
            &project,
            &branch,
            &ComputationRef::parse(head)?,
            &token,
            descriptor.map(ContentRef::parse).transpose()?.as_ref(),
        ),
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
    seal_run_record_frontier(&repository, &stopped, &head)?;
    repository.update_head(&stopped.branch, Some(&stopped.branch_base), &head)?;
    repository.release_active_run(&stopped.token)?;
    println!("sealed {} at {head}", stopped.branch);
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
    atomic_write(&args.output, &encode_bundle(&bundle)?)?;
    println!("{target}");
    Ok(())
}

fn encode_materializations(
    repository: &LocalCapsuleRepository,
    selector: &CapsuleSelector,
    target: &ComputationRef,
    state: &authoring::AuthoringState,
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
    let exported_ports = state
        .config
        .port
        .iter()
        .filter(|port| !port.internal)
        .map(|port| ExportedPort {
            port_id: port.id.clone(),
            protocol: port.protocol.clone(),
            role: port.role.clone(),
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
    atomic_write(&args.receipt, &serde_jcs::to_vec(&receipt)?)?;
    println!("{} {}", receipt.bundle_id, receipt.root_computation_ref);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedRunRecordFrontier {
    version: u32,
    run_id: String,
    branch: String,
    anchor_computation_ref: String,
    target_computation_ref: String,
    record_frontier_ref: String,
}

fn seal_run_record_frontier(
    repository: &LocalCapsuleRepository,
    run: &ato_objects::ActiveRun,
    target: &ComputationRef,
) -> Result<()> {
    let frontier_ref_path = repository
        .root()
        .join("runs")
        .join(format!("{}.record-frontier", run.token));
    let record_frontier_ref = fs::read_to_string(&frontier_ref_path).with_context(|| {
        format!(
            "missing Capture Barrier receipt at {}",
            frontier_ref_path.display()
        )
    })?;
    let record_frontier_ref = ContentRef::parse(record_frontier_ref.trim())?;
    let frontier = load_frontier(
        &repository.root().join("records"),
        &run.token,
        &record_frontier_ref,
    )?;
    if frontier.frontier_digest != record_frontier_ref {
        bail!("Capture Barrier returned a different RecordFrontier identity");
    }
    let association = SealedRunRecordFrontier {
        version: 1,
        run_id: run.token.clone(),
        branch: run.branch.clone(),
        anchor_computation_ref: run.branch_base.to_string(),
        target_computation_ref: target.to_string(),
        record_frontier_ref: record_frontier_ref.to_string(),
    };
    atomic_write(
        &repository
            .root()
            .join("runs")
            .join(format!("{}.sealed-record-frontier.json", run.token)),
        &serde_jcs::to_vec(&association)?,
    )
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
        let association: SealedRunRecordFrontier = serde_json::from_slice(&bytes)?;
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
    let driver = CliRealizationDriver::new(&project, &bindings);
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

pub(crate) fn record_schema_registry() -> Result<RecordSchemaRegistry> {
    let mut registry = RecordSchemaRegistry::default();
    registry.register(
        operation(HTTP_PROTOCOL_ID, HTTP_REQUEST_OPERATION),
        |bytes| match decode_http_event(bytes).map_err(|error| error.to_string())? {
            HttpEvent::Request { .. } => Ok(()),
            HttpEvent::Response { .. } => Err("HTTP responses are runtime output".to_owned()),
        },
    )?;
    register_browser_record_schemas(&mut registry)?;
    for operation_id in [
        PTY_INPUT_OPERATION,
        PTY_RESIZE_OPERATION,
        PTY_SIGNAL_OPERATION,
    ] {
        registry.register(operation(PTY_PROTOCOL_ID, operation_id), move |bytes| {
            let event = decode_pty_event(bytes).map_err(|error| error.to_string())?;
            let actual = match event {
                PtyEvent::Input { .. } => PTY_INPUT_OPERATION,
                PtyEvent::Resize { .. } => PTY_RESIZE_OPERATION,
                PtyEvent::Signal { .. } => PTY_SIGNAL_OPERATION,
                PtyEvent::Output { .. } | PtyEvent::Attach | PtyEvent::Detach => {
                    return Err("PTY output and lifecycle observations are not Records".to_owned());
                }
            };
            (actual == operation_id)
                .then_some(())
                .ok_or_else(|| format!("PTY payload kind does not match `{operation_id}`"))
        })?;
    }
    for operation_id in [
        BINDING_ATTACH_OPERATION,
        BINDING_REPLACE_OPERATION,
        BINDING_DETACH_OPERATION,
    ] {
        registry.register(operation(BINDING_PROTOCOL_ID, operation_id), move |bytes| {
            let event = decode_binding_event(bytes).map_err(|error| error.to_string())?;
            let actual = match event {
                BindingEvent::Attach { .. } => BINDING_ATTACH_OPERATION,
                BindingEvent::Replace { .. } => BINDING_REPLACE_OPERATION,
                BindingEvent::Detach { .. } => BINDING_DETACH_OPERATION,
            };
            (actual == operation_id)
                .then_some(())
                .ok_or_else(|| format!("Binding payload kind does not match `{operation_id}`"))
        })?;
    }
    for operation_id in [
        WORKSPACE_PUT_OPERATION,
        WORKSPACE_DELETE_OPERATION,
        WORKSPACE_RENAME_OPERATION,
    ] {
        registry.register(
            operation(WORKSPACE_PROTOCOL_ID, operation_id),
            move |bytes| {
                let mutation = decode_mutation(bytes).map_err(|error| error.to_string())?;
                let actual = match mutation {
                    WorkspaceMutation::Put { .. } => WORKSPACE_PUT_OPERATION,
                    WorkspaceMutation::Delete { .. } => WORKSPACE_DELETE_OPERATION,
                    WorkspaceMutation::Rename { .. } => WORKSPACE_RENAME_OPERATION,
                };
                (actual == operation_id).then_some(()).ok_or_else(|| {
                    format!("Workspace payload kind does not match `{operation_id}`")
                })
            },
        )?;
    }
    for operation_id in [
        ADAPTER_ADD_OPERATION,
        ADAPTER_REMOVE_OPERATION,
        ADAPTER_CONFIGURE_OPERATION,
    ] {
        registry.register(operation(ADAPTER_PROTOCOL_ID, operation_id), move |bytes| {
            let payload =
                decode_adapter_control_payload(bytes).map_err(|error| error.to_string())?;
            let actual = match payload {
                AdapterControlPayload::Add { .. } => ADAPTER_ADD_OPERATION,
                AdapterControlPayload::Remove { .. } => ADAPTER_REMOVE_OPERATION,
                AdapterControlPayload::Configure { .. } => ADAPTER_CONFIGURE_OPERATION,
            };
            (actual == operation_id)
                .then_some(())
                .ok_or_else(|| format!("Adapter payload kind does not match `{operation_id}`"))
        })?;
    }
    Ok(registry)
}

fn operation(protocol_id: &str, operation_id: &str) -> SupportedOperation {
    SupportedOperation::new(protocol_id, operation_id, 1, Default::default())
        .expect("built-in Record operation identifiers are valid")
}

fn materializer_registry() -> Result<MaterializerRegistry> {
    let mut registry = MaterializerRegistry::default();
    registry.register(Arc::new(ReplayMaterializer))?;
    registry.register(Arc::new(ReplayMaterializerV2))?;
    registry.register(Arc::new(SnapshotMaterializer))?;
    registry.register(Arc::new(WorkspaceSnapshotMaterializer))?;
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

fn contract_verifier_registry() -> Result<ContractVerifierRegistry> {
    let mut registry = ContractVerifierRegistry::default();
    registry.register(Arc::new(HttpEndpointVerifier))?;
    registry.register(Arc::new(WorkspaceContentVerifier))?;
    Ok(registry)
}

fn reference_registry() -> Result<ReferenceRegistry> {
    standard_reference_registry()
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
        let schemas = record_schema_registry().unwrap();
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
