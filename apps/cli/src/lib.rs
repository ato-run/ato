//! Product assembly for the Capsule lifecycle.

#![deny(unsafe_op_in_unsafe_fn)]

mod authoring;
mod supervisor;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
use ato_compose::ComposeReferences;
use ato_computation::{ComputationRef, ContentRef};
use ato_materializer_api::{
    Compatibility, MaterializerContext, MaterializerRegistry, RestoreCapability,
};
use ato_materializer_replay::{
    ReplayMaterializer, ReplayMaterializerV2, ReplayReferences, ReplayV2References,
};
use ato_materializer_snapshot::{SnapshotMaterializer, SnapshotReferences};
use ato_objects::{
    BranchOrigin, BundleMaterialization, CapsuleSelector, LocalCapsuleRepository, RecordId,
    ReferenceRegistry, decode_bundle, encode_bundle, export_bundle_with_materializations,
    import_bundle,
};
use ato_record_writer::RecordSchemaRegistry;
use ato_record_writer::{load_frontier, records_for_frontier};
use clap::{Args, Parser, Subcommand};

use crate::authoring::{
    AuthoringReferences, evolve_workspace, initial_computation, load_config, load_runtime_state,
    workspace_policy,
};
use crate::supervisor::{CliRealizationDriver, start_durable, stop_active};

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

pub fn run() -> Result<()> {
    match Cli::parse().command {
        Commands::Init(args) => init(args),
        Commands::Resume(args) => resume(args),
        Commands::Stop { capsule } => stop(&capsule),
        Commands::Encap(args) => encap(args),
        Commands::Run(args) => run_capsule(args),
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
    let records = repository.records_for_causal_branch(&selector.branch, selector.record)?;
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
        args.materializers
    };
    let (records_v2, replay_anchor, record_frontier_ref) = if selected
        .iter()
        .any(|materializer| materializer == "ato.replay@2")
    {
        let (records, anchor, frontier) =
            load_run_record_frontier(&repository, &selector.branch, &target)?;
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
    };
    let mut entries = Vec::new();
    for id in selected {
        let materializer = materializers.get(&id)?;
        let descriptor = materializer.encode(&target, &context)?;
        let verified = materializer.verify(&descriptor, &context)?;
        if verified != target {
            bail!("materializer `{id}` verified a different computation {verified}");
        }
        entries.push(BundleMaterialization {
            materializer_id: id,
            descriptor_ref: descriptor.to_string(),
        });
    }
    let references = reference_registry()?;
    let bundle =
        export_bundle_with_materializations(&target, &entries, repository.objects(), &references)?;
    atomic_write(&args.output, &encode_bundle(&bundle)?)?;
    println!("{target}");
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
    };
    let mut candidates = bundle.index.materializations.clone();
    candidates.sort_by(|left, right| left.materializer_id.cmp(&right.materializer_id));
    let mut diagnostics = Vec::new();
    let mut restored = None;
    for candidate in candidates {
        let descriptor = ContentRef::parse(&candidate.descriptor_ref)?;
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
        restored = Some(materializer.restore(&descriptor, &context)?);
        break;
    }
    let realization = restored.ok_or_else(|| {
        anyhow::anyhow!(
            "no compatible restore-capable Materialization: {}",
            diagnostics.join("; ")
        )
    })?;
    if realization.target() != &root {
        bail!(
            "Materialization restored {}, expected bundle root {root}",
            realization.target()
        );
    }
    realization.run().map_err(Into::into)
}

pub(crate) fn adapter_registry() -> Result<AdapterRegistry> {
    let mut registry = AdapterRegistry::default();
    registry.register(Arc::new(ProcessLifecycleAdapter))?;
    registry.register(Arc::new(PtyAdapter))?;
    registry.register(Arc::new(WorkspaceAdapter))?;
    registry.register(Arc::new(BindingAdapter))?;
    registry.register(Arc::new(HttpAdapter))?;
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
    Ok(registry)
}

fn reference_registry() -> Result<ReferenceRegistry> {
    let mut registry = ReferenceRegistry::default();
    registry.register(Arc::new(AuthoringReferences::new()))?;
    registry.register(Arc::new(ComposeReferences::default()))?;
    registry.register_materializer(Arc::new(ReplayReferences))?;
    registry.register_materializer(Arc::new(ReplayV2References))?;
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
    use super::*;

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
    fn snapshot_id_uses_materializer_vocabulary() {
        assert_eq!(
            ato_materializer_snapshot::SNAPSHOT_MATERIALIZER_ID,
            "ato.snapshot@1"
        );
    }
}
