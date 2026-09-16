//! Product assembly for the Capsule lifecycle.

#![deny(unsafe_op_in_unsafe_fn)]

mod desktop_control;
mod object_transport;
mod portable_dependency;

pub mod activity_client;
pub mod activity_mcp;

use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_adapter_api::AdapterContext;
#[cfg(test)]
use ato_adapter_browser::{
    BROWSER_CLICK_OPERATION, BROWSER_KEYBOARD_OPERATION, BROWSER_PROTOCOL_ID,
};
use ato_adapter_oci::{DockerOciAdapter, OciEndpoint, OciHandle, OciResourceLimits, OciSpec};
use ato_adapter_process::{ProcessAdapter, ProcessHandle, ProcessSpec};
use ato_adapter_workspace::restore_workspace;
use ato_computation::{ComputationRef, ContentRef};
use ato_formation::authoring::{HTTP_CONTRACT_VERIFIER, WORKSPACE_PROTOCOL};
use ato_formation::verify::{
    ContractVerificationReceipt, RuntimeHttpObservation, RuntimeObservation,
    VerificationExecutionEvidence, VerificationTargetKind, verify_runtime,
};
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
    PortableDependencyProfile, RecordId, ReferenceRegistry, decode_capsule_bundle_document,
    encode_bundle, export_bundle_with_materializations, export_object_graph, import_bundle,
    resolve_computation,
};
use ato_portable_application::portability_export::repack_portable_dependencies_with_archives;
use ato_portable_application::portability_plan::{PortableExportProfile, plan_portable_export};
use ato_portable_application::{
    OCI_CPU_MILLIS_RUNTIME, OCI_IMAGE_RUNTIME, OCI_MEMORY_BYTES_RUNTIME, OCI_PIDS_LIMIT_RUNTIME,
    OCI_PLATFORM_RUNTIME, PYTHON_RUNTIME, PortableRealizationKind, StaticApplicationServer,
    ValidatedPortableApplication, bundle_sha256, materialize_tree, validate_bundle_for_derivation,
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
    /// Preview dependency size and guarantees before a portable export.
    ExportPlan(ExportPlanArgs),
    /// Repack an existing portable application without changing K or D.
    Export(ExportArgs),
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
    #[command(name = "__portable-sandbox-exec", hide = true)]
    PortableSandboxExec(PortableSandboxExecArgs),
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
        Commands::ExportPlan(args) => export_plan(args),
        Commands::Export(args) => export_portable(args),
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
        Commands::PortableSandboxExec(args) => portable_sandbox_exec(args),
    }
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

fn export_plan(args: ExportPlanArgs) -> Result<()> {
    let bytes =
        fs::read(&args.capsule).with_context(|| format!("read {}", args.capsule.display()))?;
    let CapsuleBundleDocument::PortableApplicationV3(bundle) =
        decode_capsule_bundle_document(&bytes)?
    else {
        bail!("export planning currently requires a portable application v3 bundle");
    };
    let mut plan = plan_portable_export(&bundle, bytes.len(), args.portability)?;
    if args.oci_archive.is_some() && args.portability != PortableExportProfile::Offline {
        bail!("--oci-archive is currently supported only for offline export");
    }
    let archives = match args.oci_archive.as_deref() {
        Some(path) => portable_dependency::oci_archive_from_file(&bundle, path)?,
        None => Vec::new(),
    };
    let resolved_sources = match args.portability {
        PortableExportProfile::Thin => portable_dependency::discover_wheel_sources(&bundle),
        PortableExportProfile::Cached | PortableExportProfile::Offline => Ok(BTreeMap::new()),
    };
    match resolved_sources.and_then(|sources| {
        let profile = match args.portability {
            PortableExportProfile::Thin => PortableDependencyProfile::Thin,
            PortableExportProfile::Cached => PortableDependencyProfile::Cached,
            PortableExportProfile::Offline => PortableDependencyProfile::Offline,
        };
        repack_portable_dependencies_with_archives(&bundle, profile, &sources, &archives)
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
    let CapsuleBundleDocument::PortableApplicationV3(bundle) =
        decode_capsule_bundle_document(&bytes)?
    else {
        bail!("dependency export currently requires a complete portable application v3 bundle");
    };
    if args.output.exists() {
        bail!("export output already exists: {}", args.output.display());
    }
    if args.oci_archive.is_some() && args.portability != PortableExportProfile::Offline {
        bail!("--oci-archive is currently supported only for offline export");
    }
    let archives = match args.oci_archive.as_deref() {
        Some(path) => portable_dependency::oci_archive_from_file(&bundle, path)?,
        None => Vec::new(),
    };
    let (profile, sources) = match args.portability {
        PortableExportProfile::Thin => (
            PortableDependencyProfile::Thin,
            portable_dependency::discover_wheel_sources(&bundle)?,
        ),
        PortableExportProfile::Cached => (PortableDependencyProfile::Cached, BTreeMap::new()),
        PortableExportProfile::Offline => (PortableDependencyProfile::Offline, BTreeMap::new()),
    };
    let (output, repacked) =
        repack_portable_dependencies_with_archives(&bundle, profile, &sources, &archives)?;
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

fn run_portable_application(
    args: RunArgs,
    bundle_bytes: &[u8],
    bundle: ato_objects::PortableApplicationBundle,
) -> Result<()> {
    #[cfg(unix)]
    let shutdown = {
        let flag = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&flag))?;
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&flag))?;
        flag
    };
    if !args.bindings.is_empty() {
        bail!(
            "portable application profile {} has no external Bindings",
            bundle.index.profile
        );
    }
    let selected_derivation = match args.derivation.as_deref() {
        Some(reference) => reference,
        None if bundle.index.derivations.len() == 1 => &bundle.index.derivations[0],
        None => bail!(
            "portable Capsule declares {} derivations; select one with --derivation <sha256:...>",
            bundle.index.derivations.len()
        ),
    };
    let validated = validate_bundle_for_derivation(&bundle, selected_derivation)?;
    let cache = ato_home()?.join("cache");
    fs::create_dir_all(&cache)?;
    let runtime = tempfile::Builder::new()
        .prefix("ato-portable-run-")
        .tempdir_in(cache)?;
    let workspace = runtime.path().join("workspace");
    let (hydrated, dependency_fetches) = portable_dependency::hydrate_external_objects(&bundle)?;
    for reference in &dependency_fetches {
        eprintln!("dependency fetched and verified: {reference}");
    }
    materialize_tree(&hydrated, &validated, &workspace)?;
    let mut runtime =
        PortableLocalRuntime::start(&workspace, runtime.path(), &validated, &hydrated)?;

    let mut observation = RuntimeObservation::default();
    for input in &validated.derivation.inputs {
        if input.protocol == WORKSPACE_PROTOCOL {
            observation
                .input_refs
                .insert(input.id.clone(), validated.tree_ref.to_string());
        }
    }
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(500))
        .build()?;
    for requirement in &validated.contract.requirements {
        if requirement.verifier != HTTP_CONTRACT_VERIFIER {
            continue;
        }
        let method = requirement.method.as_deref().unwrap_or("GET");
        if method != "GET" {
            bail!("portable local verifier does not support HTTP method {method}");
        }
        let path = requirement.path.as_deref().unwrap_or("/");
        let request_url = format!("{}{path}", runtime.base_url());
        let mut attempts = 0;
        let response = loop {
            #[cfg(unix)]
            if shutdown.load(Ordering::Relaxed) {
                bail!("portable run interrupted before Contract verification completed");
            }
            match client.get(&request_url).send() {
                Ok(response) => break response,
                Err(error) => {
                    if let Some(status) = runtime.try_wait()? {
                        bail!("selected process derivation exited before verification: {status}");
                    }
                    // Dependency-backed processes may need to construct a clean
                    // environment before the declared Port becomes ready. Keep
                    // the timeout bounded, but do not impose the old static
                    // fixture's five-second startup assumption.
                    if attempts >= 1_200 {
                        return Err(error).context(format!(
                            "selected derivation did not become reachable at {request_url}"
                        ));
                    }
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        let status = response.status().as_u16();
        let body = response.bytes()?;
        observation.http.push(RuntimeHttpObservation::from_response(
            requirement.port.as_deref().unwrap_or_default(),
            method,
            path,
            status,
            &body,
        ));
    }

    let verification = verify_runtime(&validated.contract, &observation);
    let mut receipt = ContractVerificationReceipt::from_runtime(
        bundle_sha256(bundle_bytes),
        validated.contract_ref.to_string(),
        validated.derivation_ref.to_string(),
        VerificationTargetKind::CliLocal,
        &validated.contract,
        &observation,
        verification,
    );
    let mut execution = runtime.execution_evidence();
    if let Some(portability) = &bundle.portability {
        receipt.schema = ato_formation::verify::CONTRACT_VERIFICATION_RECEIPT_SCHEMA_V2.to_owned();
        execution.portability_profile = Some(
            match portability.profile {
                PortableDependencyProfile::Thin => "thin",
                PortableDependencyProfile::Cached => "cached",
                PortableDependencyProfile::Offline => "offline",
            }
            .to_owned(),
        );
        if portability.profile == PortableDependencyProfile::Offline
            && validated.realization == PortableRealizationKind::OciContainer
        {
            execution.embedded_oci_image_loaded = execution.image.clone();
        }
    }
    execution.dependency_fetches = dependency_fetches;
    receipt.execution = Some(execution);
    if let Some(path) = &args.verification_receipt {
        fs::write(path, receipt.canonical_bytes()?)
            .with_context(|| format!("write verification receipt {}", path.display()))?;
    }
    println!("Capsule: {}", validated.contract_ref);
    println!("Route: {}", validated.derivation_ref);
    println!("Runtime: {}", runtime.label());
    println!("Bundle: {}", receipt.bundle_sha256);
    println!("URL: {}", runtime.base_url());
    if !receipt.fully_satisfied {
        let failure = receipt
            .observations
            .iter()
            .find(|observation| {
                !matches!(
                    observation.outcome,
                    ato_formation::verify::ReceiptOutcome::Satisfied
                )
            })
            .map(|observation| observation.id.as_str())
            .unwrap_or("unknown");
        bail!("runtime Contract verification did not fully satisfy observation {failure}");
    }
    if args.no_open {
        return Ok(());
    }
    open_browser(runtime.base_url())?;
    println!("Press Ctrl-C to stop the local realization.");
    loop {
        #[cfg(unix)]
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        #[cfg(not(unix))]
        std::thread::park();
        #[cfg(unix)]
        std::thread::sleep(Duration::from_millis(100));
    }
    #[allow(unreachable_code)]
    Ok(())
}

enum PortableLocalRuntime {
    Static {
        _server: StaticApplicationServer,
        base_url: String,
    },
    Process {
        handle: ProcessHandle,
        base_url: String,
        executable: String,
        version: String,
    },
    Oci {
        handle: OciHandle,
        base_url: String,
    },
}

impl PortableLocalRuntime {
    fn start(
        workspace: &std::path::Path,
        runtime_root: &std::path::Path,
        route: &ValidatedPortableApplication,
        bundle: &ato_objects::PortableApplicationBundle,
    ) -> Result<Self> {
        match route.realization {
            PortableRealizationKind::StaticWeb => {
                let server = StaticApplicationServer::start(workspace, route)?;
                Ok(Self::Static {
                    base_url: server.base_url(),
                    _server: server,
                })
            }
            PortableRealizationKind::LocalProcess => {
                let step = &route.derivation.steps[0];
                let guest_port = route.derivation.ports[0]
                    .guest_port
                    .context("process derivation omitted guest_port")?;
                let listener = TcpListener::bind("127.0.0.1:0")?;
                let host_port = listener.local_addr()?.port();
                drop(listener);
                let guest_port = guest_port.to_string();
                let host_port = host_port.to_string();
                let mut command = step.argv.clone();
                let (executable, version) = if let Some(python_version) =
                    route.derivation.runtimes.get(PYTHON_RUNTIME)
                {
                    if !matches!(command[0].as_str(), "python" | "python3") {
                        bail!(
                            "Python process derivation must use a logical python executable in argv[0]"
                        );
                    }
                    resolve_pinned_python(python_version)?
                } else {
                    (command[0].clone(), "unconstrained".to_owned())
                };
                command[0] = executable.clone();
                let mut replaced = 0;
                for argument in &mut command {
                    if argument == &guest_port {
                        *argument = host_port.clone();
                        replaced += 1;
                    }
                }
                if replaced > 1 {
                    bail!(
                        "process derivation names its declared guest port more than once in argv"
                    );
                }
                let endpoint_name = endpoint_port_env_name(&route.derivation.ports[0].id);
                let mut environment = step.env.clone();
                environment.insert(endpoint_name, host_port.clone());
                let process_runtime = runtime_root.join("process");
                fs::create_dir_all(&process_runtime).with_context(|| {
                    format!(
                        "create portable process runtime {}",
                        process_runtime.display()
                    )
                })?;
                let process_tmp = process_runtime.join("tmp");
                let process_home = process_runtime.join("home");
                fs::create_dir_all(&process_tmp)?;
                fs::create_dir_all(&process_home)?;
                environment.insert(
                    "ATO_RUNTIME_DIR".to_owned(),
                    process_runtime.display().to_string(),
                );
                for name in ["TMPDIR", "TMP", "TEMP"] {
                    environment.insert(name.to_owned(), process_tmp.display().to_string());
                }
                environment.insert("HOME".to_owned(), process_home.display().to_string());
                environment.insert(
                    "XDG_CACHE_HOME".to_owned(),
                    process_home.join(".cache").display().to_string(),
                );
                command = portable_process_sandbox_command(
                    workspace,
                    &process_runtime,
                    &executable,
                    &command,
                    host_port.parse()?,
                )?;
                let adapter = ProcessAdapter::new(ProcessSpec {
                    id: step.id.clone(),
                    command,
                    cwd: PathBuf::from(&step.cwd),
                    environment,
                    isolated_group: true,
                })?;
                let handle = adapter.spawn(workspace)?;
                Ok(Self::Process {
                    handle,
                    base_url: format!("http://127.0.0.1:{host_port}"),
                    executable,
                    version,
                })
            }
            PortableRealizationKind::OciContainer => {
                let step = &route.derivation.steps[0];
                let port = &route.derivation.ports[0];
                let guest_port = port.guest_port.context("OCI route omitted guest_port")?;
                let listener = TcpListener::bind("127.0.0.1:0")?;
                let host_port = listener.local_addr()?.port();
                drop(listener);
                let runtime = &route.derivation.runtimes;
                let spec = OciSpec {
                    id: route.derivation_ref.to_string(),
                    image: runtime
                        .get(OCI_IMAGE_RUNTIME)
                        .context("OCI route omitted image")?
                        .clone(),
                    platform: runtime
                        .get(OCI_PLATFORM_RUNTIME)
                        .context("OCI route omitted platform")?
                        .clone(),
                    argv: step.argv.clone(),
                    working_dir: "/app".to_owned(),
                    environment: step.env.clone(),
                    endpoints: vec![OciEndpoint {
                        host_port,
                        guest_port,
                    }],
                    limits: OciResourceLimits {
                        memory_bytes: parse_runtime_limit(runtime, OCI_MEMORY_BYTES_RUNTIME)?,
                        cpu_limit_millis: parse_runtime_limit(runtime, OCI_CPU_MILLIS_RUNTIME)?,
                        pids_limit: parse_runtime_limit(runtime, OCI_PIDS_LIMIT_RUNTIME)?,
                    },
                    stop_timeout_seconds: 5,
                };
                let adapter = if bundle.portability.as_ref().is_some_and(|portability| {
                    portability.profile == PortableDependencyProfile::Offline
                }) {
                    let archive = bundle
                        .portability
                        .as_ref()
                        .and_then(|portability| {
                            portability.oci_archives.iter().find(|archive| {
                                archive.image == spec.image && archive.platform == spec.platform
                            })
                        })
                        .context("offline OCI image archive is missing")?;
                    let verified =
                        ato_portable_application::oci_archive::verify_oci_archive(archive)?;
                    DockerOciAdapter::new_offline(spec, verified.bytes, verified.config_reference)?
                } else {
                    DockerOciAdapter::new(spec)?
                };
                let handle = adapter.spawn(workspace, &runtime_root.join("oci"))?;
                Ok(Self::Oci {
                    handle,
                    base_url: format!("http://127.0.0.1:{host_port}"),
                })
            }
        }
    }

    fn base_url(&self) -> &str {
        match self {
            Self::Static { base_url, .. } => base_url,
            Self::Process { base_url, .. } => base_url,
            Self::Oci { base_url, .. } => base_url,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Static { .. } => "static web",
            Self::Process { .. } => "local process",
            Self::Oci { .. } => "OCI container",
        }
    }

    fn execution_evidence(&self) -> VerificationExecutionEvidence {
        match self {
            Self::Static { base_url, .. } => VerificationExecutionEvidence {
                realization: "static_web".to_owned(),
                runtime_executable: None,
                runtime_version: None,
                pid: None,
                container_id: None,
                image: None,
                platform: None,
                endpoint: Some(base_url.clone()),
                run_id: None,
                lease_id: None,
                attempt_id: None,
                dependency_fetches: Vec::new(),
                portability_profile: None,
                embedded_oci_image_loaded: None,
            },
            Self::Process {
                handle,
                base_url,
                executable,
                version,
            } => VerificationExecutionEvidence {
                realization: "process".to_owned(),
                runtime_executable: Some(executable.clone()),
                runtime_version: Some(version.clone()),
                pid: Some(handle.pid()),
                container_id: None,
                image: None,
                platform: None,
                endpoint: Some(base_url.clone()),
                run_id: None,
                lease_id: None,
                attempt_id: None,
                dependency_fetches: Vec::new(),
                portability_profile: None,
                embedded_oci_image_loaded: None,
            },
            Self::Oci { handle, base_url } => VerificationExecutionEvidence {
                realization: "oci".to_owned(),
                runtime_executable: Some("docker".to_owned()),
                runtime_version: None,
                pid: None,
                container_id: Some(handle.container_id().to_owned()),
                image: Some(handle.image().to_owned()),
                platform: Some(handle.platform().to_owned()),
                endpoint: Some(base_url.clone()),
                run_id: None,
                lease_id: None,
                attempt_id: None,
                dependency_fetches: Vec::new(),
                portability_profile: None,
                embedded_oci_image_loaded: None,
            },
        }
    }

    fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Static { .. } => Ok(None),
            Self::Process { handle, .. } => Ok(handle.try_wait()?),
            Self::Oci { handle, .. } => match handle.exit_code()? {
                Some(code) => bail!("selected OCI derivation exited before verification: {code}"),
                None => Ok(None),
            },
        }
    }
}

fn resolve_pinned_python(version: &str) -> Result<(String, String)> {
    if let Ok(output) = Command::new("uv")
        .args(["python", "find", version])
        .env("UV_PYTHON_DOWNLOADS", "never")
        .output()
        && output.status.success()
    {
        let path = String::from_utf8(output.stdout)?.trim().to_owned();
        if !path.is_empty() {
            let executable = executable_path(&path).unwrap_or_else(|| PathBuf::from(&path));
            return verified_python(&executable.display().to_string(), version);
        }
    }
    let provisioned_root = PathBuf::from("/opt/ato/toolchains/python");
    if let Ok(entries) = std::fs::read_dir(&provisioned_root) {
        let mut candidates = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|candidate| {
                candidate == version || candidate.starts_with(&format!("{version}."))
            })
            .collect::<Vec<_>>();
        candidates.sort();
        for candidate in candidates.into_iter().rev() {
            let executable = provisioned_root.join(candidate).join("bin").join("python3");
            if executable.is_file() {
                return verified_python(&executable.display().to_string(), version);
            }
        }
    }
    for executable in ["python3", "python"] {
        let Ok(output) = Command::new(executable).arg("--version").output() else {
            continue;
        };
        let reported = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success() && reported.trim().starts_with(&format!("Python {version}.")) {
            let selected = executable_path(executable)
                .unwrap_or_else(|| PathBuf::from(executable))
                .display()
                .to_string();
            return verified_python(&selected, version);
        }
    }
    bail!(
        "selected derivation requires Python {version}; install that runtime with `uv python install {version}`"
    )
}

fn executable_path(executable: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(executable);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate);
    }
    std::env::var_os("PATH").and_then(|search| {
        std::env::split_paths(&search)
            .map(|directory| directory.join(executable))
            .find(|path| path.is_file())
    })
}

fn verified_python(executable: &str, version: &str) -> Result<(String, String)> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .with_context(|| format!("inspect selected Python executable {executable}"))?;
    let reported = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() || !reported.trim().starts_with(&format!("Python {version}.")) {
        bail!(
            "selected Python executable {executable} does not satisfy {version}: {}",
            reported.trim()
        );
    }
    let identity = Command::new(executable)
        .args([
            "-c",
            "import os,sys; print(os.path.realpath(sys.executable))",
        ])
        .output()
        .with_context(|| format!("resolve selected Python executable {executable}"))?;
    let resolved = String::from_utf8(identity.stdout)?.trim().to_owned();
    if !identity.status.success() || resolved.is_empty() || !PathBuf::from(&resolved).is_file() {
        bail!("selected Python executable {executable} did not resolve to a file");
    }
    Ok((resolved, reported.trim().to_owned()))
}

fn portable_process_sandbox_command(
    workspace: &std::path::Path,
    runtime_root: &std::path::Path,
    executable: &str,
    workload: &[String],
    host_port: u16,
) -> Result<Vec<String>> {
    let workspace = workspace
        .canonicalize()
        .context("canonicalize portable workspace")?;
    let runtime_root = runtime_root
        .canonicalize()
        .context("canonicalize portable runtime directory")?;
    let executable = PathBuf::from(executable)
        .canonicalize()
        .with_context(|| format!("canonicalize portable runtime {executable}"))?;
    let interpreter_root = executable
        .parent()
        .and_then(std::path::Path::parent)
        .context("portable runtime has no readable installation root")?
        .to_path_buf();
    let system_roots = [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc",
        "/dev",
        "/proc",
        "/System",
        "/Library",
        "/private/var/db",
        "/opt",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| path.exists());
    let policy = SandboxPolicy::new()
        .allow_read_write([runtime_root.clone()])
        .allow_read_only(
            [workspace, interpreter_root]
                .into_iter()
                .chain(system_roots),
        )
        .with_network(false)
        .allow_tcp_bind([host_port]);
    let policy_path = runtime_root.join("sandbox-policy.json");
    fs::write(&policy_path, serde_json::to_vec_pretty(&policy)?)
        .with_context(|| format!("write portable sandbox policy {}", policy_path.display()))?;
    let shim = std::env::current_exe()
        .context("locate ato sandbox shim")?
        .canonicalize()
        .context("canonicalize ato sandbox shim")?;
    let mut command = vec![
        shim.display().to_string(),
        "__portable-sandbox-exec".to_owned(),
        "--policy".to_owned(),
        policy_path.display().to_string(),
        "--".to_owned(),
    ];
    command.extend(workload.iter().cloned());
    Ok(command)
}

fn endpoint_port_env_name(port_id: &str) -> String {
    format!(
        "ATO_ENDPOINT_{}_PORT",
        port_id
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

fn parse_runtime_limit(runtimes: &BTreeMap<String, String>, key: &str) -> Result<u64> {
    runtimes
        .get(key)
        .with_context(|| format!("OCI route omitted {key}"))?
        .parse()
        .with_context(|| format!("OCI route has invalid {key}"))
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
