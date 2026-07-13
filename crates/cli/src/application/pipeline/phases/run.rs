use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use capsule::CapsuleReporter;
use capsule::ato_lock::AtoLock;
use capsule::dependency_contracts::{
    DependencyLock, DependencyLockInput, ResolvedProviderManifest, verify_and_lock,
};
use capsule::execution_identity::EnvOrigin;
use capsule::execution_plan::error::AtoExecutionError;
use capsule::execution_plan::guard::ExecutorKind;
use capsule::lockfile::{
    CAPSULE_LOCK_FILE_NAME, CapsuleLock, manifest_external_capsule_dependencies,
    verify_lockfile_external_dependencies,
};
use capsule::types::{
    CapsuleManifest, CapsuleType, ConfigField, ConfigKind, MANIFEST_SCHEMA_VERSION, StateDurability,
};
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::application::build_materialization as bm;
use crate::application::dependency_credentials::{HostEnv, ProcessHostEnv, RedactionRegistry};
use crate::application::dependency_materializer::{
    AttestationStrategy, CacheStrategy, DependencyMaterializationRequest, DependencyMaterializer,
    DependencyProjection, InstallPolicies, ManifestInputs, PlatformTriple, RuntimeSelection,
    SessionDependencyMaterializer, digest_file,
};
use crate::application::dependency_runtime::orchestrator::{
    OrchestratorError, OrchestratorInput, OrchestratorProvider, RunningGraph,
    start_all as start_dependency_graph,
};
use crate::application::engine::install::support::{
    LocalRunManifestPreparationOutcome, ResolvedCliExportRequest, ResolvedRunTarget,
};
use crate::application::pipeline::cleanup::PipelineAttemptContext;
use crate::application::ports::output::OutputPort;
use crate::application::workspace::state::EffectiveLockState;
use crate::executors::launch_context::InjectedMount;
use crate::executors::source::ExecuteMode;
use crate::executors::target_runner::{self, TargetLaunchOptions};
use crate::preview;
use crate::registry::store::RegistryStore;
use crate::reporters::CliReporter;
use crate::runtime::overrides as runtime_overrides;
use crate::runtime::provisioning::{self as provisioner, AutoProvisioningOptions};
use crate::state::{
    ensure_registered_state_binding, ensure_registered_state_binding_in_store,
    parse_state_reference, resolve_registered_state_reference,
    resolve_registered_state_reference_in_store,
};
use capsule::router;

use crate::ProviderToolchain;
use crate::RunAgentMode;

use crate::application::pipeline::hourglass::HourglassPhase;

pub(crate) trait ConsumerRunProgress {
    fn start(&self, phase: HourglassPhase);
    fn ok(&self, phase: HourglassPhase, detail: &str);
    fn skip(&self, phase: HourglassPhase, detail: &str);
}

#[derive(Debug, Clone)]
pub(crate) struct CompatibilityLegacyLockContext {
    pub(crate) manifest_path: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) lock: CapsuleLock,
}

#[derive(Debug, Clone)]
pub(crate) struct RunAuthoritativeInput {
    pub(crate) lock: AtoLock,
    pub(crate) lock_path: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) materialization_root: PathBuf,
    pub(crate) effective_state: EffectiveLockState,
    pub(crate) compatibility_legacy_lock: Option<CompatibilityLegacyLockContext>,
}

// PreparedRunContext carries the already-fixed bridge artifact and compatibility-scoped
// validation context. Downstream phases may consume this data, but must not reinterpret
// manifest semantics or discover new authority from disk.
#[derive(Debug, Clone)]
pub(crate) struct RunExecutionOverride {
    pub(crate) target_label: String,
    pub(crate) args: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRunContext {
    pub(crate) authoritative_lock: Option<AtoLock>,
    pub(crate) lock_path: Option<PathBuf>,
    pub(crate) workspace_root: PathBuf,
    pub(crate) effective_state: Option<EffectiveLockState>,
    pub(crate) execution_override: Option<RunExecutionOverride>,
    pub(crate) bridge_manifest: DerivedBridgeManifest,
    pub(crate) validation_mode: capsule::types::ValidationMode,
    pub(crate) engine_override_declared: bool,
    pub(crate) compatibility_legacy_lock: Option<CompatibilityLegacyLockContext>,
    /// Install profile key for an installed-app launch (`ato launch`), `None`
    /// for ephemeral `ato run`. Captured from the trusted install-lifecycle
    /// identity on the synchronous run thread and threaded explicitly into
    /// [`resolve_launch_context`] so the launch path never reads the
    /// thread-local install context across an async executor boundary (#508).
    ///
    /// [`resolve_launch_context`]: crate::adapters::runtime::executors::target_runner::resolve_launch_context
    pub(crate) install_profile_key: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DerivedBridgeManifest {
    value: toml::Value,
}

impl DerivedBridgeManifest {
    pub(crate) fn new(value: toml::Value) -> Self {
        Self { value }
    }

    pub(crate) fn as_toml(&self) -> &toml::Value {
        &self.value
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedDerivedExecution {
    pub(crate) execution_plan: capsule::execution_plan::model::ExecutionPlan,
    pub(crate) tier: capsule::execution_plan::model::ExecutionTier,
    pub(crate) guard_result: capsule::execution_plan::guard::RuntimeGuardResult,
}

impl PreparedRunContext {
    pub(crate) fn from_authoritative_input(
        authoritative_input: Option<&RunAuthoritativeInput>,
        workspace_root: &Path,
        validation_mode: capsule::types::ValidationMode,
        target_label: Option<&str>,
    ) -> Result<Self> {
        let routed_manifest = authoritative_input
            .map(|input| {
                router::route_lock(
                    &input.lock_path,
                    &input.lock,
                    &input.materialization_root,
                    router::ExecutionProfile::Dev,
                    target_label,
                )
            })
            .transpose()?;
        let bridge_manifest = routed_manifest
            .as_ref()
            .map(|decision| decision.plan.manifest.clone())
            .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
        Ok(Self {
            authoritative_lock: authoritative_input.map(|input| input.lock.clone()),
            lock_path: authoritative_input.map(|input| input.lock_path.clone()),
            workspace_root: authoritative_input
                .map(|input| input.workspace_root.clone())
                .unwrap_or_else(|| workspace_root.to_path_buf()),
            effective_state: authoritative_input.map(|input| input.effective_state.clone()),
            execution_override: None,
            bridge_manifest: DerivedBridgeManifest::new(bridge_manifest),
            validation_mode,
            engine_override_declared: routed_manifest
                .as_ref()
                .is_some_and(|decision| decision.plan.manifest.get("engine").is_some()),
            compatibility_legacy_lock: authoritative_input
                .and_then(|input| input.compatibility_legacy_lock.clone()),
            // Stamped explicitly by the caller from the request's trusted
            // install-lifecycle identity (see the run pipeline below).
            install_profile_key: None,
        })
    }

    pub(crate) fn with_bridge_manifest(
        &self,
        bridge_manifest: toml::Value,
        validation_mode: capsule::types::ValidationMode,
        engine_override_declared: bool,
    ) -> Self {
        Self {
            authoritative_lock: self.authoritative_lock.clone(),
            lock_path: self.lock_path.clone(),
            workspace_root: self.workspace_root.clone(),
            effective_state: self.effective_state.clone(),
            execution_override: self.execution_override.clone(),
            bridge_manifest: DerivedBridgeManifest::new(bridge_manifest),
            validation_mode,
            engine_override_declared,
            compatibility_legacy_lock: self.compatibility_legacy_lock.clone(),
            install_profile_key: self.install_profile_key.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConsumerRunRequest {
    pub(crate) target: PathBuf,
    pub(crate) target_label: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) read_grants: Vec<String>,
    pub(crate) write_grants: Vec<String>,
    pub(crate) read_write_grants: Vec<String>,
    pub(crate) caller_cwd: PathBuf,
    pub(crate) effective_cwd: Option<PathBuf>,
    pub(crate) authoritative_input: Option<RunAuthoritativeInput>,
    pub(crate) desktop_open_path: Option<PathBuf>,
    pub(crate) background: bool,
    pub(crate) nacelle: Option<PathBuf>,
    pub(crate) enforcement: String,
    pub(crate) sandbox_mode: bool,
    pub(crate) dangerously_skip_permissions: bool,
    /// Single carrier for the run/session unsafe gate (#73 PR-C).
    /// Computed once at the entry point as
    /// `dangerously_skip_permissions || env CAPSULE_ALLOW_UNSAFE == "1"`,
    /// so downstream code reads the request rather than the env directly
    /// or relying on argv injection into a child supervisor. Currently set
    /// at construction time; PR-D migrates the existing env / argv readers
    /// inside the run pipeline (`source.rs`, `node_compat.rs`, `deno.rs`,
    /// `target_runner.rs`) to consume this field instead, at which point
    /// the field becomes load-bearing.
    #[allow(dead_code)] // PR-C: written; consumed in PR-D.
    pub(crate) allow_unsafe: bool,
    pub(crate) compatibility_fallback: Option<String>,
    pub(crate) provider_toolchain_requested: ProviderToolchain,
    pub(crate) use_existing_toml: Option<String>,
    pub(crate) explicit_commit: Option<String>,
    pub(crate) assume_yes: bool,
    pub(crate) verbose: bool,
    pub(crate) agent_mode: RunAgentMode,
    pub(crate) agent_local_root: Option<PathBuf>,
    pub(crate) registry: Option<String>,
    pub(crate) keep_failed_artifacts: bool,
    pub(crate) auto_fix_mode: Option<crate::GitHubAutoFixMode>,
    pub(crate) allow_unverified: bool,
    pub(crate) export_request: Option<ResolvedCliExportRequest>,
    pub(crate) state_bindings: Vec<String>,
    /// When set, unbound persistent `[state.*]` (attach="explicit") entries are
    /// auto-bound under this root (server/runner context). See
    /// `resolve_state_source_overrides_managed`. The caller encodes owner AND
    /// stable capsule identity into the root; neither is derived from here.
    pub(crate) managed_state_root: Option<PathBuf>,
    pub(crate) inject_bindings: Vec<String>,
    pub(crate) build_policy: crate::application::build_materialization::BuildPolicy,
    pub(crate) cache_strategy: CacheStrategy,
    pub(crate) reporter: Arc<CliReporter>,
    pub(crate) preview_mode: bool,
    /// #500 — opt-in strict fail-closed realization profile. When `true`, the
    /// execute phase consults the strict realization gate before any process or
    /// container is created and blocks the launch with a typed error if a
    /// required input cannot be verified. `false` (default) keeps the
    /// conservative, non-breaking behavior.
    pub(crate) strict_realization: bool,
    /// Revision-pinned output directory set by `ato launch`. When `Some`,
    /// `run_install_phase` bypasses `resolve_run_target_or_install` and uses
    /// this frozen revision output dir directly as the run target.
    pub(crate) pinned_revision_output_dir: Option<std::path::PathBuf>,
    /// Trusted install-lifecycle identity set by `ato launch`. Threaded into the
    /// dependency materialization request so the session record is stamped with
    /// installed app / profile / revision identity via explicit data flow.
    pub(crate) install_lifecycle_context:
        Option<crate::cli::commands::run::InstallLifecycleContext>,
    /// Launch-condition inputs from a `capsule://…?<query>` launch URL, overlaid
    /// onto the in-memory installed-state claims before the relaunch preflight
    /// resolves them (inputs, not proof). Empty for `ato run` / `ato launch <ipk>`.
    pub(crate) capsule_launch_inputs: Vec<capsule::installed_state::LaunchConditionInput>,
}

impl ConsumerRunRequest {
    fn effective_cwd(&self) -> &Path {
        self.effective_cwd
            .as_deref()
            .unwrap_or(self.caller_cwd.as_path())
    }
}

pub(crate) struct RunInstallPhaseResult {
    pub(crate) resolved_target: ResolvedRunTarget,
    pub(crate) manifest_outcome: LocalRunManifestPreparationOutcome,
    pub(crate) dependency_projection: DependencyProjection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxGrantAccess {
    Read,
    Write,
    ReadWrite,
}

impl SandboxGrantAccess {
    fn allows(self, kind: InferredIoKind) -> bool {
        matches!(
            (self, kind),
            (Self::Read, InferredIoKind::Read)
                | (Self::Write, InferredIoKind::Write)
                | (Self::ReadWrite, InferredIoKind::Read)
                | (Self::ReadWrite, InferredIoKind::Write)
        )
    }

    fn readonly(self) -> bool {
        matches!(self, Self::Read)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SandboxGrantScope {
    Exact,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InferredIoKind {
    Read,
    Write,
}

#[derive(Debug, Clone)]
struct ResolvedSandboxGrant {
    source_path: PathBuf,
    guest_target: PathBuf,
    access: SandboxGrantAccess,
    scope: SandboxGrantScope,
}

fn lexical_normalize_absolute(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    normalized
}

fn reject_symlink_traversal(path: &Path, allow_missing_leaf: bool) -> Result<()> {
    let mut current = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                current.pop();
            }
            Component::Normal(segment) => {
                current.push(segment);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) => {
                        if metadata.file_type().is_symlink() {
                            anyhow::bail!(
                                "sandbox grant '{}' is rejected because it traverses symlink '{}'",
                                path.display(),
                                current.display()
                            );
                        }
                    }
                    Err(err)
                        if allow_missing_leaf && err.kind() == std::io::ErrorKind::NotFound =>
                    {
                        return Ok(());
                    }
                    Err(err) => {
                        return Err(err).with_context(|| {
                            format!("failed to inspect path component {}", current.display())
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

fn normalize_existing_path(path: &Path) -> Result<(PathBuf, SandboxGrantScope)> {
    reject_symlink_traversal(path, false)?;
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve path {}", path.display()))?;
    let metadata = fs::metadata(&canonical)
        .with_context(|| format!("failed to stat path {}", canonical.display()))?;
    let scope = if metadata.is_dir() {
        SandboxGrantScope::Directory
    } else {
        SandboxGrantScope::Exact
    };
    // Grant paths become sandbox mount sources; strip the `\\?\` prefix
    // canonicalize() adds on Windows so downstream consumers see the
    // normal spelling.
    let canonical = capsule::common::paths::windows_child_compatible_path(&canonical);
    Ok((canonical, scope))
}

fn normalize_write_path(path: &Path) -> Result<(PathBuf, SandboxGrantScope)> {
    if path.exists() {
        return normalize_existing_path(path);
    }

    reject_symlink_traversal(path, true)?;

    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "write grant '{}' must include a parent directory",
            path.display()
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        anyhow::anyhow!(
            "write grant '{}' must name a file or directory",
            path.display()
        )
    })?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("failed to resolve parent directory {}", parent.display()))?;
    let canonical_parent = capsule::common::paths::windows_child_compatible_path(&canonical_parent);
    Ok((canonical_parent.join(file_name), SandboxGrantScope::Exact))
}

fn resolve_grant_source_path(
    raw: &str,
    effective_cwd: &Path,
    access: SandboxGrantAccess,
) -> Result<(PathBuf, SandboxGrantScope)> {
    let requested = PathBuf::from(raw);
    let absolute = if requested.is_absolute() {
        requested
    } else {
        effective_cwd.join(requested)
    };

    match access {
        SandboxGrantAccess::Read | SandboxGrantAccess::ReadWrite => {
            normalize_existing_path(&absolute)
        }
        SandboxGrantAccess::Write => normalize_write_path(&absolute),
    }
}

fn guest_target_path(raw: &str, guest_cwd: &Path) -> PathBuf {
    let requested = PathBuf::from(raw);
    let absolute = if requested.is_absolute() {
        requested
    } else {
        guest_cwd.join(requested)
    };
    lexical_normalize_absolute(absolute)
}

fn resolve_sandbox_grants(
    request: &ConsumerRunRequest,
    guest_cwd: &Path,
) -> Result<Vec<ResolvedSandboxGrant>> {
    let mut resolved = Vec::new();
    let effective_cwd = request.effective_cwd();
    let guest_root = if effective_cwd.is_absolute() {
        effective_cwd
    } else {
        guest_cwd
    };

    for (values, access) in [
        (&request.read_grants, SandboxGrantAccess::Read),
        (&request.write_grants, SandboxGrantAccess::Write),
        (&request.read_write_grants, SandboxGrantAccess::ReadWrite),
    ] {
        for value in values {
            let (source_path, scope) = resolve_grant_source_path(value, effective_cwd, access)?;
            resolved.push(ResolvedSandboxGrant {
                source_path,
                guest_target: guest_target_path(value, guest_root),
                access,
                scope,
            });
        }
    }

    Ok(resolved)
}

fn normalize_candidate_path(
    raw: &str,
    effective_cwd: &Path,
    kind: InferredIoKind,
) -> Option<PathBuf> {
    let candidate = PathBuf::from(raw);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        effective_cwd.join(candidate)
    };

    // Candidates compare against grant source paths, which are produced in
    // the `\\?\`-stripped canonical form (see normalize_existing_path);
    // strip here too so the comparison stays apples-to-apples on Windows.
    let strip = |path: PathBuf| capsule::common::paths::windows_child_compatible_path(&path);
    match kind {
        InferredIoKind::Read => fs::canonicalize(&absolute).ok().map(strip),
        InferredIoKind::Write => {
            if absolute.exists() {
                fs::canonicalize(&absolute).ok().map(strip)
            } else {
                let parent = absolute.parent()?;
                let file_name = absolute.file_name()?;
                let canonical_parent = strip(fs::canonicalize(parent).ok()?);
                Some(canonical_parent.join(file_name))
            }
        }
    }
}

fn grant_allows_path(grant: &ResolvedSandboxGrant, path: &Path, kind: InferredIoKind) -> bool {
    if !grant.access.allows(kind) {
        return false;
    }

    match grant.scope {
        SandboxGrantScope::Exact => path == grant.source_path,
        SandboxGrantScope::Directory => path.starts_with(&grant.source_path),
    }
}

fn infer_io_candidates(args: &[String], effective_cwd: &Path) -> Vec<(String, InferredIoKind)> {
    let mut inferred = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let current = &args[index];
        if matches!(current.as_str(), "-o" | "--output")
            && let Some(next) = args.get(index + 1)
        {
            inferred.push((next.clone(), InferredIoKind::Write));
            index += 2;
            continue;
        }
        if let Some(value) = current.strip_prefix("--output=") {
            if !value.trim().is_empty() {
                inferred.push((value.to_string(), InferredIoKind::Write));
            }
            index += 1;
            continue;
        }
        if !current.starts_with('-')
            && normalize_candidate_path(current, effective_cwd, InferredIoKind::Read).is_some()
        {
            inferred.push((current.clone(), InferredIoKind::Read));
        }
        index += 1;
    }
    inferred
}

fn validate_sandbox_grants_best_effort(
    request: &ConsumerRunRequest,
    grants: &[ResolvedSandboxGrant],
) -> Result<()> {
    let effective_cwd = request.effective_cwd();
    for (raw, kind) in infer_io_candidates(&request.args, effective_cwd) {
        let Some(normalized) = normalize_candidate_path(&raw, effective_cwd, kind) else {
            continue;
        };
        if grants
            .iter()
            .any(|grant| grant_allows_path(grant, &normalized, kind))
        {
            continue;
        }

        let detail = match kind {
            InferredIoKind::Read => "read",
            InferredIoKind::Write => "write",
        };
        let suggestion = match kind {
            InferredIoKind::Read => format!("--read {}", raw),
            InferredIoKind::Write => format!("--write {}", raw),
        };
        anyhow::bail!(
            "Missing {} grant for {}\nResolved against effective cwd: {}\n\nTry:\n  {}",
            detail,
            raw,
            effective_cwd.display(),
            suggestion
        );
    }

    Ok(())
}

fn is_one_shot_run_request(request: &ConsumerRunRequest, prepared: &PreparedRunContext) -> bool {
    matches!(prepared_capsule_type(prepared), Some(CapsuleType::Job))
        || request.export_request.is_some()
        || prepared.execution_override.is_some()
}

fn prepared_capsule_type(prepared: &PreparedRunContext) -> Option<CapsuleType> {
    let raw = prepared
        .bridge_manifest
        .as_toml()
        .get("type")
        .or_else(|| prepared.bridge_manifest.as_toml().get("capsule_type"))?
        .as_str()?
        .trim()
        .to_ascii_lowercase();

    match raw.as_str() {
        "inference" => Some(CapsuleType::Inference),
        "tool" => Some(CapsuleType::Tool),
        "job" => Some(CapsuleType::Job),
        "library" => Some(CapsuleType::Library),
        "app" => Some(CapsuleType::App),
        _ => None,
    }
}

pub(crate) struct RunPipelineState {
    pub(crate) preview_session: Option<preview::PreviewSession>,
    pub(crate) preview_mode: bool,
    pub(crate) use_progressive_ui: bool,
    pub(crate) prepared: PreparedRunContext,
    pub(crate) decision: capsule::router::RuntimeDecision,
    pub(crate) launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
    pub(crate) external_capsules: Option<crate::external_capsule::ExternalCapsuleGuard>,
    pub(crate) dep_contracts: Option<DependencyContractGuard>,
    pub(crate) agent_attempted: bool,
    pub(crate) derived_execution: Option<PreparedDerivedExecution>,
    pub(crate) compatibility_host_mode: Option<CompatibilityHostMode>,
    pub(crate) native_nacelle: Option<PathBuf>,
    /// Build materialization observation captured during the Build phase.
    /// Populated by `run_build_phase`; surfaces as `digest=` / `source=`
    /// extras on PHASE-TIMING and feeds the policy decision.
    pub(crate) build_observation:
        Option<crate::application::build_materialization::BuildObservation>,
    /// Outcome of the Build phase decision: which `result_kind` to emit on
    /// PHASE-TIMING. None until run_build_phase populates it.
    pub(crate) build_decision_kind:
        Option<crate::application::build_materialization::BuildResultKind>,
    /// PR-3b boundary plumbing: handle to the
    /// `ReceiptEmissionContext::graph_id_sink` for this launch. Set by
    /// the outer wrapper (`cli::commands::run::execute`) before the
    /// pipeline runs — both Prepare (when the state is first built)
    /// and Execute (defensive re-injection) call
    /// [`attach_receipt_graph_id_sink`]. The Execute phase writes
    /// declared/resolved ids to the sink immediately after
    /// `build_prelaunch_receipt_document_with_graph` so the partial
    /// receipt boundary observes the same ids on the failure path.
    /// `None` for paths that don't go through the wrapper (legacy tests).
    ///
    /// Note: PR-3b deliberately does NOT carry the full
    /// `LaunchGraphBundle` on the pipeline state. The bundle is owned
    /// by the receipt builder and lives only inside the Execute
    /// phase's local scope; the sink is the only handle that survives
    /// the boundary. A future PR that needs the bundle later in the
    /// pipeline can add the carrier explicitly — but adding the
    /// field today without a reader would be a dead carrier (per
    /// PR #180 review feedback).
    pub(crate) receipt_graph_id_sink:
        Option<crate::application::receipt_boundary::ReceiptGraphIdSink>,
}

/// PR-3b plumbing helper (PR #180 review fix): install the given
/// boundary sink onto a [`RunPipelineState`].
///
/// Production paths set the sink at Prepare-phase exit AND re-inject
/// it at Execute-phase entry. The re-injection at Execute is
/// defensive — Build / Verify / DryRun mutate the state in place
/// today and the sink survives, but a future refactor that
/// reconstructs the state would silently drop the field. Calling
/// this helper at both ends pins the contract: when the Execute
/// phase reaches the receipt-emit site, `state.receipt_graph_id_sink`
/// is `Some(...)` if the wrapper provided one.
pub(crate) fn attach_receipt_graph_id_sink(
    mut state: RunPipelineState,
    sink: crate::application::receipt_boundary::ReceiptGraphIdSink,
) -> RunPipelineState {
    state.receipt_graph_id_sink = Some(sink);
    state
}

#[derive(Debug)]
pub(crate) struct DependencyContractGuard {
    graph: Option<RunningGraph>,
    lock: DependencyLock,
    /// Token for the SIGINT teardown hook registered on construction
    /// (#24). Removed from the registry on normal `Drop` so the hook
    /// does not run a second time on top of the in-Drop teardown.
    sigint_token: Option<crate::application::pipeline::cleanup::DepContractTeardownToken>,
}

impl DependencyContractGuard {
    fn new(graph: RunningGraph, lock: DependencyLock) -> Self {
        // #24 SIGINT teardown registration. We capture the per-dep
        // `(pid, state_dir, alias)` tuples — NOT the `RunningGraph`
        // itself — and run the same teardown the in-process Drop runs.
        // Capturing pids/state_dirs is enough because
        // `teardown_reverse_topological` is purely pid-driven and
        // `sweep_stale_sentinel` is path-driven. If the happy path
        // wins (normal exit), `unregister_dep_contract_sigint_teardown`
        // drops this hook before Drop runs the in-process teardown.
        let sigint_targets: Vec<crate::application::dependency_runtime::TeardownTarget> = graph
            .deps()
            .iter()
            .rev()
            .map(
                |dep| crate::application::dependency_runtime::TeardownTarget {
                    dep: dep.alias.clone(),
                    pid: dep.child.id() as i32,
                    state_dir: dep.state_dir.clone(),
                    needs: Vec::new(),
                },
            )
            .collect();
        let token = if sigint_targets.is_empty() {
            None
        } else {
            Some(
                crate::application::pipeline::cleanup::register_dep_contract_sigint_teardown(
                    move || {
                        // Best-effort teardown on Ctrl+C. Errors are
                        // swallowed because we are about to exit
                        // anyway; the alternative (pretending we
                        // didn't try) leaves postmaster.pid stale.
                        let _ =
                            crate::application::dependency_runtime::teardown_reverse_topological(
                                sigint_targets.clone(),
                                // 5s instead of 10s — Ctrl+C means
                                // the user is waiting; SIGTERM with a
                                // tight grace then SIGKILL is the
                                // right balance.
                                Duration::from_secs(5),
                            );
                        for target in &sigint_targets {
                            let _ =
                                crate::application::dependency_runtime::orphan::sweep_stale_sentinel(
                                    &target.state_dir,
                                );
                        }
                    },
                ),
            )
        };
        Self {
            graph: Some(graph),
            lock,
            sigint_token: token,
        }
    }

    pub(crate) fn graph(&self) -> Option<&RunningGraph> {
        self.graph.as_ref()
    }

    pub(crate) fn lock(&self) -> &DependencyLock {
        &self.lock
    }

    pub(crate) fn shutdown_now(&mut self) {
        if let Some(token) = self.sigint_token.take() {
            crate::application::pipeline::cleanup::unregister_dep_contract_sigint_teardown(token);
        }
        if let Some(graph) = self.graph.take() {
            let _ = graph.teardown(Duration::from_secs(10));
        }
    }
}

impl Drop for DependencyContractGuard {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

impl DependencyContractGuard {
    pub(crate) fn detach(mut self) {
        // The SIGINT hook becomes load-bearing AFTER detach: there is
        // no longer a Drop owner that will tear down the graph, so we
        // INTENTIONALLY do not unregister the token. If SIGINT arrives
        // before the detached process exits, the hook reaps providers.
        // If the detached process exits cleanly later, the hook is a
        // no-op that hits ESRCH on already-gone pids.
        let _ = self.sigint_token.take();
        if let Some(graph) = self.graph.take() {
            std::mem::forget(graph);
        }
    }
}

pub(crate) fn dependency_contract_start_error(
    target_label: &str,
    error: OrchestratorError,
) -> anyhow::Error {
    use crate::application::dependency_credentials::CredentialError;
    match error {
        OrchestratorError::OrphanAliveOtherSession {
            alias,
            session_pid,
            resolved,
            state_dir,
        } => anyhow!(
            "dep '{}' state.dir is owned by ato session pid {}; stop that session or use --target <other> to share the workspace. state: {}; provider: {}",
            alias,
            session_pid,
            state_dir.display(),
            resolved
        ),
        OrchestratorError::Credential {
            alias,
            source: CredentialError::EnvKeyMissing { key },
        } => anyhow!(
            "dep '{}' credential needs ${}: export {}=<value> before re-running ('{}' is required by the manifest's top-level required_env and used in [dependencies.{}].credentials)",
            alias,
            key,
            key,
            key,
            alias
        ),
        OrchestratorError::Credential {
            alias,
            source: CredentialError::EnvKeyOutOfScope { key },
        } => anyhow!(
            "dep '{}' credential references ${} but '{}' is not declared in the manifest's top-level required_env: add it under required_env so the credential resolver can read it",
            alias,
            key,
            key
        ),
        OrchestratorError::MissingProviderHostTool {
            alias,
            tool,
            expected_path,
            suggestion,
        } => anyhow!(
            "dep '{}' ready probe expects host tool '{}' at {} but it is not installed on this host. {}",
            alias,
            tool,
            expected_path.display(),
            suggestion
        ),
        OrchestratorError::ToolArtifact { alias, source } => anyhow!(
            "dep '{}' tool artifact resolution failed before provider start: {}",
            alias,
            source
        ),
        other => anyhow::Error::new(other).context(format!(
            "failed to start dependency contracts for target '{}'",
            target_label
        )),
    }
}

fn register_dependency_contract_cleanup(
    attempt: Option<&mut PipelineAttemptContext>,
    graph: &RunningGraph,
) {
    let Some(attempt) = attempt else {
        return;
    };
    let mut scope = attempt.cleanup_scope();
    for dep in graph.deps() {
        scope.register_kill_child_process(dep.child.id(), format!("dep:{}", dep.alias));
    }
}

/// Register the per-run ephemeral state directories auto-provisioned for a
/// headless run (#700) with the run-attempt cleanup scope so they are removed
/// when the run ends. Ephemeral state must not survive the run; the auto
/// provisioner places it under `~/.ato/runs/<token>` and returns the per-run
/// roots here for removal.
fn register_headless_ephemeral_state_cleanup(
    attempt: Option<&mut PipelineAttemptContext>,
    ephemeral_dirs: &[PathBuf],
) {
    if ephemeral_dirs.is_empty() {
        return;
    }
    let Some(attempt) = attempt else {
        return;
    };
    let mut scope = attempt.cleanup_scope();
    for dir in ephemeral_dirs {
        scope.register_remove_dir(dir.clone());
    }
}

fn register_capsule_process_cleanup(
    attempt: &mut Option<&mut PipelineAttemptContext>,
    process: &crate::executors::source::CapsuleProcess,
    service_name: &str,
) {
    let Some(attempt) = attempt.as_deref_mut() else {
        return;
    };
    let mut scope = attempt.cleanup_scope();
    scope.register_kill_child_process(process.child.id(), service_name.to_string());
    if let Some(pid) = process.workload_pid {
        scope.register_kill_child_process(pid, format!("{}:workload", service_name));
    }
}

fn dependency_contract_session_snapshot(
    session_id: &str,
    consumer_pid: i32,
    graph: &RunningGraph,
) -> crate::runtime::process::DependencyContractSessionSnapshot {
    let providers = graph
        .deps()
        .iter()
        .map(|dep| {
            let runtime_export_keys = dep.runtime_exports.keys().cloned().collect();
            crate::runtime::process::DependencyContractProcessInfo {
                alias: dep.alias.clone(),
                pid: dep.child.id() as i32,
                state_dir: dep.state_dir.clone(),
                resolved: dep.resolved.clone(),
                allocated_port: dep.allocated_port,
                log_path: dep.log_path.clone(),
                runtime_export_keys,
            }
        })
        .collect();
    crate::runtime::process::DependencyContractSessionSnapshot {
        session_id: session_id.to_string(),
        consumer_pid,
        providers,
    }
}

pub(crate) fn persist_background_dependency_contracts(
    session_id: &str,
    consumer_pid: i32,
    dep_contracts: Option<&DependencyContractGuard>,
) -> Result<()> {
    let Some(graph) = dep_contracts.and_then(DependencyContractGuard::graph) else {
        return Ok(());
    };
    if graph.deps().is_empty() {
        return Ok(());
    }
    let snapshot = dependency_contract_session_snapshot(session_id, consumer_pid, graph);
    let process_manager = crate::runtime::process::ProcessManager::new()?;
    process_manager.write_dependency_session_snapshot(session_id, &snapshot)?;
    Ok(())
}

fn detach_dependency_contracts_for_background(dep_contracts: &mut Option<DependencyContractGuard>) {
    if let Some(guard) = dep_contracts.take() {
        guard.detach();
    }
}

pub(crate) async fn start_dependency_contracts_for_run(
    prepared: &PreparedRunContext,
    plan: &capsule::router::ManifestData,
    lockfile: &CapsuleLock,
) -> Result<DependencyContractGuard> {
    let consumer =
        router::CompatManifestBridge::from_manifest_value(prepared.bridge_manifest.as_toml())
            .context("failed to parse consumer manifest for dependency contracts")?
            .manifest_model()
            .clone();
    let mut providers_for_lock = BTreeMap::new();
    let mut providers_for_run = BTreeMap::new();

    for locked in lockfile
        .capsule_dependencies
        .iter()
        .filter(|dependency| dependency.contract.is_some())
    {
        let manifest_path =
            crate::external_capsule::cache::ensure_runtime_tree_for_dependency(locked)
                .await
                .with_context(|| {
                    format!(
                        "failed to materialize dependency-contract provider '{}'",
                        locked.name
                    )
                })?;
        let loaded = capsule::manifest::load_manifest_with_validation_mode(
            &manifest_path,
            prepared.validation_mode,
        )
        .with_context(|| {
            format!(
                "failed to parse provider manifest for dependency '{}'",
                locked.name
            )
        })?;
        let provider_root = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .context("provider manifest path has no parent")?;
        let resolved = locked_dependency_resolved_ref(locked);
        providers_for_lock.insert(
            locked.name.clone(),
            ResolvedProviderManifest {
                requested: locked.source.clone(),
                resolved: resolved.clone(),
                manifest: loaded.model.clone(),
            },
        );
        providers_for_run.insert(
            locked.name.clone(),
            OrchestratorProvider {
                manifest: loaded.model,
                provider_root,
                resolved,
            },
        );
    }

    let dependency_lock = verify_and_lock(DependencyLockInput {
        consumer: &consumer,
        providers: providers_for_lock,
    })
    .context("dependency-contract verification failed")?;
    let host_env = ProcessHostEnv;
    let redaction = Arc::new(RedactionRegistry::new());
    let graph = start_dependency_graph(OrchestratorInput {
        lock: &dependency_lock,
        providers: providers_for_run,
        consumer: &consumer,
        ato_home: capsule::common::paths::nacelle_home_dir_or_workspace_tmp(),
        parent_package_id: parent_package_id(&consumer),
        host_env: &host_env,
        redaction,
        session_pid: std::process::id() as i32,
        default_ready_timeout: Duration::from_secs(30),
        ready_probe_interval: Duration::from_millis(200),
        // Honour `[targets.<label>] needs = [...]`: only deps the
        // selected target actually requires get spawned. Frontend-only
        // runs (e.g. `--target web`) skip backend-only providers like
        // postgres, which removes the orphan-postgres collision when
        // alternating between `--target web` and the default backend.
        selected_target: Some(plan.selected_target_label().to_string()),
    })
    .map_err(|err| dependency_contract_start_error(plan.selected_target_label(), err))?;

    Ok(DependencyContractGuard::new(graph, dependency_lock))
}

fn locked_dependency_resolved_ref(locked: &capsule::lockfile::LockedCapsuleDependency) -> String {
    if let Some(digest) = locked.digest.as_deref().or(locked.sha256.as_deref()) {
        return format!("{}#{}", locked.source, digest);
    }
    if let Some(version) = locked.resolved_version.as_deref() {
        return format!("{}#version:{}", locked.source, version);
    }
    locked.source.clone()
}

fn parent_package_id(consumer: &CapsuleManifest) -> String {
    let name = consumer.name.trim();
    let version = consumer.version.trim();
    if name.is_empty() {
        "unknown".to_string()
    } else if version.is_empty() {
        name.to_string()
    } else {
        format!("{name}@{version}")
    }
}

pub(crate) fn inject_dependency_contract_env(
    mut launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
    plan: &capsule::router::ManifestData,
    lock: &DependencyLock,
    graph: &RunningGraph,
) -> Result<crate::executors::launch_context::RuntimeLaunchContext> {
    for (key, value) in plan.execution_env() {
        if !value.contains("{{deps.") {
            continue;
        }
        let (resolved, origin) = render_consumer_dependency_template(&value, lock, graph)
            .with_context(|| format!("failed to resolve dependency env '{}'", key))?;
        launch_ctx = launch_ctx.with_injected_env_with_origin(
            HashMap::from([(key, resolved)]),
            origin.unwrap_or(EnvOrigin::ManifestStatic),
        );
    }
    Ok(launch_ctx)
}

#[derive(Debug, Default, Clone)]
pub(crate) struct MissingDependencyContractEnvReport {
    pub(crate) keys: Vec<String>,
    pub(crate) schema: Vec<ConfigField>,
}

fn push_missing_dependency_contract_env(
    report: &mut MissingDependencyContractEnvReport,
    seen_missing: &mut std::collections::HashSet<String>,
    host_env: &dyn HostEnv,
    name: &str,
    label: Option<String>,
) {
    if host_env
        .get(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return;
    }
    if !seen_missing.insert(name.to_string()) {
        return;
    }

    report.keys.push(name.to_string());
    report.schema.push(ConfigField {
        name: name.to_string(),
        label,
        description: None,
        kind: ConfigKind::Secret,
        default: None,
        placeholder: None,
    });
}

pub(crate) fn collect_missing_dependency_contract_manifest_env(
    manifest_value: &toml::Value,
    host_env: &dyn HostEnv,
) -> Result<MissingDependencyContractEnvReport> {
    use capsule::foundation::types::{ParamValue, TemplateExpr, TemplateSegment, TemplatedString};

    let capsule_dependencies = manifest_external_capsule_dependencies(manifest_value)
        .context("failed to read consumer dependency declarations for dependency env preflight")?;

    let mut report = MissingDependencyContractEnvReport::default();
    let mut seen_missing: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in &capsule_dependencies {
        if entry.contract.is_none() {
            continue;
        }

        for (param_key, value) in &entry.parameters {
            let ParamValue::String(raw) = value else {
                continue;
            };
            let Ok(template) = TemplatedString::parse(raw) else {
                continue;
            };
            for segment in &template.segments {
                let TemplateSegment::Expr(TemplateExpr::Env(name)) = segment else {
                    continue;
                };
                push_missing_dependency_contract_env(
                    &mut report,
                    &mut seen_missing,
                    host_env,
                    name,
                    Some(format!(
                        "dep '{}'.parameters.{} → {{env.{}}}",
                        entry.alias, param_key, name
                    )),
                );
            }
        }

        for (cred_key, template) in &entry.credentials {
            for segment in &template.segments {
                let TemplateSegment::Expr(TemplateExpr::Env(name)) = segment else {
                    continue;
                };
                push_missing_dependency_contract_env(
                    &mut report,
                    &mut seen_missing,
                    host_env,
                    name,
                    Some(format!(
                        "dep '{}'.credentials.{} → {{env.{}}}",
                        entry.alias, cred_key, name
                    )),
                );
            }
        }
    }

    if let Some(required_env) = manifest_value
        .get("required_env")
        .and_then(toml::Value::as_array)
    {
        for value in required_env {
            let Some(name) = value.as_str().map(str::trim) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            push_missing_dependency_contract_env(
                &mut report,
                &mut seen_missing,
                host_env,
                name,
                None,
            );
        }
    }

    Ok(report)
}

pub(crate) fn preflight_dependency_contract_manifest_env(
    plan: &capsule::router::ManifestData,
    manifest_value: &toml::Value,
    host_env: &dyn HostEnv,
    action: &str,
) -> Result<()> {
    if !manifest_external_capsule_dependencies(manifest_value)
        .context("failed to read consumer dependency declarations for dependency env preflight")?
        .iter()
        .any(|dependency| dependency.contract.is_some())
    {
        return Ok(());
    }

    let report = collect_missing_dependency_contract_manifest_env(manifest_value, host_env)?;
    if report.keys.is_empty() {
        return Ok(());
    }

    let target = plan.selected_target_label();
    let message = format!(
        "missing required environment variables for dependency contracts of target '{}': {} (set them before {})",
        target,
        report.keys.join(", "),
        action
    );
    Err(
        AtoExecutionError::missing_required_env(message, report.keys, report.schema, Some(target))
            .into(),
    )
}

pub(crate) fn is_env_satisfied(
    name: &str,
    env_layers: &[&HashMap<String, String>],
    host_env: Option<&dyn HostEnv>,
) -> bool {
    env_layers.iter().any(|layer| {
        layer
            .get(name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    }) || host_env
        .and_then(|env| env.get(name))
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub(crate) fn preflight_orchestration_session_environment(
    plan: &capsule::router::ManifestData,
    manifest_value: &toml::Value,
    orchestration: &capsule::foundation::types::OrchestrationPlan,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
    host_env: &dyn HostEnv,
    action: &str,
) -> Result<()> {
    preflight_orchestration_service_required_env(
        plan,
        orchestration,
        launch_ctx,
        host_env,
        action,
    )?;
    preflight_dependency_contract_manifest_env(plan, manifest_value, host_env, action)
}

fn preflight_orchestration_service_required_env(
    plan: &capsule::router::ManifestData,
    orchestration: &capsule::foundation::types::OrchestrationPlan,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
    host_env: &dyn HostEnv,
    action: &str,
) -> Result<()> {
    let launch_env = launch_ctx.merged_env();
    let mut missing_keys: Vec<String> = Vec::new();
    let mut missing_schema: Vec<ConfigField> = Vec::new();
    let mut seen_missing = std::collections::HashSet::new();

    for service in &orchestration.services {
        let runtime = service.runtime.runtime();
        let base_env = runtime_overrides::merged_env(runtime.env.clone());
        for name in &runtime.required_env {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            if is_env_satisfied(name, &[&launch_env, &base_env], Some(host_env)) {
                continue;
            }
            if !seen_missing.insert(name.to_string()) {
                continue;
            }

            missing_keys.push(name.to_string());
            missing_schema.push(ConfigField {
                name: name.to_string(),
                label: Some(format!(
                    "service '{}'.target '{}' required_env {}",
                    service.name, runtime.target, name
                )),
                description: None,
                kind: ConfigKind::Secret,
                default: None,
                placeholder: None,
            });
        }
    }

    if missing_keys.is_empty() {
        return Ok(());
    }

    let target = plan.selected_target_label();
    let message = format!(
        "missing required environment variables for orchestration services of target '{}': {} (set them before {})",
        target,
        missing_keys.join(", "),
        action
    );
    Err(AtoExecutionError::missing_required_env(
        message,
        missing_keys,
        missing_schema,
        Some(target),
    )
    .into())
}

pub(crate) async fn setup_dependency_contracts_launch_context(
    plan: &capsule::router::ManifestData,
    prepared: &mut PreparedRunContext,
    reporter: &Arc<CliReporter>,
    launch_ctx: &mut crate::executors::launch_context::RuntimeLaunchContext,
    preflight_action: &str,
) -> Result<Option<DependencyContractGuard>> {
    let capsule_dependencies =
        manifest_external_capsule_dependencies(prepared.bridge_manifest.as_toml())
            .context("failed to read consumer dependency declarations")?;
    if !capsule_dependencies
        .iter()
        .any(|dependency| dependency.contract.is_some())
    {
        return Ok(None);
    }

    let compatibility_legacy_lock = match prepared.compatibility_legacy_lock.as_ref() {
        Some(ctx) => ctx.clone(),
        None => {
            let bridge = capsule::router::CompatManifestBridge::from_manifest_value(
                prepared.bridge_manifest.as_toml(),
            )
            .context("failed to build compatibility bridge for auto-lock")?;
            let compat_input = capsule::router::CompatProjectInput::from_bridge(
                prepared.workspace_root.clone(),
                bridge,
            )
            .context("failed to build CompatProjectInput for auto-lock")?;
            let lock_path = capsule::contract::lockfile::ensure_lockfile_for_compat_input(
                &compat_input,
                reporter.clone(),
                false,
            )
            .await
            .context("auto-lock for dependency contracts failed")?;
            let bytes = std::fs::read(&lock_path).with_context(|| {
                format!("failed to read auto-generated lock {}", lock_path.display())
            })?;
            let lock: capsule::lockfile::CapsuleLock = serde_json::from_slice(&bytes)
                .with_context(|| {
                    format!(
                        "failed to parse auto-generated lock {}",
                        lock_path.display()
                    )
                })?;
            CompatibilityLegacyLockContext {
                manifest_path: compat_input.workspace_root().join("capsule.toml"),
                path: lock_path,
                lock,
            }
        }
    };
    prepared.compatibility_legacy_lock = Some(compatibility_legacy_lock.clone());

    preflight_dependency_contract_manifest_env(
        plan,
        prepared.bridge_manifest.as_toml(),
        &ProcessHostEnv,
        preflight_action,
    )?;
    // PR-4a: bundle-derived `DependencyContracts` is the primary
    // pre-spawn gate. Legacy
    // `verify_lockfile_external_dependencies(manifest, lock)` stays
    // as a debug parity guard.
    {
        let external_dependencies = manifest_external_capsule_dependencies(&plan.manifest)?;
        let bundle = crate::application::graph_views::build_declared_only_bundle(
            &external_dependencies,
            Some(plan.manifest_path.display().to_string()),
            None,
            Vec::new(),
        );
        capsule::lockfile::verify_lockfile_against_contracts(
            &bundle.derived.dependency_contracts,
            &compatibility_legacy_lock.lock,
        )?;
        debug_assert!(
            verify_lockfile_external_dependencies(&plan.manifest, &compatibility_legacy_lock.lock,)
                .is_ok(),
            "PR-4a parity: legacy verifier disagrees with bundle-derived verifier \
             at run.rs pre-spawn gate (compatibility branch)"
        );
    }

    let guard =
        start_dependency_contracts_for_run(prepared, plan, &compatibility_legacy_lock.lock).await?;

    if let Some(graph) = guard.graph() {
        for line in graph.summary_lines() {
            eprintln!("{line}");
        }

        let mut updated = std::mem::replace(
            launch_ctx,
            crate::executors::launch_context::RuntimeLaunchContext::empty(),
        );
        updated = inject_dependency_contract_env(updated, plan, guard.lock(), graph)?;
        let dep_endpoints: Vec<String> = graph
            .deps()
            .iter()
            .filter_map(|dep| dep.allocated_port.map(|port| format!("127.0.0.1:{port}")))
            .collect();
        if !dep_endpoints.is_empty() {
            updated = updated.with_dep_endpoints(dep_endpoints);
        }
        *launch_ctx = updated;
    }

    Ok(Some(guard))
}

fn render_consumer_dependency_template(
    raw: &str,
    lock: &DependencyLock,
    graph: &RunningGraph,
) -> Result<(String, Option<EnvOrigin>)> {
    use capsule::types::{TemplateExpr, TemplateSegment, TemplatedString};

    let template = TemplatedString::parse(raw)
        .map_err(|err| anyhow::anyhow!("invalid dependency template '{raw}': {err}"))?;
    let mut out = String::new();
    let mut origin = None;
    for segment in template.segments {
        match segment {
            TemplateSegment::Literal(text) => out.push_str(&text),
            TemplateSegment::Expr(TemplateExpr::DepRuntimeExport { dep, key }) => {
                let value = graph
                    .runtime_exports(&dep)
                    .and_then(|exports| exports.get(&key))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "dependency '{}' did not provide runtime_exports.{}",
                            dep,
                            key
                        )
                    })?;
                out.push_str(value);
                origin = Some(EnvOrigin::DepRuntimeExport(dep));
            }
            TemplateSegment::Expr(TemplateExpr::DepIdentityExport { dep, key }) => {
                let value = lock
                    .entries
                    .get(&dep)
                    .and_then(|entry| entry.identity_exports.get(&key))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "dependency '{}' did not provide identity_exports.{}",
                            dep,
                            key
                        )
                    })?;
                out.push_str(value);
                if origin.is_none() {
                    origin = Some(EnvOrigin::DepIdentityExport(dep));
                }
            }
            TemplateSegment::Expr(expr) => {
                out.push_str(&format!("{{{{{expr}}}}}"));
            }
        }
    }
    Ok((out, origin))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompatibilityHostMode {
    Disabled,
    Enabled,
}

pub(crate) async fn run_install_phase<P>(
    request: &ConsumerRunRequest,
    progress: &P,
) -> Result<RunInstallPhaseResult>
where
    P: ConsumerRunProgress,
{
    progress.start(HourglassPhase::Install);

    let resolved_target = if let Some(pinned) = &request.pinned_revision_output_dir {
        // Revision-pinned launch from `ato launch`: bypass `resolve_run_target_or_install`
        // and the ~/.ato path guard. The output directory is a frozen, trusted revision
        // root written by `InstallRevisionFinalizer`.
        crate::install::support::ResolvedRunTarget {
            path: pinned.clone(),
            agent_local_root: None,
            desktop_open_path: None,
            export_request: request.export_request.clone(),
            provider_workspace: None,
            transient_workspace_root: None,
            community_submit_context: None,
        }
    } else {
        crate::install::support::resolve_run_target_or_install(
            request.target.clone(),
            request.assume_yes,
            request.provider_toolchain_requested,
            request.use_existing_toml.clone(),
            request.explicit_commit.clone(),
            request.keep_failed_artifacts,
            request.auto_fix_mode,
            request.allow_unverified,
            request.registry.as_deref(),
            request.reporter.clone(),
        )
        .await?
    };
    let manifest_outcome = crate::install::support::ensure_local_manifest_ready_for_run(
        &resolved_target,
        request.assume_yes,
        request.reporter.clone(),
    )?;
    let dependency_request = dependency_request_for_run(request, &resolved_target)?;
    let materializer = SessionDependencyMaterializer::new();
    let dependency_projection = materializer.materialize(&dependency_request)?;
    let verification = materializer.verify(&dependency_projection)?;
    if !verification.ok {
        anyhow::bail!("{}", verification.messages.join("; "));
    }

    let detail = match manifest_outcome {
        LocalRunManifestPreparationOutcome::Ready => {
            "target resolved and manifest ready; using isolated run workspace"
        }
        LocalRunManifestPreparationOutcome::CreatedManualManifest => {
            "manifest created; stopping before prepare"
        }
    };
    progress.ok(HourglassPhase::Install, detail);

    Ok(RunInstallPhaseResult {
        resolved_target,
        manifest_outcome,
        dependency_projection,
    })
}

fn dependency_request_for_run(
    request: &ConsumerRunRequest,
    resolved_target: &ResolvedRunTarget,
) -> Result<DependencyMaterializationRequest> {
    let source_root = resolved_target
        .provider_workspace
        .as_ref()
        .map(|workspace| workspace.workspace_root.clone())
        .unwrap_or_else(|| resolved_target.path.clone());
    let workspace_root = source_root
        .parent()
        .map(Path::to_path_buf)
        .filter(|_| source_root.is_file())
        .unwrap_or_else(|| source_root.clone());
    let ecosystem = resolved_target
        .provider_workspace
        .as_ref()
        .map(|workspace| workspace.target.provider.as_str().to_string())
        .unwrap_or_else(|| "source".to_string());
    let package_manager = infer_package_manager(&workspace_root);
    let runtime = RuntimeSelection {
        name: if ecosystem == "pypi" {
            "python".to_string()
        } else if ecosystem == "npm" {
            "node".to_string()
        } else {
            "source".to_string()
        },
        version: None,
    };
    let manifests = ManifestInputs {
        lockfile_digest: first_digest(
            &workspace_root,
            &[
                "package-lock.json",
                "pnpm-lock.yaml",
                "yarn.lock",
                "bun.lock",
                "bun.lockb",
                "uv.lock",
                "requirements.txt",
            ],
        )?,
        package_manifest_digest: first_digest(
            &workspace_root,
            &[
                "package.json",
                "pyproject.toml",
                "requirements.txt",
                "Cargo.toml",
            ],
        )?,
        workspace_manifest_digest: digest_file(&workspace_root.join("capsule.toml"))?,
        path_dependency_digest: None,
    };

    Ok(DependencyMaterializationRequest {
        session_id: if ecosystem == "source" {
            "run".to_string()
        } else {
            format!("provider-{ecosystem}")
        },
        capsule_id: request.target.to_string_lossy().to_string(),
        source_root,
        workspace_root,
        ecosystem,
        package_manager,
        package_manager_version: None,
        runtime,
        manifests,
        policies: InstallPolicies {
            lifecycle_script_policy: "sandbox".to_string(),
            registry_policy: "default".to_string(),
            network_policy: request.enforcement.clone(),
            env_allowlist_digest: None,
        },
        platform: PlatformTriple::current(),
        cache_strategy: request.cache_strategy,
        attestation_strategy: AttestationStrategy::None,
        // Trusted, request-scoped install-lifecycle identity (set only by
        // `ato launch`). Threaded as typed data so the materialized session
        // record is stamped without any reliance on process env / globals.
        install_lifecycle_context: request.install_lifecycle_context.clone(),
    })
}

fn first_digest(root: &Path, names: &[&str]) -> Result<Option<String>> {
    for name in names {
        if let Some(digest) = digest_file(&root.join(name))? {
            return Ok(Some(digest));
        }
    }
    Ok(None)
}

fn infer_package_manager(root: &Path) -> Option<String> {
    [
        ("pnpm-lock.yaml", "pnpm"),
        ("package-lock.json", "npm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
        ("uv.lock", "uv"),
        ("requirements.txt", "pip"),
    ]
    .into_iter()
    .find_map(|(file, manager)| root.join(file).exists().then(|| manager.to_string()))
}

fn run_validation_mode(preview_mode: bool) -> capsule::types::ValidationMode {
    if preview_mode {
        capsule::types::ValidationMode::Preview
    } else {
        capsule::types::ValidationMode::Strict
    }
}

/// Collect per-endpoint preferred-port requests from `capsule://` port query
/// inputs (#548). Keyed by logical endpoint name (`main` for the bare `port`):
///
/// - a value parsing as `u16` → [`PortPreference::Concrete`] (the requested
///   preferred port).
/// - the literal `"auto"` → [`PortPreference::Auto`], an *explicit* "no concrete
///   preferred port" that suppresses the env-`PORT` fallback for that endpoint at
///   admission time (so `port=auto` with `PORT` in the service env creates no
///   concrete claim — it lands on the runtime's OS auto-assign path).
///
/// Other `Literal` values (non-numeric, out-of-range) are dropped: parsing
/// already rejects them, so they should never reach here.
fn collect_port_preferences(
    inputs: &[capsule::installed_state::LaunchConditionInput],
) -> HashMap<String, crate::executors::launch_context::PortPreference> {
    use crate::executors::launch_context::PortPreference;
    use capsule::installed_state::{LaunchConditionInputKind, LaunchConditionInputValue};
    let mut prefs = HashMap::new();
    for input in inputs {
        if input.kind != LaunchConditionInputKind::Port {
            continue;
        }
        let LaunchConditionInputValue::Literal(value) = &input.value else {
            continue;
        };
        let preference = if value == "auto" {
            PortPreference::Auto
        } else if let Ok(port) = value.parse::<u16>() {
            PortPreference::Concrete(port)
        } else {
            continue;
        };
        prefs.insert(input.key.clone(), preference);
    }
    prefs
}

/// Build the runtime-owned data-directory env for a sandboxed source run.
///
/// Sets `ATO_DATA_DIR` to the writable session guest dir and `DATABASE_PATH` to
/// `<guest_dir>/app.db`, but ONLY for keys that `already_set` reports as absent
/// — the runtime never overrides a value the capsule manifest or the user
/// provided. These keys are re-applied past the sandbox `--clearenv` by the
/// nacelle launcher's runtime allowlist.
fn sandbox_session_data_env(
    guest_dir: &str,
    already_set: impl Fn(&str) -> bool,
) -> std::collections::HashMap<String, String> {
    let mut env = std::collections::HashMap::new();
    if !already_set("ATO_DATA_DIR") {
        env.insert("ATO_DATA_DIR".to_string(), guest_dir.to_string());
    }
    if !already_set("DATABASE_PATH") {
        env.insert("DATABASE_PATH".to_string(), format!("{guest_dir}/app.db"));
    }
    env
}

/// Pick the path the session-data env (`ATO_DATA_DIR` / `DATABASE_PATH`) must
/// reference for a sandboxed source run.
///
/// On mount-namespace backends (Linux bwrap) the host dir is remapped to the
/// guest path, so the env uses the guest path. The macOS seatbelt backend has
/// no mount namespace — the child sees the host filesystem and the injected
/// mount becomes a write-allow rule for the host path, not a remap — so the env
/// must reference the host dir directly. Otherwise a stateful capsule tries to
/// create the guest root (`/runs`) on the read-only host fs and exits before
/// readiness (#628).
fn sandbox_session_data_env_dir(guest_dir: &str, host_dir: &std::path::Path) -> String {
    if cfg!(target_os = "macos") {
        host_dir.to_string_lossy().to_string()
    } else {
        guest_dir.to_string()
    }
}

pub(crate) async fn run_prepare_phase<P>(
    request: &ConsumerRunRequest,
    progress: &P,
    mut attempt: Option<&mut PipelineAttemptContext>,
) -> Result<RunPipelineState>
where
    P: ConsumerRunProgress,
{
    progress.start(HourglassPhase::Prepare);

    let workspace_root = if let Some(authoritative_input) = request.authoritative_input.as_ref() {
        authoritative_input.workspace_root.clone()
    } else if request.target.is_dir() {
        request.target.clone()
    } else {
        request
            .target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| request.target.clone())
    };
    let manifest_path = workspace_root.join("capsule.toml");
    let preview_session = if manifest_path.exists() {
        preview::load_preview_session_for_manifest(&manifest_path)?
    } else {
        None
    };
    let preview_mode = request.preview_mode || preview_session.is_some();
    let use_progressive_ui = request.verbose
        && crate::progressive_ui::can_use_progressive_ui(false)
        && !request.background;
    let source_label = preview_session
        .as_ref()
        .map(|session| session.target_reference.clone())
        .unwrap_or_else(|| workspace_root.display().to_string());

    if use_progressive_ui {
        crate::progressive_ui::show_run_intro(&source_label)?;
    }

    let validation_mode = run_validation_mode(preview_mode);
    let effective_target_label = request
        .export_request
        .as_ref()
        .map(|export| export.target_label.as_str())
        .or(request.target_label.as_deref());
    let mut prepared = PreparedRunContext::from_authoritative_input(
        request.authoritative_input.as_ref(),
        &workspace_root,
        validation_mode,
        effective_target_label,
    )?;
    // Source of truth for installed-app launch identity: the explicit
    // install-lifecycle context `ato launch` passes through the request — not
    // the thread-local. Threaded onto the prepared context so the (async)
    // launch-context resolution stamps it without crossing a thread-local
    // boundary (#508).
    prepared.install_profile_key = request
        .install_lifecycle_context
        .as_ref()
        .map(|ctx| ctx.install_profile_key.clone());
    // Installed-app relaunch preflight (#508): read the Installed-State DB ledger
    // (the SOT) and block before launch if a required condition is unsatisfied.
    // Gated on the install identity, so `ato run` / non-installed launches are
    // untouched. Runs here in the prepare phase, before any executor.
    //
    // `capsule://…?<query>` launch URLs (`ato launch capsule://…`) supply
    // launch-condition inputs (which grant/binding to try); they are overlaid
    // onto the in-memory claims before resolution. They are inputs, not proof —
    // the resolver still checks the DB registry before admission. Empty for
    // `ato run` and `ato launch <ipk>`.
    crate::adapters::runtime::relaunch_preflight::run_relaunch_preflight(
        request.install_lifecycle_context.as_ref().map(|ctx| {
            (
                ctx.install_profile_key.as_str(),
                ctx.install_revision_id.as_str(),
            )
        }),
        &request.capsule_launch_inputs,
    )?;
    let mut state_source_overrides =
        if let Some(authoritative_input) = request.authoritative_input.as_ref() {
            let mut overrides = authoritative_input
                .effective_state
                .state_source_overrides
                .clone();
            // The lock path does not run `resolve_state_source_overrides_managed`
            // (that lives on the manifest branch below), so apply the managed
            // auto-bind here too: any persistent `[state.*]` the lock left unbound
            // is bound under `managed_state_root`. Existing (explicit/lock)
            // bindings always win. `--managed-state-root` is an explicit contract
            // for non-interactive runner execution, so a manifest that cannot be
            // read is a hard error — never a silent skip that surfaces later as a
            // confusing unbound-state failure.
            if let Some(root) = request.managed_state_root.as_deref() {
                let capsule_toml = authoritative_input.workspace_root.join("capsule.toml");
                let loaded = capsule::manifest::load_manifest_with_validation_mode(
                    &capsule_toml,
                    validation_mode,
                )
                .with_context(|| {
                    format!(
                        "failed to load {} to apply --managed-state-root",
                        capsule_toml.display()
                    )
                })?;
                let managed = resolve_state_source_overrides_managed(
                    &loaded.model,
                    &request.state_bindings,
                    None,
                    Some(root),
                    effective_target_label,
                )?;
                for (key, value) in managed {
                    overrides.entry(key).or_insert(value);
                }
            }
            overrides
        } else {
            HashMap::new()
        };
    // Headless / Connected Runner state auto-provisioning (#687). `ato run
    // <source> --sandbox` (what the runner spawns) carries no `--state`
    // binding, so a recipe declaring a `[state.*]` block would otherwise
    // hard-error on the unbound persistent state. Mirror the desktop path by
    // auto-provisioning a per-source `~/.ato/state/run/...` directory for any
    // declared state that is still unbound. Only the authoritative-input path
    // is provisioned here; the non-authoritative branch provisions against its
    // freshly loaded manifest below.
    if request.sandbox_mode
        && request.authoritative_input.is_some()
        && manifest_path.exists()
        && let Ok(loaded) =
            capsule::manifest::load_manifest_with_validation_mode(&manifest_path, validation_mode)
    {
        let normalized_source_ref =
            headless_normalized_source_ref(request, preview_session.as_ref());
        let profile_id = request
            .install_lifecycle_context
            .as_ref()
            .map(|ctx| ctx.install_profile_id.as_str());
        let runner_namespace = runtime_overrides::scoped_id_override();
        let key_inputs = HeadlessStateKeyInputs {
            normalized_source_ref: &normalized_source_ref,
            selected_target_label: effective_target_label.unwrap_or("default"),
            profile_id,
            runner_namespace: runner_namespace.as_deref(),
            workspace_root_for_fallback: &workspace_root,
        };
        let outcome = auto_provision_headless_state_overrides(
            &loaded.model,
            &state_source_overrides,
            &key_inputs,
        )?;
        state_source_overrides = outcome.overrides;
        register_headless_ephemeral_state_cleanup(attempt.as_deref_mut(), &outcome.ephemeral_dirs);
    }
    let mut decision = if let Some(authoritative_input) = request.authoritative_input.as_ref() {
        let mut decision = capsule::router::route_lock_with_state_overrides(
            &authoritative_input.lock_path,
            &authoritative_input.lock,
            &authoritative_input.materialization_root,
            router::ExecutionProfile::Dev,
            effective_target_label,
            state_source_overrides,
        )?;
        decision.plan.workspace_root = authoritative_input.workspace_root.clone();
        // Patch compat_manifest from capsule.toml when present so that v0.3-specific
        // fields (build_command, language, package_type, etc.) are available to
        // run_v03_lifecycle_steps. The inferred lock used by route_lock_with_state_overrides
        // does not preserve these fields, causing the build step to be skipped (#301).
        //
        // Also patch `prepared.bridge_manifest` from the same capsule.toml so the
        // top-level `[dependencies.<alias>]` block survives into
        // `manifest_external_capsule_dependencies` below — the lock-derived
        // bridge_manifest seeded by `PreparedRunContext::from_authoritative_input`
        // does not preserve the dependency-contract grammar, so without this
        // local-path runs of a capsule with `[dependencies.db]` etc. would skip
        // dep_contracts startup entirely and the consumer would see literal
        // `{{deps.db.runtime_exports.DATABASE_URL}}` instead of the resolved
        // value (#22). The github-URL fetch path patches bridge_manifest via a
        // separate code path (the relocated checkout reseeds the prepared
        // context downstream); local-path runs go straight from the install
        // phase's authoritative_input into this branch.
        let capsule_toml = authoritative_input.workspace_root.join("capsule.toml");
        if capsule_toml.exists()
            && let Ok(loaded) = capsule::manifest::load_manifest_with_validation_mode(
                &capsule_toml,
                validation_mode,
            )
        {
            let raw = reconcile_compat_manifest_targets(
                &loaded.raw,
                decision.plan.selected_target_label(),
            );
            if let Ok(bridge) = capsule::router::CompatManifestBridge::from_manifest_value(&raw) {
                decision.plan.compat_manifest = Some(bridge);
            }
            if let Ok(value) = toml::from_str::<toml::Value>(&loaded.raw_text) {
                prepared.bridge_manifest = DerivedBridgeManifest::new(value);
            }
        }
        decision
    } else {
        let loaded_manifest =
            capsule::manifest::load_manifest_with_validation_mode(&manifest_path, validation_mode)?;
        prepared.bridge_manifest = DerivedBridgeManifest::new(
            toml::from_str(&loaded_manifest.raw_text)
                .unwrap_or_else(|_| loaded_manifest.raw.clone()),
        );
        prepared.engine_override_declared = loaded_manifest.raw.get("engine").is_some();
        let manifest = loaded_manifest.model.clone();
        if manifest.schema_version.trim() == MANIFEST_SCHEMA_VERSION
            && manifest.capsule_type == CapsuleType::Library
        {
            anyhow::bail!(
                "schema_version=0.3 type=library package cannot be started with `ato run`"
            );
        }
        let mut state_source_overrides =
            resolve_explicit_or_auto_state_source_overrides(&manifest, request)?;
        // #731: when the runner supplies a server-managed state root, bind any
        // persistent `[state.*]` left unbound by explicit `--state` under it.
        // Mirrors the lock path above; explicit bindings always win.
        if let Some(root) = request.managed_state_root.as_deref() {
            let managed = resolve_state_source_overrides_managed(
                &manifest,
                &request.state_bindings,
                None,
                Some(root),
                effective_target_label,
            )?;
            for (key, value) in managed {
                state_source_overrides.entry(key).or_insert(value);
            }
        }
        // Headless / Connected Runner state auto-provisioning (#687): fill in
        // a `~/.ato/state/run/...` directory for any declared state still
        // unbound after the explicit `--state` and managed bindings were
        // applied. Keyed on a stable source-derived id (#700), with ephemeral
        // state routed to a per-run cleanup-scoped dir.
        if request.sandbox_mode {
            let normalized_source_ref =
                headless_normalized_source_ref(request, preview_session.as_ref());
            let profile_id = request
                .install_lifecycle_context
                .as_ref()
                .map(|ctx| ctx.install_profile_id.as_str());
            let runner_namespace = runtime_overrides::scoped_id_override();
            let key_inputs = HeadlessStateKeyInputs {
                normalized_source_ref: &normalized_source_ref,
                selected_target_label: effective_target_label.unwrap_or("default"),
                profile_id,
                runner_namespace: runner_namespace.as_deref(),
                workspace_root_for_fallback: &workspace_root,
            };
            let outcome = auto_provision_headless_state_overrides(
                &manifest,
                &state_source_overrides,
                &key_inputs,
            )?;
            state_source_overrides = outcome.overrides;
            register_headless_ephemeral_state_cleanup(
                attempt.as_deref_mut(),
                &outcome.ephemeral_dirs,
            );
        }
        capsule::router::route_manifest_with_state_overrides_and_validation_mode(
            &manifest_path,
            router::ExecutionProfile::Dev,
            effective_target_label,
            state_source_overrides,
            validation_mode,
        )?
    };
    prepared.execution_override =
        build_execution_override(request, decision.plan.selected_target_label());
    if decision
        .plan
        .execution_package_type()
        .is_some_and(|value| value.eq_ignore_ascii_case("library"))
    {
        anyhow::bail!(
            "schema_version=0.3 type=library package '{}' cannot be started with `ato run`",
            decision.plan.selected_target_label()
        );
    }

    let preflight_manifest = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok());
    run_external_service_preflight(
        preflight_manifest
            .as_ref()
            .unwrap_or_else(|| prepared.bridge_manifest.as_toml()),
    )
    .await?;

    let capsule_dependencies = if prepared
        .bridge_manifest
        .as_toml()
        .get("targets")
        .and_then(|value| value.as_table())
        .is_some()
    {
        manifest_external_capsule_dependencies(prepared.bridge_manifest.as_toml())?
    } else {
        Vec::new()
    };
    let has_legacy_external_dependencies = capsule_dependencies
        .iter()
        .any(|dependency| dependency.contract.is_none());
    let has_dependency_contracts = capsule_dependencies
        .iter()
        .any(|dependency| dependency.contract.is_some());
    let mut external_capsules = None;
    if has_legacy_external_dependencies {
        if request.background {
            anyhow::bail!("external capsule dependencies do not support --background yet");
        }
        let compatibility_legacy_lock =
            prepared.compatibility_legacy_lock.as_ref().ok_or_else(|| {
                AtoExecutionError::lock_incomplete(
                    "external capsule dependencies require capsule.lock.json",
                    Some(CAPSULE_LOCK_FILE_NAME),
                )
            })?;
        // PR-4a: bundle-derived primary, legacy parity in debug.
        {
            let external_dependencies =
                manifest_external_capsule_dependencies(&decision.plan.manifest)?;
            let bundle = crate::application::graph_views::build_declared_only_bundle(
                &external_dependencies,
                Some(decision.plan.manifest_path.display().to_string()),
                None,
                Vec::new(),
            );
            capsule::lockfile::verify_lockfile_against_contracts(
                &bundle.derived.dependency_contracts,
                &compatibility_legacy_lock.lock,
            )?;
            debug_assert!(
                verify_lockfile_external_dependencies(
                    &decision.plan.manifest,
                    &compatibility_legacy_lock.lock,
                )
                .is_ok(),
                "PR-4a parity: legacy verifier disagrees with bundle-derived verifier \
                 at run.rs pre-spawn gate (external-capsules branch)"
            );
        }
        external_capsules = Some(
            crate::external_capsule::start_external_capsules(
                &decision.plan,
                &compatibility_legacy_lock.lock,
                &request.inject_bindings,
                request.reporter.clone(),
                &crate::external_capsule::ExternalCapsuleOptions {
                    enforcement: request.enforcement.clone(),
                    sandbox_mode: request.sandbox_mode,
                    dangerously_skip_permissions: request.dangerously_skip_permissions,
                    assume_yes: request.assume_yes,
                },
            )
            .await?,
        );
    }
    let injected_data =
        crate::data_injection::resolve_and_record(&decision.plan, &request.inject_bindings).await?;
    let launch_ctx =
        target_runner::resolve_launch_context(&decision.plan, &prepared, &request.reporter).await?;
    let mut launch_ctx = if request.effective_cwd.is_some() {
        launch_ctx.with_effective_cwd_override(request.effective_cwd().to_path_buf())
    } else {
        launch_ctx.with_effective_cwd(request.effective_cwd().to_path_buf())
    }
    // workspace_root is the materialized capsule root for this run.
    // The host source executor uses it to discriminate caller_cwd
    // (user's pwd) vs. execution_cwd (process cwd): caller_cwd is
    // promoted to process cwd only if it lives inside this root,
    // so `ato run github.com/...` (caller cwd unrelated to the
    // fetched workspace) correctly cd's into LaunchSpec.working_dir
    // rather than the user's terminal pwd. See
    // executors::source::resolve_host_execution_cwd.
    .with_workspace_root(prepared.workspace_root.clone())
    .with_injected_env(injected_data.env)
    .with_injected_mounts(injected_data.mounts);

    // #508/#549: resolve SecretStore-backed launch-condition grants into a
    // dedicated, receipt-excluded secret env channel — after relaunch preflight
    // admission and before spawn. Gated on an installed identity plus at least one
    // `secret.*=grant:<id>` OR sensitive `env.*=grant:<id>` input, so `ato run` and
    // `ato launch <ipk>` open no DB. A grant that exists but has no stored value
    // blocks the launch (typed).
    if let Some(lifecycle) = request.install_lifecycle_context.as_ref() {
        let has_secret_grant = request.capsule_launch_inputs.iter().any(|input| {
            matches!(
                input.kind,
                capsule::installed_state::LaunchConditionInputKind::Secret
                    | capsule::installed_state::LaunchConditionInputKind::Env
            ) && matches!(
                input.value,
                capsule::installed_state::LaunchConditionInputValue::Grant(_)
            )
        });
        if has_secret_grant {
            let db = capsule::installed_state::InstalledStateDb::open_default()
                .context("open installed-state DB for secret injection")?;
            let secret_env = crate::adapters::runtime::secret_injection::resolve_secret_injection(
                &db,
                &lifecycle.install_profile_key,
                Some(&lifecycle.install_revision_id),
                &request.capsule_launch_inputs,
                &crate::adapters::runtime::secret_injection::SecretStoreValueStore,
            )?;
            launch_ctx = launch_ctx.with_secret_env(secret_env);
        }
    }

    // #508: materialize SecretStore-analog state bindings into a dedicated,
    // receipt-excluded mount channel — after relaunch preflight admission and
    // before spawn. Gated on an installed identity plus at least one
    // `state.*=binding:<id>` input, so `ato run` and `ato launch <ipk>` open no DB.
    // A binding that is admitted but whose target was never recorded blocks the
    // launch (typed). The bound host path reaches the runtime on `state_mounts`,
    // never `injected_mounts` (which the receipt observes), so it never enters the
    // execution receipt / session record / logs.
    if let Some(lifecycle) = request.install_lifecycle_context.as_ref() {
        let has_state_binding = request.capsule_launch_inputs.iter().any(|input| {
            input.kind == capsule::installed_state::LaunchConditionInputKind::State
                && matches!(
                    input.value,
                    capsule::installed_state::LaunchConditionInputValue::Binding(_)
                )
        });
        if has_state_binding {
            let db = capsule::installed_state::InstalledStateDb::open_default()
                .context("open installed-state DB for state binding materialization")?;
            let state_mounts =
                crate::adapters::runtime::state_binding_injection::resolve_state_binding_materialization(
                    &db,
                    &lifecycle.install_profile_key,
                    Some(&lifecycle.install_revision_id),
                    &request.capsule_launch_inputs,
                )?;
            launch_ctx = launch_ctx.with_state_mounts(state_mounts);
        }
    }

    // #548: carry `capsule://…?port[.<endpoint>]=<n>` query inputs as per-endpoint
    // preferred ports so the web-service port admission picks them before
    // consulting the claim ledger. Gated on an installed identity, so `ato run`
    // (no install lifecycle) is untouched. `port=auto` (the literal `"auto"`) is
    // carried as `PortPreference::Auto` — an *explicit* "no concrete preferred
    // port" that suppresses the env-`PORT` fallback for that endpoint at
    // admission time, so it never becomes a concrete claim and the runtime uses
    // its OS auto-assign path.
    if request.install_lifecycle_context.is_some() {
        let port_preferences = collect_port_preferences(&request.capsule_launch_inputs);
        if !port_preferences.is_empty() {
            launch_ctx = launch_ctx.with_port_preferences(port_preferences);
        }
    }
    if let Some(external_capsules) = external_capsules.as_ref() {
        for (dependency, env) in external_capsules.caller_envs() {
            launch_ctx = launch_ctx.with_injected_env_with_origin(
                env.clone(),
                EnvOrigin::DepRuntimeExport(dependency),
            );
        }
    }
    let mut dep_contracts = None;
    if has_dependency_contracts {
        let guard = setup_dependency_contracts_launch_context(
            &decision.plan,
            &mut prepared,
            &request.reporter,
            &mut launch_ctx,
            "running the capsule",
        )
        .await?;
        if let Some(guard) = guard {
            if let Some(graph) = guard.graph() {
                register_dependency_contract_cleanup(attempt.as_deref_mut(), graph);
            }
            dep_contracts = Some(guard);
        }
    }

    if request.sandbox_mode && !request.dangerously_skip_permissions {
        let sandbox_grants = resolve_sandbox_grants(request, &decision.plan.manifest_dir)?;
        validate_sandbox_grants_best_effort(request, &sandbox_grants)?;
        launch_ctx = launch_ctx.with_injected_mounts(
            sandbox_grants
                .into_iter()
                .map(|grant| InjectedMount {
                    source: grant.source_path,
                    target: grant.guest_target.to_string_lossy().to_string(),
                    readonly: grant.access.readonly(),
                })
                .collect(),
        );

        // Writable per-run session data directory. Sandboxed source runs mount
        // the capsule at /app read-only, so a stateful capsule (e.g. one that
        // writes SQLite) has nowhere to persist. Mount a fresh per-run host dir
        // at the guest path /runs/ato/session — chosen so it classifies as
        // SessionLocal (ephemeral) rather than PersistentState, keeping the
        // receipt honest — and point the common data-path env vars at it ONLY
        // when the capsule/user has not already set them. The dir is ephemeral
        // (registered for run cleanup); it lives OUTSIDE the materialized source
        // tree so it does not perturb the source-tree hash.
        const SESSION_DATA_GUEST: &str = "/runs/ato/session";
        let host_session_dir = capsule::common::paths::ato_runs_dir()
            .join("session-data")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or(0)
            ));
        std::fs::create_dir_all(&host_session_dir).with_context(|| {
            format!(
                "Failed to create sandbox session data dir: {}",
                host_session_dir.display()
            )
        })?;
        if let Some(attempt) = attempt.as_mut() {
            let mut scope = attempt.cleanup_scope();
            scope.register_remove_dir(host_session_dir.clone());
        }
        // The data-path env (ATO_DATA_DIR / DATABASE_PATH) must reference the
        // path the workload actually sees at runtime — the guest path under a
        // mount namespace (Linux bwrap), the host path under macOS seatbelt
        // which has none. See `sandbox_session_data_env_dir` (#628).
        let session_data_env_dir =
            sandbox_session_data_env_dir(SESSION_DATA_GUEST, &host_session_dir);
        launch_ctx = launch_ctx.with_injected_mounts(vec![InjectedMount {
            source: host_session_dir,
            target: SESSION_DATA_GUEST.to_string(),
            readonly: false,
        }]);

        // Inject data-path env only when neither the capsule manifest env nor an
        // earlier injection already provides it — never override user/capsule.
        let plan_env = decision.plan.execution_env();
        let merged_env = launch_ctx.merged_env();
        let session_env = sandbox_session_data_env(&session_data_env_dir, |key| {
            plan_env.contains_key(key) || merged_env.contains_key(key)
        });
        if !session_env.is_empty() {
            launch_ctx = launch_ctx.with_injected_env(session_env);
        }
    }
    let mut agent_attempted = false;

    let provisioning_outcome = provisioner::run_auto_provisioning_phase(
        &decision.plan,
        &launch_ctx,
        request.reporter.clone(),
        &AutoProvisioningOptions {
            preview_mode,
            background: request.background,
        },
    )
    .await?;
    if use_progressive_ui
        && let Some(audit_reporter) =
            provisioner::AuditReporter::from_outcome(&provisioning_outcome)
    {
        let body = audit_reporter.body();
        if !body.is_empty() {
            crate::progressive_ui::show_note(audit_reporter.title(), body)?;
        }
    }
    launch_ctx = launch_ctx
        .with_injected_env(provisioning_outcome.additional_env)
        .with_injected_mounts(provisioning_outcome.additional_mounts);

    if let Some(shadow_workspace) = provisioning_outcome.shadow_workspace.as_ref() {
        if let Some(attempt) = attempt.as_mut() {
            let mut scope = attempt.cleanup_scope();
            scope.register_remove_dir(shadow_workspace.root_dir.clone());
        }

        debug!(
            issue_count = provisioning_outcome.plan.issues.len(),
            action_count = provisioning_outcome.plan.actions.len(),
            shadow_root = %shadow_workspace.root_dir.display(),
            audit_path = %shadow_workspace.audit_path.display(),
            shadow_manifest = shadow_workspace.manifest_path.as_ref().map(|path| path.display().to_string()),
            "Auto-provisioning shadow workspace prepared"
        );

        if let Some(shadow_manifest_path) = shadow_workspace.manifest_path.as_ref() {
            if use_progressive_ui {
                crate::progressive_ui::show_step(
                    "Auto-provisioning: rerouting execution through the shadow workspace",
                )?;
            }
            // Save before decision is moved — needed to re-read capsule.toml below (#301).
            let pre_reroute_workspace_root = decision.plan.workspace_root.clone();
            (decision, launch_ctx, prepared) = reroute_auto_provisioned_execution(
                decision,
                launch_ctx,
                &prepared,
                request.reporter.clone(),
                preview_mode,
                shadow_manifest_path,
            )
            .await?;
            // The shadow manifest is derived from the inferred lock which does not carry
            // build_command (it is not stored in the lock schema). Re-read capsule.toml
            // from the original workspace and patch compat_manifest so that
            // run_v03_lifecycle_steps sees the build step (#301).
            let capsule_toml = pre_reroute_workspace_root.join("capsule.toml");
            if capsule_toml.exists()
                && let Ok(loaded) = capsule::manifest::load_manifest_with_validation_mode(
                    &capsule_toml,
                    validation_mode,
                )
            {
                let raw = reconcile_compat_manifest_targets(
                    &loaded.raw,
                    decision.plan.selected_target_label(),
                );
                if let Ok(bridge) = capsule::router::CompatManifestBridge::from_manifest_value(&raw)
                {
                    decision.plan.compat_manifest = Some(bridge);
                }
            }
        }
    }

    if let Some((rerouted_decision, rerouted_launch_ctx, rerouted_prepared)) =
        maybe_run_agent_setup(
            request,
            &decision,
            &launch_ctx,
            &prepared,
            preview_mode,
            use_progressive_ui,
            &mut agent_attempted,
            "force",
            None,
            matches!(request.agent_mode, RunAgentMode::Force),
        )
        .await?
    {
        decision = rerouted_decision;
        launch_ctx = rerouted_launch_ctx;
        prepared = rerouted_prepared;
    }

    progress.ok(
        HourglassPhase::Prepare,
        "manifest and launch context resolved",
    );

    Ok(RunPipelineState {
        preview_session,
        preview_mode,
        use_progressive_ui,
        prepared,
        decision,
        launch_ctx,
        external_capsules,
        dep_contracts,
        agent_attempted,
        derived_execution: None,
        compatibility_host_mode: None,
        native_nacelle: None,
        build_observation: None,
        build_decision_kind: None,
        receipt_graph_id_sink: None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalServiceMode {
    ReuseIfPresent,
    Managed,
    RequiredExternal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalServiceHealthcheckKind {
    Http,
    Tcp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalServiceHealthcheck {
    kind: ExternalServiceHealthcheckKind,
    endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServiceRequiredAsset {
    OllamaModel { model: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalServiceContract {
    service_name: String,
    source_ref: String,
    mode: ExternalServiceMode,
    healthcheck: Option<ExternalServiceHealthcheck>,
    required_assets: Vec<ServiceRequiredAsset>,
}

impl ExternalServiceMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReuseIfPresent => "reuse-if-present",
            Self::Managed => "managed",
            Self::RequiredExternal => "required-external",
        }
    }
}

impl ServiceRequiredAsset {
    fn label(&self) -> String {
        match self {
            Self::OllamaModel { model } => format!("ollama-model={model}"),
        }
    }

    fn remediation_hint(&self) -> Option<String> {
        match self {
            Self::OllamaModel { model } => Some(format!("Run: ollama pull {model}")),
        }
    }
}

fn parse_external_service_mode(raw: &str) -> Option<ExternalServiceMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "reuse-if-present" => Some(ExternalServiceMode::ReuseIfPresent),
        "managed" => Some(ExternalServiceMode::Managed),
        "required-external" => Some(ExternalServiceMode::RequiredExternal),
        _ => None,
    }
}

fn parse_external_service_healthcheck(
    service_name: &str,
    source_ref: &str,
    service: &toml::value::Table,
) -> Option<ExternalServiceHealthcheck> {
    let parsed = service
        .get("healthcheck")
        .and_then(toml::Value::as_table)
        .and_then(|healthcheck| {
            let endpoint = healthcheck
                .get("url")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let kind = healthcheck
                .get("kind")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .unwrap_or("http");
            let kind = match kind.to_ascii_lowercase().as_str() {
                "http" => ExternalServiceHealthcheckKind::Http,
                "tcp" => ExternalServiceHealthcheckKind::Tcp,
                _ => return None,
            };
            Some(ExternalServiceHealthcheck {
                kind,
                endpoint: endpoint.to_string(),
            })
        });

    parsed.or_else(|| {
        if source_ref.trim().eq_ignore_ascii_case("dependency:ollama")
            || service_name.trim().eq_ignore_ascii_case("ollama")
        {
            Some(ExternalServiceHealthcheck {
                kind: ExternalServiceHealthcheckKind::Http,
                endpoint: "http://127.0.0.1:11434/api/tags".to_string(),
            })
        } else {
            None
        }
    })
}

fn parse_external_service_contracts(manifest: &toml::Value) -> Vec<ExternalServiceContract> {
    let legacy_ollama_model = manifest
        .get("bootstrap")
        .and_then(toml::Value::as_table)
        .and_then(|bootstrap| bootstrap.get("defaults"))
        .and_then(toml::Value::as_table)
        .and_then(|defaults| defaults.get("ollama_model"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    manifest
        .get("services")
        .and_then(toml::Value::as_table)
        .map(|services| {
            services
                .iter()
                .filter_map(|(service_name, service_value)| {
                    let service = service_value.as_table()?;
                    let source_ref = service
                        .get("from")
                        .and_then(toml::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())?
                        .to_string();
                    let mode = service
                        .get("mode")
                        .and_then(toml::Value::as_str)
                        .and_then(parse_external_service_mode)?;
                    let mut required_assets = Vec::new();
                    if source_ref.eq_ignore_ascii_case("dependency:ollama")
                        && let Some(model) = legacy_ollama_model.clone()
                    {
                        required_assets.push(ServiceRequiredAsset::OllamaModel { model });
                    }

                    Some(ExternalServiceContract {
                        service_name: service_name.trim().to_string(),
                        source_ref: source_ref.clone(),
                        mode,
                        healthcheck: parse_external_service_healthcheck(
                            service_name,
                            &source_ref,
                            service,
                        ),
                        required_assets,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_preflight_service_contracts(manifest: &toml::Value) -> Vec<ExternalServiceContract> {
    parse_external_service_contracts(manifest)
}

#[cfg(test)]
fn parse_reuse_if_present_service_preflights(
    manifest: &toml::Value,
) -> Vec<ExternalServiceContract> {
    parse_preflight_service_contracts(manifest)
        .into_iter()
        .filter(|service| service.mode == ExternalServiceMode::ReuseIfPresent)
        .collect()
}

fn service_preflight_header(summary: &str, service: &ExternalServiceContract) -> String {
    format!(
        "{summary}\nservice: {}\nmode: {}\nsource: {}",
        service.service_name,
        service.mode.as_str(),
        service.source_ref
    )
}

fn missing_healthcheck_message(service: &ExternalServiceContract) -> String {
    format!(
        "{}\ndetail: no healthcheck is declared for this service mode",
        service_preflight_header("Service cannot be preflighted", service)
    )
}

fn unavailable_service_message(service: &ExternalServiceContract, endpoint: &str) -> String {
    let detail = match service.mode {
        ExternalServiceMode::ReuseIfPresent => {
            "no reusable instance is currently reachable\nStart the service and retry"
        }
        ExternalServiceMode::RequiredExternal => {
            "this service is managed outside Ato\nStart it externally and retry"
        }
        ExternalServiceMode::Managed => {
            "this service is declared as Ato-managed\nAutomatic startup is not available in this run path yet"
        }
    };

    format!(
        "{}\nhealthcheck: {}\ndetail: service is not reachable\n{}",
        service_preflight_header("Service is unavailable", service),
        endpoint,
        detail
    )
}

fn required_asset_missing_message(
    service: &ExternalServiceContract,
    asset: &ServiceRequiredAsset,
) -> String {
    let mut message = format!(
        "{}\nasset: {}\ndetail: a required service asset is missing",
        service_preflight_header("Required service asset is missing", service),
        asset.label()
    );
    if let Some(hint) = asset.remediation_hint() {
        message.push('\n');
        message.push_str(&hint);
    }
    message
}

fn tcp_healthcheck_ready(endpoint: &str) -> bool {
    let addresses = if let Ok(url) = reqwest::Url::parse(endpoint) {
        match (url.host_str(), url.port_or_known_default()) {
            (Some(host), Some(port)) => format!("{host}:{port}").to_socket_addrs(),
            _ => return false,
        }
    } else {
        endpoint.to_socket_addrs()
    };

    let Ok(addresses) = addresses else {
        return false;
    };

    addresses
        .into_iter()
        .any(|address| TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok())
}

fn validate_required_service_assets(
    service: &ExternalServiceContract,
    payload: Option<&JsonValue>,
) -> Result<()> {
    for asset in &service.required_assets {
        match asset {
            ServiceRequiredAsset::OllamaModel { model } => {
                let Some(payload) = payload else {
                    let missing = ServiceRequiredAsset::OllamaModel {
                        model: model.clone(),
                    };
                    anyhow::bail!(required_asset_missing_message(service, &missing));
                };
                let model_present = payload
                    .get("models")
                    .and_then(JsonValue::as_array)
                    .map(|models| {
                        models.iter().any(|entry| {
                            entry
                                .get("name")
                                .or_else(|| entry.get("model"))
                                .and_then(JsonValue::as_str)
                                .map(|name| name.trim() == model)
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                if !model_present {
                    let missing = ServiceRequiredAsset::OllamaModel {
                        model: model.clone(),
                    };
                    anyhow::bail!(required_asset_missing_message(service, &missing));
                }
            }
        }
    }

    Ok(())
}

async fn run_external_service_preflight(manifest: &toml::Value) -> Result<()> {
    let preflights = parse_preflight_service_contracts(manifest);
    if preflights.is_empty() {
        return Ok(());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .context("failed to build external service preflight HTTP client")?;

    for service in preflights {
        let Some(healthcheck) = service.healthcheck.as_ref() else {
            anyhow::bail!(missing_healthcheck_message(&service));
        };

        debug!(
            service_name = %service.service_name,
            source_ref = %service.source_ref,
            healthcheck_endpoint = %healthcheck.endpoint,
            mode = service.mode.as_str(),
            "Running external service preflight"
        );

        match healthcheck.kind {
            ExternalServiceHealthcheckKind::Http => {
                let response = client
                    .get(&healthcheck.endpoint)
                    .send()
                    .await
                    .with_context(|| {
                        unavailable_service_message(&service, &healthcheck.endpoint)
                    })?;
                if !response.status().is_success() {
                    anyhow::bail!(unavailable_service_message(&service, &healthcheck.endpoint));
                }

                let payload = if service.required_assets.is_empty() {
                    None
                } else {
                    Some(
                        response
                            .json::<JsonValue>()
                            .await
                            .context("failed to parse external service healthcheck response")?,
                    )
                };
                validate_required_service_assets(&service, payload.as_ref())?;
            }
            ExternalServiceHealthcheckKind::Tcp => {
                if !tcp_healthcheck_ready(&healthcheck.endpoint) {
                    anyhow::bail!(unavailable_service_message(&service, &healthcheck.endpoint));
                }
                validate_required_service_assets(&service, None)?;
            }
        }
    }

    Ok(())
}

fn build_execution_override(
    request: &ConsumerRunRequest,
    target_label: &str,
) -> Option<RunExecutionOverride> {
    let mut args = request
        .export_request
        .as_ref()
        .map(|export| export.prefix_args.clone())
        .unwrap_or_default();
    args.extend(request.args.clone());

    if args.is_empty() {
        return None;
    }

    Some(RunExecutionOverride {
        target_label: target_label.trim().to_string(),
        args,
    })
}

pub(crate) async fn run_build_phase<P>(
    request: &ConsumerRunRequest,
    progress: &P,
    mut state: RunPipelineState,
) -> Result<RunPipelineState>
where
    P: ConsumerRunProgress,
{
    progress.start(HourglassPhase::Build);

    let workspace_root = state.prepared.workspace_root.clone();
    let prepared = bm::prepare_decision(
        &state.decision.plan,
        &state.launch_ctx,
        request.build_policy,
        &workspace_root,
    );
    state.build_observation = prepared.observation.clone();
    state.build_decision_kind = Some(prepared.decision.result_kind);
    let build_output_lock = if matches!(
        &prepared.decision.action,
        bm::DecisionAction::Project(_) | bm::DecisionAction::Execute | bm::DecisionAction::Fail
    ) {
        prepared
            .observation
            .as_ref()
            .map(crate::application::phase_materializer::acquire_build_output_lock_for_observation)
            .transpose()?
    } else {
        None
    };

    match prepared.decision.action {
        bm::DecisionAction::Skip => {
            maybe_apply_dependency_materialization(request, &mut state).await?;
            progress.ok(
                HourglassPhase::Build,
                "build materialization reused — executor skipped",
            );
            return Ok(state);
        }
        bm::DecisionAction::Project(layer) => {
            let Some(observation) = prepared.observation.as_ref() else {
                anyhow::bail!("build output projection requires a build observation");
            };
            match crate::application::phase_materializer::project_build_outputs(
                &workspace_root,
                observation,
                &layer,
            ) {
                Ok(()) => {
                    drop(build_output_lock);
                    maybe_apply_dependency_materialization(request, &mut state).await?;
                    progress.ok(
                        HourglassPhase::Build,
                        "build output layer projected — executor skipped",
                    );
                    return Ok(state);
                }
                Err(error) => {
                    let no_build = matches!(
                        request.build_policy,
                        crate::application::build_materialization::BuildPolicy::NoBuild
                    );
                    if no_build {
                        eprintln!(
                            "ATO-WARN failed to project required local build output layer; \
                             trying remote materialization: {error:#}"
                        );
                    } else {
                        eprintln!(
                            "ATO-WARN failed to project build output layer; trying remote \
                             materialization before local build: {}",
                            error
                        );
                    }
                    match try_remote_build_output_projection(
                        &state.decision.plan,
                        &workspace_root,
                        observation,
                        request.reporter.is_json(),
                    ) {
                        Ok(true) => {
                            drop(build_output_lock);
                            maybe_apply_dependency_materialization(request, &mut state).await?;
                            state.build_decision_kind = Some(bm::BuildResultKind::Materialized);
                            progress.ok(
                                HourglassPhase::Build,
                                "remote build output layer projected — executor skipped",
                            );
                            return Ok(state);
                        }
                        Ok(false) => {}
                        Err(remote_error) => {
                            if no_build {
                                eprintln!(
                                    "ATO-ERROR failed to use remote build output \
                                     materialization: {remote_error:#}"
                                );
                                return Err(remote_error
                                    .context("failed to use remote build output materialization"));
                            } else {
                                eprintln!(
                                    "ATO-WARN remote build output materialization unavailable; \
                                     build will execute: {remote_error:#}"
                                );
                            }
                        }
                    }
                    if no_build {
                        eprintln!(
                            "ATO-ERROR failed to project required build output layer: {error:#}"
                        );
                        return Err(error.context("failed to project required build output layer"));
                    }
                }
            }
        }
        bm::DecisionAction::Fail => {
            let Some(observation) = prepared.observation.as_ref() else {
                return Err(bm::no_build_error(&prepared.decision));
            };
            match try_remote_build_output_projection(
                &state.decision.plan,
                &workspace_root,
                observation,
                request.reporter.is_json(),
            ) {
                Ok(true) => {
                    drop(build_output_lock);
                    maybe_apply_dependency_materialization(request, &mut state).await?;
                    state.build_decision_kind = Some(bm::BuildResultKind::Materialized);
                    progress.ok(
                        HourglassPhase::Build,
                        "remote build output layer projected — executor skipped",
                    );
                    return Ok(state);
                }
                Ok(false) => {}
                Err(error) => {
                    eprintln!(
                        "ATO-ERROR failed to use remote build output materialization: {error:#}"
                    );
                    return Err(error.context("failed to use remote build output materialization"));
                }
            }
            return Err(bm::no_build_error(&prepared.decision));
        }
        bm::DecisionAction::Execute => {
            if let Some(observation) = prepared.observation.as_ref() {
                match try_remote_build_output_projection(
                    &state.decision.plan,
                    &workspace_root,
                    observation,
                    request.reporter.is_json(),
                ) {
                    Ok(true) => {
                        drop(build_output_lock);
                        maybe_apply_dependency_materialization(request, &mut state).await?;
                        state.build_decision_kind = Some(bm::BuildResultKind::Materialized);
                        progress.ok(
                            HourglassPhase::Build,
                            "remote build output layer projected — executor skipped",
                        );
                        return Ok(state);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        eprintln!(
                            "ATO-WARN remote build output materialization unavailable; \
                             build will execute: {error:#}"
                        );
                    }
                }
            }
        }
    }

    if let Err(error) = crate::commands::run::run_v03_lifecycle_steps(
        &state.decision.plan,
        &request.reporter,
        &state.launch_ctx,
    )
    .await
    {
        let Some((rerouted_decision, rerouted_launch_ctx, rerouted_prepared)) =
            maybe_run_agent_setup(
                request,
                &state.decision,
                &state.launch_ctx,
                &state.prepared,
                state.preview_mode,
                state.use_progressive_ui,
                &mut state.agent_attempted,
                "run_v03_lifecycle_steps",
                crate::application::agent::AgentFailureClassifier::classify(
                    &error,
                    "run_v03_lifecycle_steps",
                ),
                false,
            )
            .await?
        else {
            return Err(error);
        };
        state.decision = rerouted_decision;
        state.launch_ctx = rerouted_launch_ctx;
        state.prepared = rerouted_prepared;
        // Refresh observation against the rerouted plan so the persisted
        // record matches the executor that actually ran.
        state.build_observation = bm::observe_for_plan(&state.decision.plan, &state.launch_ctx)
            .ok()
            .flatten();
        crate::commands::run::run_v03_lifecycle_steps(
            &state.decision.plan,
            &request.reporter,
            &state.launch_ctx,
        )
        .await?;
    }

    if let Some(observation) = state.build_observation.as_ref() {
        // Capture while the lock is still held so that workspace-output
        // reading and state-record writing are inside the same lock region
        // as the build executor.
        let output_layer = build_output_lock.as_ref().and_then(|lock| {
            match crate::application::phase_materializer::capture_build_outputs_locked(
                lock,
                &workspace_root,
                observation,
            ) {
                Ok(layer) => layer,
                Err(err) => {
                    eprintln!(
                        "ATO-WARN failed to capture build output layer for local \
                         materialization: {err}"
                    );
                    None
                }
            }
        });
        bm::persist_after_execute(
            &state.decision.plan,
            &workspace_root,
            observation,
            request.reporter.is_json(),
            output_layer,
        );
        drop(build_output_lock);
    }

    maybe_apply_dependency_materialization(request, &mut state).await?;

    state.build_decision_kind = Some(bm::BuildResultKind::Executed);

    progress.ok(HourglassPhase::Build, "build and lifecycle hooks completed");

    Ok(state)
}

async fn maybe_apply_dependency_materialization(
    request: &ConsumerRunRequest,
    state: &mut RunPipelineState,
) -> Result<()> {
    if let Some(materialization) = crate::application::dependency_materializer::materialize_for_run(
        &state.decision.plan,
        &state.launch_ctx,
    )? {
        if request.verbose {
            request
                .reporter
                .notify(format!(
                    "📦 Dependency materialization: {} -> {}",
                    materialization.derivation_hash, materialization.output_hash
                ))
                .await?;
        }
        state.launch_ctx = state
            .launch_ctx
            .clone()
            .with_injected_mounts(vec![materialization.mount]);
    }
    Ok(())
}

fn try_remote_build_output_projection(
    plan: &capsule::router::ManifestData,
    workspace_root: &std::path::Path,
    observation: &bm::BuildObservation,
    suppress_recommendation: bool,
) -> Result<bool> {
    let Some(layer) =
        crate::application::phase_materializer_remote::lookup_remote_build_output_layer(
            workspace_root,
            observation,
        )?
    else {
        return Ok(false);
    };
    crate::application::phase_materializer::project_build_outputs(
        workspace_root,
        observation,
        &layer,
    )
    .context("failed to project imported remote build output layer")?;
    bm::persist_after_remote_project(
        plan,
        workspace_root,
        observation,
        suppress_recommendation,
        layer,
    );
    Ok(true)
}

pub(crate) async fn run_verify_phase<P>(
    request: &ConsumerRunRequest,
    progress: &P,
    mut state: RunPipelineState,
) -> Result<RunPipelineState>
where
    P: ConsumerRunProgress,
{
    progress.start(HourglassPhase::Verify);

    if state.decision.plan.is_orchestration_mode() {
        if request.background {
            anyhow::bail!("--background is not supported for orchestration mode");
        }
        progress.skip(
            HourglassPhase::Verify,
            "orchestration mode resolves execution during execute",
        );
        return Ok(state);
    }

    if matches!(state.decision.kind, capsule::router::RuntimeKind::Oci) {
        if request.background {
            anyhow::bail!("--background is not supported for runtime=oci");
        }
        progress.skip(
            HourglassPhase::Verify,
            "runtime=oci defers runtime checks to execute",
        );
        return Ok(state);
    }

    let prepared = match target_runner::prepare_target_execution(
        &state.decision.plan,
        &state.prepared,
        state.launch_ctx.clone(),
        &build_target_launch_options(request, state.preview_mode),
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            let Some((rerouted_decision, rerouted_launch_ctx, rerouted_prepared)) =
                maybe_run_agent_setup(
                    request,
                    &state.decision,
                    &state.launch_ctx,
                    &state.prepared,
                    state.preview_mode,
                    state.use_progressive_ui,
                    &mut state.agent_attempted,
                    "prepare_target_execution",
                    crate::application::agent::AgentFailureClassifier::classify(
                        &error,
                        "prepare_target_execution",
                    ),
                    false,
                )
                .await?
            else {
                return Err(error);
            };
            state.decision = rerouted_decision;
            state.launch_ctx = rerouted_launch_ctx;
            state.prepared = rerouted_prepared;
            target_runner::prepare_target_execution(
                &state.decision.plan,
                &state.prepared,
                state.launch_ctx.clone(),
                &build_target_launch_options(request, state.preview_mode),
            )?
        }
    };

    state.derived_execution = Some(PreparedDerivedExecution {
        execution_plan: prepared.execution_plan,
        tier: prepared.tier,
        guard_result: prepared.guard_result,
    });
    state.decision = prepared.runtime_decision;
    state.launch_ctx = prepared.launch_ctx;

    if state.use_progressive_ui
        && let Some(preview_session) = state.preview_session.as_ref()
    {
        crate::progressive_ui::render_preview_plan(preview_session)?;
        crate::progressive_ui::render_promotion_summary(
            &preview_session.derived_plan.promotion_eligibility,
        )?;
    }

    progress.ok(HourglassPhase::Verify, "execution plan resolved");

    Ok(state)
}

pub(crate) async fn run_dry_run_phase<P>(
    request: &ConsumerRunRequest,
    progress: &P,
    mut state: RunPipelineState,
) -> Result<RunPipelineState>
where
    P: ConsumerRunProgress,
{
    progress.start(HourglassPhase::DryRun);

    if state.decision.plan.is_orchestration_mode() {
        progress.skip(
            HourglassPhase::DryRun,
            "orchestration mode does not require run preflight",
        );
        return Ok(state);
    }

    if matches!(state.decision.kind, capsule::router::RuntimeKind::Oci) {
        target_runner::preflight_required_environment_variables(
            &state.decision.plan,
            &state.launch_ctx,
        )?;
        progress.ok(
            HourglassPhase::DryRun,
            "runtime=oci environment preflight completed",
        );
        return Ok(state);
    }

    let guard_result = state
        .derived_execution
        .as_ref()
        .map(|derived| &derived.guard_result)
        .context("run pipeline verify phase did not resolve an execution guard result")?;
    let compatibility_host_mode = resolve_compatibility_host_mode(
        guard_result.executor_kind,
        request.compatibility_fallback.as_deref(),
    )?;
    let host_fallback_requested = matches!(compatibility_host_mode, CompatibilityHostMode::Enabled);
    // native-inference is host-native by design: the engine binary runs as a host
    // process and the executor dispatch ALWAYS takes the host launcher, never the
    // source nacelle sandbox (see the `ExecutorKind::Native` arm's host_execution).
    // It is also `ExecutorKind::Native`, so without this guard a normal `ato run`
    // of a native-inference capsule would run the nacelle sandbox preflight and
    // fail E304 on a host with no sandbox backend — even though it never uses one
    // — forcing users onto `--dangerously-skip-permissions`. Skip the sandbox
    // preflight for native-inference, mirroring the dispatch.
    let is_native_inference =
        state.decision.plan.execution_runtime().as_deref() == Some("native-inference");
    if should_run_native_sandbox_preflight(
        guard_result.executor_kind,
        request.dangerously_skip_permissions,
        host_fallback_requested,
        is_native_inference,
    ) {
        state.native_nacelle = Some(crate::commands::run::preflight_native_sandbox(
            request.nacelle.clone(),
            &state.decision.plan,
            &state.prepared,
            state.launch_ctx.effective_cwd().map(PathBuf::as_path),
            &request.reporter,
        )?);
    }
    state.compatibility_host_mode = Some(compatibility_host_mode);

    progress.ok(HourglassPhase::DryRun, "runtime preflight completed");

    Ok(state)
}

/// Whether the Tier2 nacelle **sandbox** preflight should run for this launch.
///
/// It applies only to sandboxed `ExecutorKind::Native` (source/python) runs.
/// Host-execution launches never use the nacelle sandbox, so they must skip it
/// (otherwise a host with no sandbox backend fails E304 even though none is
/// needed): `--dangerously-skip-permissions`, the compatibility host fallback,
/// and — crucially — `native-inference` (which is `ExecutorKind::Native` but is
/// host-native by design and always takes the host launcher). See #748.
fn should_run_native_sandbox_preflight(
    executor_kind: ExecutorKind,
    dangerously_skip_permissions: bool,
    host_fallback_requested: bool,
    is_native_inference: bool,
) -> bool {
    matches!(executor_kind, ExecutorKind::Native)
        && !dangerously_skip_permissions
        && !host_fallback_requested
        && !is_native_inference
}

#[allow(clippy::too_many_arguments)]
#[async_trait(?Send)]
pub(crate) trait ConsumerRunExecuteHooks {
    fn preflight_native_sandbox(
        &self,
        nacelle_override: Option<PathBuf>,
        plan: &capsule::router::ManifestData,
        prepared: &PreparedRunContext,
        effective_cwd: Option<&Path>,
        reporter: &Arc<CliReporter>,
    ) -> Result<PathBuf>;

    async fn complete_background_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        plan: &capsule::router::ManifestData,
        runtime: String,
        scoped_id: Option<String>,
        is_one_shot: bool,
        ready_without_events: bool,
        desktop_open_only: bool,
        compatibility_host_mode: CompatibilityHostMode,
        execution_id: Option<String>,
        reporter: &Arc<CliReporter>,
    ) -> Result<()>;

    async fn complete_foreground_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        reporter: Arc<CliReporter>,
        is_one_shot: bool,
        sandbox_initialized: bool,
        ipc_socket_mapped: bool,
        desktop_open_only: bool,
        use_progressive_ui: bool,
        execution_id: Option<String>,
    ) -> Result<i32>;

    async fn cleanup_existing_scoped_processes_before_run(
        &self,
        scoped_id: &str,
        reporter: &Arc<CliReporter>,
    ) -> Result<()>;

    async fn notify_web_endpoint(
        &self,
        plan: &capsule::router::ManifestData,
        reporter: &Arc<CliReporter>,
    ) -> Result<()>;

    fn process_runtime_label(
        &self,
        plan: &capsule::router::ManifestData,
        dangerous_skip_permissions: bool,
        compatibility_host_mode: CompatibilityHostMode,
    ) -> String;
}

fn cleanup_process_artifacts(paths: &[PathBuf]) {
    for path in paths {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn maybe_report_failed_provider_workspace(request: &ConsumerRunRequest, workspace_root: &Path) {
    if !request.keep_failed_artifacts {
        return;
    }

    let resolution_metadata = workspace_root.join("resolution.json");
    if resolution_metadata.exists() {
        crate::install::provider_target::maybe_report_kept_failed_provider_workspace(
            workspace_root,
            request.reporter.is_json(),
        );
    }
}

/// Resolve `{{deps.<alias>.runtime_exports.<key>}}` templates using the
/// dependency orchestrator's resolved exports. If the value is not a
/// template, it is returned unchanged.
fn resolve_dep_template_inner(value: &str, graph: &RunningGraph) -> String {
    if !value.contains("deps.") || !value.contains("runtime_exports.") {
        return value.to_string();
    }
    let re = regex::Regex::new(r"\{\{deps\.(\w+)\.runtime_exports\.(\w+)\}\}").unwrap();
    let mut result = value.to_string();
    for cap in re.captures_iter(value) {
        let alias = &cap[1];
        let export_key = &cap[2];
        if let Some(exports) = graph.runtime_exports(alias)
            && let Some(resolved) = exports.get(export_key)
        {
            let pattern = format!("{{{{deps.{}.runtime_exports.{}}}}}", alias, export_key);
            result = result.replace(&pattern, resolved);
        }
    }
    result
}

pub(crate) async fn run_execute_phase<P, H>(
    request: &ConsumerRunRequest,
    progress: &P,
    state: RunPipelineState,
    attempt: Option<&mut PipelineAttemptContext>,
    hooks: &H,
) -> Result<()>
where
    P: ConsumerRunProgress,
    H: ConsumerRunExecuteHooks,
{
    progress.start(HourglassPhase::Execute);

    let mut attempt = attempt;

    let RunPipelineState {
        preview_session: _,
        preview_mode,
        use_progressive_ui,
        prepared,
        decision,
        launch_ctx,
        mut external_capsules,
        mut dep_contracts,
        agent_attempted: _,
        derived_execution,
        compatibility_host_mode,
        native_nacelle,
        build_observation,
        build_decision_kind: _,
        receipt_graph_id_sink,
    } = state;

    if decision.plan.is_orchestration_mode() {
        if request.background {
            anyhow::bail!("--background is not supported for orchestration mode");
        }

        // OCI service graph: route to the official Podman-backed multi-service executor.
        // This must be checked before the legacy Bollard orchestrator to avoid routing
        // OCI services through the Docker-compatible path.
        if decision.plan.all_services_are_oci() {
            let exit = crate::executors::oci_multi_service::execute_multi_service(
                &decision.plan,
                request.reporter.clone(),
                &launch_ctx,
                request.strict_realization,
                receipt_graph_id_sink.as_ref(),
            )
            .await?;
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                if let Some(dep_contracts) = dep_contracts.as_mut() {
                    dep_contracts.shutdown_now();
                }
                maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
                std::process::exit(exit);
            }
            progress.ok(
                HourglassPhase::Execute,
                "oci multi-service runtime completed",
            );
            return Ok(());
        }

        let exit = crate::executors::orchestrator::execute(
            &decision.plan,
            &prepared,
            request.reporter.clone(),
            &launch_ctx,
            crate::executors::orchestrator::OrchestratorOptions {
                enforcement: request.enforcement.clone(),
                sandbox_mode: request.sandbox_mode,
                dangerously_skip_permissions: request.dangerously_skip_permissions,
                assume_yes: request.assume_yes,
                nacelle: request.nacelle.clone(),
                // Foreground `ato run` keeps the historical fixed-host-port
                // publish for `services.main` so external tools (CLI users,
                // shells, browser bookmarks) reach the recipe on the declared
                // port. Sessions that own the only consumer (e.g. Desktop's
                // WebView) opt into EphemeralMainService instead.
                publish_policy: crate::executors::orchestrator::PublishPolicy::ExternalDefault,
            },
            attempt.as_deref_mut(),
        )
        .await?;
        if exit != 0 {
            if let Some(external_capsules) = external_capsules.as_mut() {
                external_capsules.shutdown_now();
            }
            if let Some(dep_contracts) = dep_contracts.as_mut() {
                dep_contracts.shutdown_now();
            }
            maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
            std::process::exit(exit);
        }

        progress.ok(HourglassPhase::Execute, "orchestration runtime completed");
        return Ok(());
    }

    if matches!(decision.kind, capsule::router::RuntimeKind::Oci) {
        if request.background {
            anyhow::bail!("--background is not supported for runtime=oci");
        }

        target_runner::preflight_required_environment_variables(&decision.plan, &launch_ctx)?;
        let exit = crate::executors::oci_single_target::execute_single_target(
            &decision.plan,
            request.reporter.clone(),
            &launch_ctx,
            request.strict_realization,
            receipt_graph_id_sink.as_ref(),
        )
        .await?;
        if exit != 0 {
            if let Some(external_capsules) = external_capsules.as_mut() {
                external_capsules.shutdown_now();
            }
            if let Some(dep_contracts) = dep_contracts.as_mut() {
                dep_contracts.shutdown_now();
            }
            maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
            std::process::exit(exit);
        }

        progress.ok(HourglassPhase::Execute, "oci runtime completed");
        return Ok(());
    }

    let derived_execution = derived_execution
        .context("run pipeline execute phase requires lock-derived execution artifacts")?;
    let execution_plan = derived_execution.execution_plan;
    let guard_result = derived_execution.guard_result;
    let compatibility_host_mode = compatibility_host_mode
        .context("run pipeline execute phase requires compatibility host mode")?;

    debug!(
        runtime = execution_plan.target.runtime.as_str(),
        driver = execution_plan.target.driver.as_str(),
        ?derived_execution.tier,
        executor = ?guard_result.executor_kind,
        requires_sandbox_opt_in = guard_result.requires_sandbox_opt_in,
        dangerously_skip_permissions = request.dangerously_skip_permissions,
        "ExecutionPlan resolved"
    );

    let sidecar = match crate::common::sidecar::maybe_start_sidecar() {
        Ok(Some(sidecar)) => {
            debug!("Sidecar started");
            Some(sidecar)
        }
        Ok(None) => {
            debug!("Sidecar not available (no TSNET env)");
            None
        }
        Err(err) => {
            debug!(error = %err, "Sidecar start failed");
            None
        }
    };

    let mut sidecar_cleanup = crate::SidecarCleanup::new(sidecar, request.reporter.clone());
    if let Some(attempt) = attempt.as_mut() {
        let mut scope = (*attempt).cleanup_scope();
        sidecar_cleanup.register_attempt_cleanup(&mut scope);
    }
    // #747: capture the background engine's stdout/stderr to a log file (under the
    // ato run dir, next to the pid file) instead of discarding it to /dev/null.
    // execute_host returns this as the session's log_path, so `ato ps`/`ato logs`
    // and the "process exited before readiness" error can show WHY a process that
    // exits before readiness failed — rather than an opaque E999. `apply_logged_stdio`
    // creates the dir and the redirect survives the parent's exit (detach-safe).
    let mode = if request.background {
        let run_dir = capsule::common::paths::ato_path_or_workspace_tmp("run");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        ExecuteMode::Logged(run_dir.join(format!("engine-{}-{}.log", std::process::id(), stamp)))
    } else {
        ExecuteMode::Foreground
    };

    let run_scoped_id = runtime_overrides::scoped_id_override();

    // Auto-assign a unique port when none was specified via manifest or
    // override. The override is installed through a restore-on-drop guard
    // bound to this function's scope: it stays visible to the synchronous
    // `runtime_overrides::override_port` reads in the executor path (and is
    // inherited by the workload child at spawn time), then is restored when
    // this run returns so it cannot leak into a subsequent run in the same
    // process. See `scoped_override_port` for the env-safety rationale.
    let _auto_port_guard: Option<runtime_overrides::PortOverrideGuard> =
        if runtime_overrides::override_port(decision.plan.execution_port()).is_none() {
            let identity = build_port_identity(
                &decision.plan.manifest_path,
                decision.plan.selected_target_label(),
                run_scoped_id.as_deref(),
            );
            crate::runtime::port_manager::PortManager::new()
                .ok()
                .and_then(|mgr| mgr.resolve_port(&identity).ok())
                .map(runtime_overrides::scoped_override_port)
        } else {
            None
        };

    if request.background
        && let Some(scoped_id) = run_scoped_id.as_deref()
    {
        hooks
            .cleanup_existing_scoped_processes_before_run(scoped_id, &request.reporter)
            .await?;
    }

    if execution_plan.target.runtime == capsule::execution_plan::model::ExecutionRuntime::Web {
        hooks
            .notify_web_endpoint(&decision.plan, &request.reporter)
            .await?;
    }

    let receipt_output =
        crate::application::execution_receipt_builder::build_prelaunch_receipt_document_with_graph(
            &decision.plan,
            &execution_plan,
            &launch_ctx,
            build_observation.as_ref(),
        )?;
    // PR-3b: publish declared/resolved ids to the boundary's
    // `ReceiptGraphIdSink` IMMEDIATELY after the bundle is built — so
    // if the rest of this function (writing the receipt, opening the
    // workload, waiting for readiness) fails, the partial-receipt
    // boundary wrapper still picks up the ids the would-be success
    // receipt would have carried.
    if let (Some(sink), Some(bundle)) = (
        receipt_graph_id_sink.as_ref(),
        receipt_output.launch_graph.as_ref(),
    ) {
        sink.set(crate::application::receipt_boundary::GraphIds {
            declared_execution_id: Some(bundle.derived.execution_ids.declared_execution_id.clone()),
            resolved_execution_id: Some(bundle.derived.execution_ids.resolved_execution_id.clone()),
        });
    }
    let execution_receipt_document = receipt_output.document;
    let execution_receipt_path =
        crate::application::execution_receipts::write_receipt_document_atomic(
            &execution_receipt_document,
        )?;
    let (execution_id, schema_label) = match &execution_receipt_document {
        capsule::execution_identity::ExecutionReceiptDocument::V1(receipt) => {
            (receipt.execution_id.clone(), "v1")
        }
        capsule::execution_identity::ExecutionReceiptDocument::V2(receipt) => {
            (receipt.execution_id.clone(), "v2-experimental")
        }
    };
    request
        .reporter
        .notify(format!(
            "Execution receipt ({}): {} ({})",
            schema_label,
            execution_id,
            execution_receipt_path.display()
        ))
        .await?;
    // Stable machine-readable line for non-TTY callers (CI, scripts, MCP).
    request
        .reporter
        .notify(format!("RECEIPT: {}", execution_receipt_path.display()))
        .await?;

    // #500 — strict fail-closed realization gate. Opt-in via
    // `--strict-realization`. This runs at the prelaunch boundary: the resolved
    // launch graph is built and the prelaunch receipt is persisted, but no guest
    // process, runtime process, or container has been created yet. In the
    // default profile this is a no-op; in strict mode it blocks the launch with
    // a typed `AtoExecutionError` (recoverable downstream via downcast) when a
    // required input cannot be verified. The host/provider realization-evidence
    // producer is #501, so until it lands strict mode is conservatively
    // fail-closed — see `application::strict_realization`.
    if request.strict_realization {
        // Fail-closed: strict mode must refuse to launch anything it cannot even
        // inspect. A missing resolved launch graph is a block, not a skip.
        let Some(launch_graph) = receipt_output.launch_graph.as_ref() else {
            return Err(
                crate::application::strict_realization::missing_launch_graph_error().into(),
            );
        };
        let env = crate::application::strict_realization::launch_environment();
        // Unbox before handing to anyhow: downstream recovery downcasts to
        // `AtoExecutionError` (utils/error.rs), which a boxed wrap would hide.
        crate::application::strict_realization::enforce_strict_realization(
            launch_graph,
            &env,
            crate::application::strict_realization::launch_profile(true),
        )
        .map_err(|e| anyhow::Error::new(*e))?;
    }

    // ── Ready-State restore sub-mode (additive; developer-preview) ───────────
    // Legacy fall-through (None) happens ONLY for: flag off, or flag on + capsule
    // NOT Ready-State-eligible. With the flag ON + an eligible capsule, a MISSING
    // sealed artifact FAILS CLOSED (not a silent cold run) — the user explicitly
    // enabled Ready-State as a validation mode. An explicit-but-unavailable
    // backend also fails closed (`select_backend`).
    if crate::application::ready_state::flags::ready_state_enabled() {
        use crate::application::ready_state;
        use capsule::Measurable;
        // Only reached when the flag is on, so the legacy path does ZERO of this
        // (no manifest re-parse / hash). decide_ready_state_run fails CLOSED when
        // the capsule is eligible but no sealed artifact exists (validation mode
        // must not silently degrade to a cold run); returns None only for a
        // non-eligible capsule (→ legacy dispatch below).
        //
        // Eligibility + the artifact key MUST be derived from the SAME manifest the
        // `ato build` seal used — the RAW `capsule.toml` in the source dir.
        // `decision.plan.manifest` is the derived/normalized ExecutionPlan manifest:
        // it drops the top-level `[snapshot]` section (→ never Ready-State-eligible)
        // and canonicalizes differently (→ a different `capsule_manifest_hash` than
        // the seal). And `decision.plan.manifest_path` is the resolved `ato.lock.json`,
        // not the capsule manifest. Either made `ato run` silently cold-path past
        // every sealed artifact. Read the raw `capsule.toml` from the source dir
        // (`manifest_dir`) — the same bytes the build sealed; fall back to the plan
        // manifest only if the file can't be read/parsed.
        let rs_capsule_toml = decision.plan.manifest_dir.join("capsule.toml");
        let rs_raw: toml::Value = std::fs::read_to_string(&rs_capsule_toml)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_else(|| decision.plan.manifest.clone());
        let rs_manifest = capsule::types::CapsuleManifest::from_toml(&toml::to_string(&rs_raw)?)?;
        let rs_hash = ready_state::capsule_manifest_hash(&rs_raw)?;
        let rs_root = ready_state::state_root();
        if let Some(plan) = ready_state::decide_ready_state_run(&rs_manifest, &rs_hash, &rs_root)? {
            // Phase 8a-RunGate (#912): the binding-preview decision (names only, never
            // values). D2 routes a binding-required capsule through the post-restore
            // bound-ready gate ONLY under ATO_READY_STATE_BINDINGS_PREVIEW=1; otherwise
            // the #837 pre-restore guard fail-closes exactly as today.
            let binding_req = ready_state::bindings::requires_runtime_bindings(&rs_manifest);
            let binding_names: Vec<String> = binding_req
                .secrets
                .iter()
                .chain(binding_req.bindings.iter())
                .chain(binding_req.external.iter())
                .cloned()
                .collect();
            let binding_preview = ready_state::flags::bindings_preview_enabled();
            let binding_gate_active = binding_preview && !binding_names.is_empty();
            // v1.6 (ato#983) Slice 3 revision: captured now — `plan.manifest`
            // is moved into `restore_and_expose` below — so MountVolumes can
            // be sent before any binding delivery.
            let has_durable_state = plan
                .manifest
                .supervisor_build
                .as_ref()
                .is_some_and(|s| !s.state_volumes.is_empty());
            // v1.2 PR 2: the grant-scoped resolver (default: host-local SecretStore,
            // namespace rs-<hash16>) + LAUNCH PREFLIGHT. Every declared binding must
            // resolve BEFORE anything is restored — a missing grant blocks the launch
            // with the aggregated, actionable report (name + description + the exact
            // `ato secrets set … --namespace …` command). Values stay in memory only
            // until lease delivery; never logged or recorded.
            let mut preflight_resolved: Vec<(String, protocol::binding_lease::SecretValue)> =
                Vec::new();
            let mut resolver_kind: Option<String> = None;
            let mut grant_namespace: Option<String> = None;
            if binding_gate_active {
                // The namespace derivation is itself fail-closed: a malformed
                // manifest hash must never degrade into a weaker grant scope.
                let ns = ready_state::binding_grants::binding_namespace(&rs_hash)?;
                let resolver = ready_state::secret_resolver::select_resolver(&ns)?;
                resolver_kind = Some(resolver.kind().to_string());
                preflight_resolved = ready_state::binding_grants::preflight_resolve(
                    resolver.as_ref(),
                    &binding_names,
                    &rs_manifest,
                    &ns,
                )?;
                grant_namespace = Some(ns);
            }
            {
                let mut receipt = ready_state::binding_host::BindingPreviewReceipt::decide(
                    binding_preview,
                    binding_names.clone(),
                );
                receipt.resolver_kind = resolver_kind.clone();
                receipt.grant_namespace = grant_namespace.clone();
                receipt.record(&capsule::common::paths::ato_path_or_workspace_tmp("run"));
            }
            if !binding_gate_active {
                // flag off, OR no bindings required → unchanged: the #837 guard
                // fail-closes any binding-required capsule before restore.
                ready_state::bindings::ensure_no_unwired_runtime_bindings(
                    &rs_manifest,
                    ready_state::bindings::BindingGuardMode::VerifyOnly,
                )?;
            }
            let backend = ready_state::backend::select_backend()?;
            // L2 (#912): placement capability gate. A binding-required preview must
            // fail closed BEFORE restore if this backend/host cannot deliver bindings
            // (no vsock / not firecracker / not x86_64) — never silently fall back.
            if binding_gate_active {
                let caps = backend.probe();
                if !caps.binding.supports_binding_lease {
                    let reason = caps
                        .binding
                        .unavailable_reason()
                        .unwrap_or_else(|| "binding-lease unsupported".into());
                    tracing::warn!(target: "ato::ready_state", %reason, "binding preview fail-closed (placement)");
                    anyhow::bail!(
                        "Ready-State binding preview requested but this host cannot deliver bindings: {reason}"
                    );
                }
            }
            // Orphan Ready-State overlays from crashed prior serving runs are
            // reaped/quarantined by the canonical startup sweep
            // (`RuntimeProcessRegistry::sweep_run_dir_orphans` → Class 4); no
            // separate sweep is wired here.
            let store =
                ready_state::store::open_store(&plan.state_root, &plan.capsule_manifest_hash)?;
            // U10 (#877): opt-in mem_backend selection diagnostics — record what a
            // selector WOULD choose, then restore via File EXACTLY as before. Pure
            // observation; no behavior change.
            if ready_state::flags::uffd_diagnostics_enabled() {
                let caps = backend.probe();
                let no_bindings = !ready_state::bindings::requires_runtime_bindings(&rs_manifest)
                    .requires_bindings();
                let run_dir = capsule::common::paths::ato_path_or_workspace_tmp("run");
                let msg = ready_state::diagnostics::record(
                    backend.id(),
                    &caps,
                    &plan.manifest,
                    &store,
                    no_bindings,
                    &plan.capsule_manifest_hash,
                    ready_state::flags::ready_state_enabled(),
                    &run_dir,
                );
                let _ = request.reporter.notify(msg).await;
            }
            let overlay = capsule::common::paths::ato_path_or_workspace_tmp("run")
                .join(format!("ready-state-{}", std::process::id()));
            // U15 (#882): opt-in AUTO-selection preview — the pure selector chooses
            // File vs UFFD from the real facts (no-binding only, local CAS, remote
            // off), gracefully falling back to File on an unsupported host. Takes
            // precedence over the U11 forced preview.
            let uffd_preview = if ready_state::flags::uffd_auto_preview_enabled() {
                let caps = backend.probe();
                let no_bindings = !ready_state::bindings::requires_runtime_bindings(&rs_manifest)
                    .requires_bindings();
                let has_mem = plan
                    .manifest
                    .layers
                    .memory
                    .as_ref()
                    .and_then(|m| m.chunks.first())
                    .map(|c| store.has_chunk(&c.hash))
                    .unwrap_or(false);
                let inputs = snapshot::mem_backend_selector::MemBackendInputs {
                    host_supports_uffd: caps.supports_uffd_mem_backend,
                    runner_class_compatible: true,
                    capsule_no_bindings: no_bindings,
                    local_cas_has_memory: has_mem,
                    // The engine auto-loads a persisted hotset profile (U12) when
                    // present; the selector only decides File vs UFFD here.
                    hotset_profile_available: false,
                    remote_preview_enabled: false,
                    remote_available: false,
                    validation_mode: ready_state::flags::ready_state_enabled(),
                    fallback_allowed: true,
                };
                let decision = snapshot::mem_backend_selector::decide_mem_backend(&inputs);
                use snapshot::mem_backend_selector::MemBackendChoice::*;
                let engage = matches!(decision.choice, UffdLocal | UffdHotset | UffdRemote);
                tracing::info!(
                    target: "ato::ready_state",
                    choice = ?decision.choice,
                    engage,
                    reasons = ?decision.reasons,
                    "READY-STATE auto-select mem_backend (preview)"
                );
                let _ = request
                    .reporter
                    .notify(format!(
                        "Ready-State auto-select: {:?} — {}",
                        decision.choice,
                        decision.reasons.last().map(String::as_str).unwrap_or("")
                    ))
                    .await;
                engage
            } else if ready_state::flags::uffd_preview_enabled() {
                let caps = backend.probe();
                if !caps.supports_uffd_mem_backend {
                    anyhow::bail!(
                        "UFFD preview (ATO_READY_STATE_UFFD_PREVIEW) requested but this host does \
                         not support the UFFD mem_backend: {}. Unset it to use the File path.",
                        caps.uffd_reason.as_deref().unwrap_or("unsupported")
                    );
                }
                let has_mem = plan
                    .manifest
                    .layers
                    .memory
                    .as_ref()
                    .and_then(|m| m.chunks.first())
                    .map(|c| store.has_chunk(&c.hash))
                    .unwrap_or(false);
                if !has_mem {
                    anyhow::bail!(
                        "UFFD preview requires the memory image in the local CAS; it is not present."
                    );
                }
                tracing::info!(
                    target: "ato::ready_state",
                    "READY-STATE: UFFD local preview engaged (no-binding capsule, local CAS demand)"
                );
                true
            } else {
                false
            };
            let receipt = ready_state::restore::restore_and_expose(
                backend.as_ref(),
                &store,
                plan.manifest,
                overlay,
                plan.host_runner_class,
                uffd_preview,
            )?;
            let session = receipt.session;
            // v1.6 (ato#983) Slice 3 revision: MOUNT VOLUMES BEFORE BIND —
            // durable state is a restore-time binding too (never baked into
            // the build-time snapshot — see `mount_volumes_before_expose`'s
            // doc comment), independent of the secret binding-preview flag
            // below (a state-only capsule may have no secrets to preview at
            // all). Same fail-closed shape: any failure tears the restored
            // VM down and never exposes traffic.
            if has_durable_state {
                let uds = session.vsock_uds.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "durable state declared but the restored session has no vsock channel \
                         (the artifact was not built with vsock) — cannot mount"
                    )
                });
                let mount = uds.and_then(|uds| {
                    ready_state::binding_host::mount_volumes_before_expose(
                        &uds,
                        true,
                        std::time::Duration::from_secs(10),
                    )
                });
                if let Err(e) = mount {
                    let _ = backend.stop(session.clone());
                    anyhow::bail!(
                        "Ready-State durable-state mount failed closed (no traffic exposed): {e}"
                    );
                }
            }
            // Phase 8a-RunGate PR D2 (#912): BIND BEFORE EXPOSE. For a binding-required
            // capsule under the preview flag, connect the guest-agent over vsock,
            // deliver the leases, and block until bound-ready — BEFORE any traffic is
            // exposed. Any failure (no vsock channel / missing secret / connect timeout /
            // agent Error / not bound-ready) FAILS CLOSED: tear the restored VM down and
            // never expose. The secret is delivered only over vsock, never recorded.
            if binding_gate_active {
                let bind = (|| -> anyhow::Result<()> {
                    let uds = session.vsock_uds.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "binding preview: the restored session has no vsock channel \
                             (the artifact was not built with vsock) — cannot deliver bindings"
                        )
                    })?;
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    // Values were preflight-resolved BEFORE restore; the lease clock
                    // (issued/expires) starts here at delivery. TTL is policy-driven
                    // (ATO_READY_STATE_BINDING_TTL_MS, default 1h) — the foreground
                    // renewal loop renews inside it.
                    let leases = ready_state::binding_host::issue_leases(
                        std::mem::take(&mut preflight_resolved),
                        now_ms,
                        ready_state::flags::binding_ttl_ms(),
                    )?;
                    ready_state::binding_host::bind_before_expose(
                        uds,
                        &leases,
                        std::time::Duration::from_secs(10),
                    )
                })();
                if let Err(e) = bind {
                    // Fail closed: no unbound session is ever exposed.
                    let _ = backend.stop(session.clone());
                    anyhow::bail!(
                        "Ready-State binding preview failed closed (no traffic exposed): {e}"
                    );
                }
                tracing::info!(
                    target: "ato::ready_state",
                    bindings = ?binding_names,
                    "READY-STATE binding preview: bound-ready — exposing traffic"
                );
            }
            // Surface RuntimeMetadata::MicroVm through the restored-session handle.
            let handle = ready_state::runtime_adapter::RestoredRuntimeHandle::new(session.clone());
            let metrics = handle.capture_metrics().await.map_err(anyhow::Error::new)?;
            // Long-lived serving (Phase 7) requires a REAL serving VMM process.
            // Only a backend that spawns one (Firecracker) sets `vmm_pid = Some`;
            // a backend with no serving process (Fake / KVM-free) returns `None`.
            // We must NOT register a `runtime="microvm"` ProcessInfo against the
            // CLI's own pid or report "serving" when no serving process exists —
            // that would claim a long-lived session that isn't real. Such backends
            // stay on the verify-only path (restore → teardown → Ok), preserving
            // the KVM-free developer-preview smoke without faking a serving VM.
            let Some(serving_pid) = session.vmm_pid else {
                tracing::info!(
                    target: "ato::ready_state",
                    backend = backend.id(),
                    metadata = ?metrics.metadata,
                    "READY-STATE: verified restore (backend '{}' has no serving process — not long-lived)",
                    backend.id()
                );
                let _ = ready_state::restore::teardown(backend.as_ref(), session);
                return Ok(());
            };
            // The restored Firecracker child is already detached (reparents to
            // init), so it KEEPS SERVING after this returns. Register it like a
            // background process so `ato ps` lists it and a LATER fresh-process
            // `ato stop` reaps it from the on-disk record (pid/tap/overlay), NOT
            // the in-memory backend registry. NO teardown here. (Binding-required
            // capsules already failed closed above; this session is no-binding.)
            let id = format!("capsule-{serving_pid}");
            let now = SystemTime::now();
            let info = crate::runtime::process::ProcessInfo {
                id: id.clone(),
                name: decision
                    .plan
                    .manifest_path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "capsule".to_string()),
                pid: serving_pid,
                workload_pid: None,
                status: crate::runtime::process::ProcessStatus::Ready,
                runtime: "microvm".to_string(),
                start_time: now,
                os_start_time_unix_ms: capsule::state::session::process::process_start_time_unix_ms(
                    serving_pid as u32,
                ),
                workload_os_start_time_unix_ms: None,
                manifest_path: Some(decision.plan.manifest_path.clone()),
                scoped_id: None,
                target_label: Some(decision.plan.selected_target_label().to_string()),
                requested_port: session.guest_port,
                log_path: Some(session.overlay_root.join("console.log")),
                ready_at: Some(now),
                last_event: Some("restored".to_string()),
                last_error: None,
                exit_code: None,
                ready_state_backend_id: Some(backend.id().to_string()),
                ready_state_overlay_root: Some(session.overlay_root.clone()),
                ready_state_session_id: Some(session.session_id.clone()),
                ready_state_tap_dev: None,
                // D3: record the vsock UDS for a bound session so `ato stop` can
                // scrub the guest bindings before teardown.
                ready_state_vsock_uds: if binding_gate_active {
                    session.vsock_uds.clone()
                } else {
                    None
                },
            };
            crate::runtime::process::ProcessManager::new()?.write_pid(&info)?;
            tracing::info!(
                target: "ato::ready_state",
                backend = backend.id(),
                metadata = ?metrics.metadata,
                session = %session.session_id,
                pid = serving_pid,
                port = ?session.guest_port,
                "READY-STATE: serving (long-lived)"
            );
            let port_str = session
                .guest_port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".to_string());

            if ready_state::flags::foreground_serve_enabled() {
                // Foreground serve (Phase 7.5b, opt-in): BLOCK until the guest exits
                // or Ctrl-C, tearing the microVM down either way. The SIGINT hook
                // reuses the dep-contract teardown registry; it captures only Send
                // data (cloned session + id) and reconstructs a fresh Firecracker
                // backend + ProcessManager inside (cross-process via .fc-session.json),
                // so it owns no non-Send state. teardown + delete_pid are idempotent,
                // so double-SIGINT and a SIGINT racing the normal-exit path are safe.
                use snapshot::SnapshotBackend as _;
                request
                    .reporter
                    .notify(format!(
                        "🚀 Ready-State microVM serving (ID: {id}) on port {port_str} — Ctrl-C to stop"
                    ))
                    .await?;
                let sigint_session = session.clone();
                let sigint_id = id.clone();
                let token =
                    crate::application::pipeline::cleanup::register_dep_contract_sigint_teardown(
                        move || {
                            let _ = snapshot::FirecrackerBackend::new().stop(sigint_session);
                            if let Ok(pm) = crate::runtime::process::ProcessManager::new() {
                                let _ = pm.delete_pid(&sigint_id);
                            }
                        },
                    );
                // v1.2 PR 2 (L8): while a BOUND session serves in the foreground, renew
                // its leases inside the TTL and revoke any lease whose grant disappears
                // (`ato secrets delete …` mid-session ⇒ guest value scrubbed, traffic
                // gates). Background serving has no host process to renew from — there
                // the single TTL stands and expiry-scrubs lazily (documented).
                let renewal = (binding_gate_active && session.vsock_uds.is_some()).then(|| {
                    ready_state::binding_host::spawn_lease_renewal(
                        session.vsock_uds.clone().expect("checked above"),
                        grant_namespace
                            .clone()
                            .expect("set when the binding gate is active"),
                        binding_names.clone(),
                        ready_state::flags::binding_ttl_ms(),
                        // Local `ato run` resolves from the user's own store — no
                        // ato-api AI grant on this path (P3b is runner-lease only).
                        None,
                    )
                });
                let exit = crate::executors::source::wait_for_pid_exit(serving_pid as u32).await;
                if let Some(task) = renewal {
                    task.abort();
                }
                // Normal exit (guest shut down on its own): drop the now-stale SIGINT
                // hook so it can't double-fire, then reap tap/overlay/lock + pid record.
                crate::application::pipeline::cleanup::unregister_dep_contract_sigint_teardown(
                    token,
                );
                let _ = ready_state::restore::teardown(backend.as_ref(), session);
                let _ = crate::runtime::process::ProcessManager::new().map(|pm| pm.delete_pid(&id));
                let code = exit.unwrap_or(0);
                tracing::info!(target: "ato::ready_state", id = %id, code, "READY-STATE: foreground serve ended");
                progress.ok(HourglassPhase::Execute, "ready-state microVM exited");
                return Ok(());
            }

            // Default (#845): background register-and-return; `ato stop` reaps later.
            request
                .reporter
                .notify(format!(
                    "🚀 Ready-State microVM serving (ID: {id}) on port {port_str} — stop with `ato stop {id}`"
                ))
                .await?;
            progress.ok(HourglassPhase::Execute, "ready-state microVM serving");
            return Ok(());
        }
    }

    let run_command_uses_specialized_executor = decision
        .plan
        .execution_driver()
        .map(|driver| {
            matches!(
                driver.trim().to_ascii_lowercase().as_str(),
                "deno" | "node" | "python" | "wasmtime"
            )
        })
        .unwrap_or(false);

    if decision.plan.execution_run_command().is_some()
        && !run_command_uses_specialized_executor
        && !matches!(guard_result.executor_kind, ExecutorKind::Native)
    {
        let mut process = crate::executors::shell::execute(&decision.plan, mode, &launch_ctx)?;
        register_capsule_process_cleanup(
            &mut attempt,
            &process,
            decision.plan.selected_target_label(),
        );
        if request.background {
            let pid = process.child.id();
            let id = format!("capsule-{}", pid);
            let now = SystemTime::now();

            let info = crate::runtime::process::ProcessInfo {
                id: id.clone(),
                name: decision
                    .plan
                    .manifest_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                pid: pid as i32,
                workload_pid: None,
                status: crate::runtime::process::ProcessStatus::Ready,
                runtime: "shell".to_string(),
                start_time: now,
                os_start_time_unix_ms: capsule::state::session::process::process_start_time_unix_ms(
                    pid,
                ),
                workload_os_start_time_unix_ms: None,
                manifest_path: Some(decision.plan.manifest_path.clone()),
                scoped_id: run_scoped_id.clone(),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                requested_port: runtime_overrides::override_port(decision.plan.execution_port()),
                log_path: None,
                ready_at: Some(now),
                last_event: Some("spawned".to_string()),
                last_error: None,
                exit_code: None,
                ready_state_backend_id: None,
                ready_state_overlay_root: None,
                ready_state_session_id: None,
                ready_state_tap_dev: None,
                ready_state_vsock_uds: None,
            };

            let process_manager = crate::runtime::process::ProcessManager::new()?;
            process_manager.write_pid(&info)?;
            persist_background_dependency_contracts(&id, pid as i32, dep_contracts.as_ref())?;
            detach_dependency_contracts_for_background(&mut dep_contracts);
            request
                .reporter
                .notify(format!("🚀 Capsule started in background (ID: {})", id))
                .await?;
            drop(process.child);
            sidecar_cleanup.stop_now();
            progress.ok(
                HourglassPhase::Execute,
                "background shell execution started",
            );
            return Ok(());
        }

        let exit_code = crate::executors::source::wait_for_exit(&mut process.child).await?;
        cleanup_process_artifacts(&process.cleanup_paths);
        sidecar_cleanup.stop_now();
        if exit_code != 0 {
            if let Some(external_capsules) = external_capsules.as_mut() {
                external_capsules.shutdown_now();
            }
            if let Some(dep_contracts) = dep_contracts.as_mut() {
                dep_contracts.shutdown_now();
            }
            maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
            std::process::exit(exit_code);
        }

        progress.ok(HourglassPhase::Execute, "shell execution completed");
        return Ok(());
    }

    let host_fallback_requested = matches!(compatibility_host_mode, CompatibilityHostMode::Enabled);
    let desktop_native_open_only = request.desktop_open_path.is_some();
    let is_one_shot = is_one_shot_run_request(request, &prepared);
    if use_progressive_ui && !desktop_native_open_only {
        if host_fallback_requested {
            crate::progressive_ui::render_host_fallback_warning()?;
        } else {
            crate::progressive_ui::render_security_context(
                guard_result.executor_kind,
                host_fallback_requested,
                request.dangerously_skip_permissions,
                runtime_overrides::override_port(decision.plan.execution_port()),
                launch_ctx.effective_cwd().map(PathBuf::as_path),
                launch_ctx.injected_mounts().len(),
                launch_ctx
                    .injected_mounts()
                    .iter()
                    .filter(|mount| !mount.readonly)
                    .count(),
            )?;
            render_execution_roots_note(&decision.plan, &launch_ctx)?;
        }
    }

    // PR-4b: the consent gate stays on plan-direct `has_consent`
    // because of the zero-permission short-circuit. The bundle-derived
    // `ExecutionConsentView` is still built (for symmetry with
    // preflight, and so the bundle becomes the canonical consent
    // identity surface for future PRs). Debug parity guard pins the
    // two surfaces agree outside the zero-permission case.
    let consent_already_granted = if request.dangerously_skip_permissions {
        true
    } else {
        let plan_granted = crate::consent_store::has_consent(&execution_plan)?;
        debug_assert!(
            {
                let consent_deps = capsule::lockfile::manifest_external_capsule_dependencies(
                    &decision.plan.manifest,
                )
                .ok();
                let view_granted = consent_deps.map(|deps| {
                    let consent_input = capsule::engine::execution_graph::GraphConsentInput {
                        scoped_id: execution_plan.consent.key.scoped_id.clone(),
                        version: execution_plan.consent.key.version.clone(),
                        target_label: execution_plan.consent.key.target_label.clone(),
                        policy_segment_hash: execution_plan.consent.policy_segment_hash.clone(),
                        provisioning_policy_hash: execution_plan
                            .consent
                            .provisioning_policy_hash
                            .clone(),
                    };
                    let bundle =
                        crate::application::graph_views::build_declared_only_bundle_with_consent(
                            &deps,
                            Some(decision.plan.manifest_path.display().to_string()),
                            None,
                            Vec::new(),
                            consent_input,
                        );
                    let view =
                        crate::application::graph_views::ExecutionConsentView::from_bundle(&bundle);
                    crate::consent_store::has_consent_view(&view).unwrap_or(plan_granted)
                });
                view_granted
                    .map(|view_granted| plan_granted || plan_granted == view_granted)
                    .unwrap_or(true)
            },
            "PR-4b parity: has_consent_view disagrees with plan-direct has_consent \
             at run.rs pre-launch gate (outside the zero-permission short-circuit)"
        );
        plan_granted
    };
    if !consent_already_granted {
        if use_progressive_ui {
            crate::progressive_ui::render_execution_consent_summary(
                &crate::consent_store::consent_summary(&execution_plan),
            )?;
            let prompt = if host_fallback_requested {
                "Proceed with this Execution Plan and Host Fallback mode?"
            } else {
                "Proceed with this Execution Plan?"
            };
            if !crate::progressive_ui::confirm_action(prompt, false)? {
                crate::progressive_ui::show_cancel("Execution cancelled.")?;
                return Err(AtoExecutionError::from_ato_error(
                    capsule::AtoError::ExecutionContractInvalid {
                        message: "ExecutionPlan consent rejected by user".to_string(),
                        hint: Some(
                            "Execution Plan の要約を確認し、許可する場合のみ再実行してください。"
                                .to_string(),
                        ),
                        field: Some("execution_plan.consent".to_string()),
                        service: None,
                    },
                )
                .into());
            }
            crate::consent_store::record_consent(&execution_plan)?;
        } else if request.assume_yes && prepared.workspace_root.join("resolution.json").exists() {
            crate::consent_store::record_consent(&execution_plan)?;
        } else {
            crate::consent_store::require_consent(&execution_plan, request.assume_yes)?;
        }
    } else if host_fallback_requested {
        // Host fallback opt-in is independent of dangerously-skip-permissions.
        if use_progressive_ui {
            if request.assume_yes {
                crate::progressive_ui::show_warning(
                    "Proceeding with Host Fallback mode (--yes specified)",
                )?;
            } else if !crate::progressive_ui::confirm_action(
                "Proceed with Host Fallback mode?",
                false,
            )? {
                crate::progressive_ui::show_cancel("Execution cancelled.")?;
                return Ok(());
            }
        } else if !request.assume_yes {
            anyhow::bail!(
                "Host Fallback mode requires interactive confirmation. Re-run with --yes in non-interactive environments."
            );
        }
    } else if use_progressive_ui
        && preview_mode
        && !request.assume_yes
        && !crate::progressive_ui::confirm_action(
            "Proceed with Preview Run? (Ephemeral Sandbox)",
            true,
        )?
    {
        crate::progressive_ui::show_cancel("Preview cancelled.")?;
        return Ok(());
    }

    // Run prestart_command after provider readiness and consent, before
    // the main process launch (e.g., Prisma database migrations).
    if let Some(command) = decision
        .plan
        .prestart_command_string()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        let prestart_cwd = crate::adapters::runtime::provisioning::dependency_root(&decision.plan);
        tracing::info!(%command, cwd=%prestart_cwd.display(), "running prestart command");
        let target_env: Vec<(String, String)> = decision.plan.execution_env().into_iter().collect();
        // Resolve {{deps.X.runtime_exports.Y}} templates from the running dep graph.
        let resolved_env: Vec<(String, String)> = {
            let graph = dep_contracts
                .as_ref()
                .and_then(DependencyContractGuard::graph);
            target_env
                .into_iter()
                .map(|(key, value)| {
                    let resolved = if let Some(g) = graph {
                        resolve_dep_template_inner(&value, g)
                    } else {
                        value.to_string()
                    };
                    (key, resolved)
                })
                .collect()
        };
        tracing::info!(%command, cwd=%prestart_cwd.display(), env_count=%resolved_env.len(), has_graph=%dep_contracts.as_ref().and_then(DependencyContractGuard::graph).is_some(), "running prestart command");
        // Debug: log DATABASE_URL value
        if let Some((_, db_url)) = resolved_env.iter().find(|(k, _)| k == "DATABASE_URL") {
            tracing::info!(%db_url, "prestart DATABASE_URL resolved");
        }
        // The prestart hook runs as a POSIX shell script on the host. On a
        // platform without `/bin/sh` (Windows lacking Git Bash/MSYS2) the
        // spawn below would fail with an opaque "os error 2" → generic E999.
        // Gate it with a typed, actionable error instead (issue #377). No-op
        // on Linux/macOS where a shell is always present.
        crate::application::shell_preflight::ensure_host_posix_shell(&command)?;
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&command)
            .current_dir(&prestart_cwd)
            .stdin(std::process::Stdio::null());
        for (key, value) in &resolved_env {
            cmd.env(key, value);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            // Defense in depth: if the shell probe passed but the spawn still
            // can't find `sh` (PATH race, broken symlink), translate the
            // file-not-found failure into the same typed error rather than a
            // bare spawn context that maps to E999.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(
                    crate::application::shell_preflight::source_build_shell_unavailable_error(
                        &command,
                        std::env::consts::OS,
                    ),
                );
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context("failed to spawn prestart command"));
            }
        };
        let status = child.wait().context("prestart command wait failed")?;
        if !status.success() {
            anyhow::bail!(
                "prestart command exited with status {}: {}",
                status,
                command
            );
        }
    }

    match guard_result.executor_kind {
        ExecutorKind::Native => {
            // native-inference is host-native by design (the engine binary is a
            // host process, like OCI runs host containers) — always take the host
            // launcher path, never the source nacelle sandbox.
            let is_native_inference =
                matches!(decision.kind, capsule::router::RuntimeKind::NativeInference);
            let host_execution = request.dangerously_skip_permissions
                || host_fallback_requested
                || desktop_native_open_only
                || is_native_inference;
            // Ensure managed native-inference assets (the llama.cpp engine and/or
            // a downloaded model) are present in their caches BEFORE the host
            // launcher resolves their deterministic paths. Local `engine_path` /
            // `model` skip the corresponding fetch.
            if is_native_inference {
                ensure_native_inference_engine(&decision.plan).await?;
                ensure_native_inference_model(&decision.plan).await?;
            }
            let process = if host_execution {
                if let Some(app_path) = request.desktop_open_path.as_ref() {
                    crate::executors::source::execute_open_path(app_path, mode)?
                } else {
                    crate::executors::source::execute_host(
                        &decision.plan,
                        prepared.authoritative_lock.as_ref(),
                        request.reporter.clone(),
                        mode,
                        &launch_ctx,
                    )?
                }
            } else {
                let nacelle = match native_nacelle {
                    Some(path) => path,
                    None => hooks.preflight_native_sandbox(
                        request.nacelle.clone(),
                        &decision.plan,
                        &prepared,
                        launch_ctx.effective_cwd().map(PathBuf::as_path),
                        &request.reporter,
                    )?,
                };
                crate::executors::source::execute(
                    &decision.plan,
                    prepared.authoritative_lock.as_ref(),
                    prepared.effective_state.as_ref(),
                    Some(nacelle),
                    request.reporter.clone(),
                    &request.enforcement,
                    mode,
                    &launch_ctx,
                )?
            };
            register_capsule_process_cleanup(
                &mut attempt,
                &process,
                decision.plan.selected_target_label(),
            );

            if request.background {
                let process_id = format!("capsule-{}", process.child.id());
                let consumer_pid = process.child.id() as i32;
                // Label the recorded runtime from the actual execution mode, not
                // just the permission flags: native-inference (and host-fallback)
                // run as bare host processes, so they must record runtime="host".
                // Recording "nacelle" for a non-nacelle host process makes
                // process_info_is_alive() fail the nacelle-cmdline identity check,
                // so `ato ps`/`ato stop` treat the live session as dead. `host_execution`
                // already folds in is_native_inference / dangerously_skip / host_fallback.
                let runtime = hooks.process_runtime_label(
                    &decision.plan,
                    host_execution,
                    compatibility_host_mode,
                );
                let ready_without_events = host_execution && process.event_rx.is_none();
                hooks
                    .complete_background_source_process(
                        process,
                        &decision.plan,
                        runtime,
                        run_scoped_id.clone(),
                        is_one_shot,
                        ready_without_events,
                        desktop_native_open_only,
                        compatibility_host_mode,
                        // Host execution uses spawn_host_lifecycle_events; re-stamp
                        // its receipt from the observed outcome. The nacelle-sandbox
                        // path (!host_execution) keeps its launch-passed gate.
                        host_execution.then(|| execution_id.clone()),
                        &request.reporter,
                    )
                    .await?;
                persist_background_dependency_contracts(
                    &process_id,
                    consumer_pid,
                    dep_contracts.as_ref(),
                )?;
                detach_dependency_contracts_for_background(&mut dep_contracts);
                sidecar_cleanup.stop_now();
                progress.ok(
                    HourglassPhase::Execute,
                    if desktop_native_open_only {
                        "background desktop app launch requested"
                    } else {
                        "background native execution started"
                    },
                );
                return Ok(());
            }

            let exit_code = hooks
                .complete_foreground_source_process(
                    process,
                    request.reporter.clone(),
                    is_one_shot,
                    !host_execution,
                    launch_ctx
                        .socket_paths()
                        .map(|paths| !paths.is_empty())
                        .unwrap_or(false),
                    desktop_native_open_only,
                    use_progressive_ui,
                    host_execution.then(|| execution_id.clone()),
                )
                .await?;
            sidecar_cleanup.stop_now();

            if exit_code != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                if let Some(dep_contracts) = dep_contracts.as_mut() {
                    dep_contracts.shutdown_now();
                }
                maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
                std::process::exit(exit_code);
            }
        }
        ExecutorKind::NodeCompat if host_fallback_requested => {
            let process = crate::executors::source::execute_host(
                &decision.plan,
                prepared.authoritative_lock.as_ref(),
                request.reporter.clone(),
                mode,
                &launch_ctx,
            )?;
            register_capsule_process_cleanup(
                &mut attempt,
                &process,
                decision.plan.selected_target_label(),
            );

            if request.background {
                let process_id = format!("capsule-{}", process.child.id());
                let consumer_pid = process.child.id() as i32;
                let runtime =
                    hooks.process_runtime_label(&decision.plan, false, compatibility_host_mode);
                let ready_without_events = process.event_rx.is_none();
                hooks
                    .complete_background_source_process(
                        process,
                        &decision.plan,
                        runtime,
                        run_scoped_id.clone(),
                        is_one_shot,
                        ready_without_events,
                        false,
                        compatibility_host_mode,
                        // NodeCompat uses spawn_host_lifecycle_events; re-stamp
                        // its receipt from the observed readiness outcome.
                        Some(execution_id.clone()),
                        &request.reporter,
                    )
                    .await?;
                persist_background_dependency_contracts(
                    &process_id,
                    consumer_pid,
                    dep_contracts.as_ref(),
                )?;
                detach_dependency_contracts_for_background(&mut dep_contracts);
                sidecar_cleanup.stop_now();
                progress.ok(
                    HourglassPhase::Execute,
                    "background host fallback execution started",
                );
                return Ok(());
            }

            let exit_code = hooks
                .complete_foreground_source_process(
                    process,
                    request.reporter.clone(),
                    is_one_shot,
                    false,
                    launch_ctx
                        .socket_paths()
                        .map(|paths| !paths.is_empty())
                        .unwrap_or(false),
                    false,
                    use_progressive_ui,
                    Some(execution_id.clone()),
                )
                .await?;
            sidecar_cleanup.stop_now();

            if exit_code != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                if let Some(dep_contracts) = dep_contracts.as_mut() {
                    dep_contracts.shutdown_now();
                }
                maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
                std::process::exit(exit_code);
            }
        }
        ExecutorKind::Wasm => {
            let exit = crate::executors::wasm::execute(
                &decision.plan,
                request.reporter.clone(),
                &launch_ctx,
            )?;
            sidecar_cleanup.stop_now();
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                if let Some(dep_contracts) = dep_contracts.as_mut() {
                    dep_contracts.shutdown_now();
                }
                std::process::exit(exit);
            }
        }
        ExecutorKind::WebStatic => {
            if request.background {
                let child = crate::executors::open_web::spawn_background(&decision.plan)?;
                let pid = child.id();
                let id = format!("capsule-{}", pid);
                let now = SystemTime::now();

                let info = crate::runtime::process::ProcessInfo {
                    id: id.clone(),
                    name: decision
                        .plan
                        .manifest_path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    pid: pid as i32,
                    workload_pid: None,
                    status: crate::runtime::process::ProcessStatus::Ready,
                    runtime: "web-static".to_string(),
                    start_time: now,
                    os_start_time_unix_ms:
                        capsule::state::session::process::process_start_time_unix_ms(pid),
                    workload_os_start_time_unix_ms: None,
                    manifest_path: Some(decision.plan.manifest_path.clone()),
                    scoped_id: run_scoped_id.clone(),
                    target_label: Some(decision.plan.selected_target_label().to_string()),
                    requested_port: runtime_overrides::override_port(
                        decision.plan.execution_port(),
                    ),
                    log_path: None,
                    ready_at: Some(now),
                    last_event: Some("spawned".to_string()),
                    last_error: None,
                    exit_code: None,
                    ready_state_backend_id: None,
                    ready_state_overlay_root: None,
                    ready_state_session_id: None,
                    ready_state_tap_dev: None,
                    ready_state_vsock_uds: None,
                };

                let process_manager = crate::runtime::process::ProcessManager::new()?;
                process_manager.write_pid(&info)?;
                persist_background_dependency_contracts(&id, pid as i32, dep_contracts.as_ref())?;
                detach_dependency_contracts_for_background(&mut dep_contracts);

                request
                    .reporter
                    .notify(format!("🚀 Capsule started in background (ID: {})", id))
                    .await?;

                drop(child);
                sidecar_cleanup.stop_now();
                progress.ok(HourglassPhase::Execute, "background web runtime started");
                return Ok(());
            }

            crate::executors::open_web::execute(&decision.plan, request.reporter.clone())?;
            sidecar_cleanup.stop_now();
        }
        ExecutorKind::Deno => {
            let exit = crate::executors::deno::execute(
                &decision.plan,
                prepared.authoritative_lock.as_ref(),
                &execution_plan,
                &launch_ctx,
                request.dangerously_skip_permissions,
                attempt,
            )?;
            sidecar_cleanup.stop_now();
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                if let Some(dep_contracts) = dep_contracts.as_mut() {
                    dep_contracts.shutdown_now();
                }
                maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
                std::process::exit(exit);
            }
        }
        ExecutorKind::NodeCompat => {
            if request.background {
                let process = crate::executors::node_compat::spawn_background(
                    &decision.plan,
                    prepared.authoritative_lock.as_ref(),
                    &execution_plan,
                    &launch_ctx,
                    request.dangerously_skip_permissions,
                )?;
                register_capsule_process_cleanup(
                    &mut attempt,
                    &process,
                    decision.plan.selected_target_label(),
                );
                let process_id = format!("capsule-{}", process.child.id());
                let consumer_pid = process.child.id() as i32;
                let runtime =
                    hooks.process_runtime_label(&decision.plan, false, compatibility_host_mode);
                let ready_without_events = process.event_rx.is_none();
                hooks
                    .complete_background_source_process(
                        process,
                        &decision.plan,
                        runtime,
                        run_scoped_id.clone(),
                        is_one_shot,
                        ready_without_events,
                        false,
                        compatibility_host_mode,
                        // NodeCompat uses spawn_host_lifecycle_events; re-stamp
                        // its receipt from the observed readiness outcome.
                        Some(execution_id.clone()),
                        &request.reporter,
                    )
                    .await?;
                persist_background_dependency_contracts(
                    &process_id,
                    consumer_pid,
                    dep_contracts.as_ref(),
                )?;
                detach_dependency_contracts_for_background(&mut dep_contracts);
                sidecar_cleanup.stop_now();
                progress.ok(
                    HourglassPhase::Execute,
                    "background node compat execution started",
                );
                return Ok(());
            }
            // Foreground NodeCompat (Connected Runner dispatch / `ato run …
            // --sandbox -y`) must emit honest readiness exactly like the host
            // source executor: spawn with the lifecycle pump wired so the
            // declared port is TCP-probed, the canonical `LIFECYCLE: ready
            // port=N` line is printed, and the V2 receipt readiness gate is
            // re-stamped from the observed event. The blocking `execute()` path
            // did none of this, so dispatched node capsules never went ready
            // and timed out at the 600s runner ready deadline (#623).
            let process = crate::executors::node_compat::spawn_foreground(
                &decision.plan,
                prepared.authoritative_lock.as_ref(),
                &execution_plan,
                &launch_ctx,
                request.dangerously_skip_permissions,
            )?;
            register_capsule_process_cleanup(
                &mut attempt,
                &process,
                decision.plan.selected_target_label(),
            );
            let exit = hooks
                .complete_foreground_source_process(
                    process,
                    request.reporter.clone(),
                    is_one_shot,
                    false,
                    launch_ctx
                        .socket_paths()
                        .map(|paths| !paths.is_empty())
                        .unwrap_or(false),
                    false,
                    use_progressive_ui,
                    // NodeCompat uses spawn_host_lifecycle_events; re-stamp its
                    // receipt from the observed readiness outcome.
                    Some(execution_id.clone()),
                )
                .await?;
            sidecar_cleanup.stop_now();
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                if let Some(dep_contracts) = dep_contracts.as_mut() {
                    dep_contracts.shutdown_now();
                }
                maybe_report_failed_provider_workspace(request, &prepared.workspace_root);
                std::process::exit(exit);
            }
        }
    }

    progress.ok(
        HourglassPhase::Execute,
        if request.desktop_open_path.is_some() {
            "desktop app launch requested"
        } else {
            "capsule execution completed"
        },
    );

    Ok(())
}

/// Fetch a managed native-inference engine binary into the toolchain cache
/// before the host launcher resolves it. A local `engine_path` overrides this
/// (nothing to fetch). Dispatches through the [`Engine`] trait
/// (`capsule::routing::native_inference`): the engine resolves which build to
/// fetch (variant/platform dispatch first, failing closed for an unsupported or
/// not-ready accelerated variant) and performs the fetch.
///
/// [`Engine`]: capsule::routing::native_inference::Engine
async fn ensure_native_inference_engine(plan: &capsule::router::ManifestData) -> Result<()> {
    use capsule::routing::native_inference::{self, HostCapabilities};

    let Some(engine) = native_inference::resolve_engine(plan) else {
        // No recognized managed engine declared: the launcher's
        // resolve_server_command produces the precise error.
        return Ok(());
    };

    // Ensure-step: build a PROBED host snapshot (Vulkan readiness is detected on
    // Linux) so the variant's platform/readiness fail-closed gate runs for real,
    // rather than falling back to a CPU build. A failed probe is "not ready".
    let host = HostCapabilities::from_profile(
        capsule::foundation::host_gpu::detect_host_gpu_profile().ok(),
    );
    let ctx = native_inference::engine_context(plan, engine.as_ref());

    let fetcher = capsule::packers::runtime_fetcher::RuntimeFetcher::new()?;
    engine.ensure_engine(&ctx, &host, &fetcher).await?;
    Ok(())
}

/// Download + verify a managed native-inference model into the
/// content-addressed cache before the host launcher resolves its deterministic
/// blob path. A local `model` overrides this. Routed through the [`Engine`]
/// trait's `ensure_model` (today's `model_url` + `model_sha256` CAS path for
/// llama.cpp).
///
/// [`Engine`]: capsule::routing::native_inference::Engine
async fn ensure_native_inference_model(plan: &capsule::router::ManifestData) -> Result<()> {
    use capsule::routing::native_inference;

    let Some(engine) = native_inference::resolve_engine(plan) else {
        // No recognized managed engine declared: the launcher's
        // resolve_model_path produces the precise error.
        return Ok(());
    };

    // The model path/fetch is engine-driven and variant-independent.
    let ctx = native_inference::engine_context(plan, engine.as_ref());

    engine.ensure_model(&ctx).await?;
    Ok(())
}

pub(crate) async fn reroute_auto_provisioned_execution(
    decision: capsule::router::RuntimeDecision,
    launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    preview_mode: bool,
    shadow_manifest_path: &Path,
) -> Result<(
    capsule::router::RuntimeDecision,
    crate::executors::launch_context::RuntimeLaunchContext,
    PreparedRunContext,
)> {
    let validation_mode = run_validation_mode(preview_mode);
    let loaded_manifest = capsule::manifest::load_manifest_with_validation_mode(
        shadow_manifest_path,
        validation_mode,
    )?;
    let rerouted_decision =
        capsule::router::route_manifest_with_state_overrides_and_validation_mode(
            shadow_manifest_path,
            router::ExecutionProfile::Dev,
            Some(decision.plan.selected_target_label()),
            decision.plan.state_source_overrides.clone(),
            validation_mode,
        )?;
    let engine_override_declared = loaded_manifest.raw.get("engine").is_some();
    let rerouted_prepared = prepared.with_bridge_manifest(
        toml::from_str(&loaded_manifest.raw_text).unwrap_or_else(|_| loaded_manifest.raw.clone()),
        validation_mode,
        engine_override_declared,
    );
    let rerouted_launch_ctx = target_runner::resolve_launch_context(
        &rerouted_decision.plan,
        &rerouted_prepared,
        &reporter,
    )
    .await?
    .with_effective_cwd(
        launch_ctx
            .effective_cwd()
            .cloned()
            .unwrap_or_else(|| prepared.workspace_root.clone()),
    )
    .with_injected_env(launch_ctx.merged_env())
    .with_injected_mounts(launch_ctx.injected_mounts().to_vec());
    Ok((rerouted_decision, rerouted_launch_ctx, rerouted_prepared))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn maybe_run_agent_setup(
    request: &ConsumerRunRequest,
    decision: &capsule::router::RuntimeDecision,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
    prepared: &PreparedRunContext,
    preview_mode: bool,
    use_progressive_ui: bool,
    agent_attempted: &mut bool,
    trigger: &str,
    failure: Option<crate::application::agent::ClassifiedFailure>,
    force_reroute: bool,
) -> Result<
    Option<(
        capsule::router::RuntimeDecision,
        crate::executors::launch_context::RuntimeLaunchContext,
        PreparedRunContext,
    )>,
> {
    let agent_enabled = request.agent_local_root.is_some()
        && !preview_mode
        && !matches!(request.agent_mode, RunAgentMode::Off);
    if !agent_enabled || *agent_attempted {
        return Ok(None);
    }
    if !force_reroute && failure.is_none() {
        return Ok(None);
    }
    if !force_reroute && !matches!(request.agent_mode, RunAgentMode::Auto) {
        return Ok(None);
    }
    if force_reroute && !matches!(request.agent_mode, RunAgentMode::Force) {
        return Ok(None);
    }
    if !force_reroute
        && failure.as_ref().is_some_and(|failure| {
            matches!(
                failure.kind,
                crate::application::agent::SetupFailureKind::MissingLockfile
            )
        })
    {
        return Ok(None);
    }

    if !manifest_path_is_inside_source_root(
        &decision.plan.manifest_path,
        &decision.plan.manifest_dir,
    ) {
        debug!(
            manifest_path = %decision.plan.manifest_path.display(),
            source_root = %decision.plan.manifest_dir.display(),
            "Skipping agent setup for lock-derived source inference plan"
        );
        return Ok(None);
    }

    *agent_attempted = true;
    let agent_request = crate::application::agent::AgentRunRequest {
        project_root: request
            .agent_local_root
            .clone()
            .context("agent local root is missing")?,
        source_root: decision.plan.manifest_dir.clone(),
        manifest_path: decision.plan.manifest_path.clone(),
        plan: decision.plan.clone(),
        launch_ctx: launch_ctx.clone(),
        trigger: trigger.to_string(),
        failure,
        force_reroute,
        reporter: request.reporter.clone(),
        assume_yes: request.assume_yes,
        use_progressive_ui,
    };
    let outcome = crate::application::agent::run_agent_setup(agent_request)
        .await
        .map_err(|error| {
            anyhow::anyhow!("agent setup attempt failed during {}: {}", trigger, error)
        })?;
    if !outcome.modified && !force_reroute {
        return Ok(None);
    }

    if use_progressive_ui {
        crate::progressive_ui::show_note(
            "Agent Session",
            format!(
                "Artifacts      : {}\nShadow Manifest: {}",
                crate::progressive_ui::format_path_for_note(&outcome.artifact_dir),
                crate::progressive_ui::format_path_for_note(&outcome.shadow_manifest_path)
            ),
        )?;
    }

    let rerouted = reroute_auto_provisioned_execution(
        decision.clone(),
        launch_ctx.clone(),
        prepared,
        request.reporter.clone(),
        preview_mode,
        &outcome.shadow_manifest_path,
    )
    .await?;
    Ok(Some(rerouted))
}

fn manifest_path_is_inside_source_root(manifest_path: &Path, source_root: &Path) -> bool {
    let manifest_path = canonical_or_absolute(manifest_path);
    let source_root = canonical_or_absolute(source_root);
    manifest_path.starts_with(source_root)
}

fn canonical_or_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    absolute.canonicalize().unwrap_or(absolute)
}

pub(crate) fn resolve_state_source_overrides(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
) -> Result<HashMap<String, String>> {
    resolve_state_source_overrides_with_store(manifest, raw_bindings, None)
}

/// Resolve `--state` bindings for a run, deferring to headless auto-provisioning
/// for any remaining unbound persistent state when the run is sandboxed (#687).
///
/// In `--sandbox` mode the missing-binding hard error is suppressed because the
/// caller auto-provisions the remaining declared state immediately afterward.
/// Without `--sandbox` the original fail-closed behavior is preserved: a
/// persistent state with no `--state` binding is still an error.
fn resolve_explicit_or_auto_state_source_overrides(
    manifest: &CapsuleManifest,
    request: &ConsumerRunRequest,
) -> Result<HashMap<String, String>> {
    if !request.sandbox_mode {
        return resolve_state_source_overrides(manifest, &request.state_bindings);
    }
    let requested = parse_state_bindings(&request.state_bindings)?;
    resolve_requested_state_source_overrides_lenient(manifest, &requested, None)
}

pub(crate) fn resolve_state_source_overrides_with_store(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
    store: Option<&RegistryStore>,
) -> Result<HashMap<String, String>> {
    let requested = parse_state_bindings(raw_bindings)?;
    resolve_state_source_overrides_from_requested(manifest, &requested, store, None)
}

/// Like [`resolve_state_source_overrides_with_store`], but additionally
/// auto-binds any *unbound* persistent `[state.*]` entry under `managed_state_root`
/// (server/runner contexts where no interactive folder prompt is possible).
///
/// Explicit `--state` bindings always win. `target_label` selects the effective
/// target (so two targets of the same capsule get distinct directories);
/// `None` falls back to the manifest `default_target`. The caller MUST encode
/// the server-confirmed owner/account AND a stable, immutable capsule identity
/// into `managed_state_root` (see [`managed_state_dir`]); neither is derived
/// from capsule-controlled input here.
pub(crate) fn resolve_state_source_overrides_managed(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
    store: Option<&RegistryStore>,
    managed_state_root: Option<&Path>,
    target_label: Option<&str>,
) -> Result<HashMap<String, String>> {
    let requested = parse_state_bindings(raw_bindings)?;
    let managed = managed_state_root.map(|root| ManagedStateRoot {
        root,
        target: target_label.unwrap_or(manifest.default_target.as_str()),
    });
    resolve_state_source_overrides_from_requested(manifest, &requested, store, managed)
}

fn parse_state_bindings(raw_bindings: &[String]) -> Result<HashMap<String, String>> {
    let mut requested = HashMap::new();
    for raw in raw_bindings {
        let (state_name, locator) = raw.split_once('=').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --state binding '{}'; expected data=/absolute/path or data=state-...",
                raw
            )
        })?;
        let state_name = state_name.trim();
        let locator = locator.trim();
        if state_name.is_empty() || locator.is_empty() {
            anyhow::bail!(
                "invalid --state binding '{}'; expected data=/absolute/path or data=state-...",
                raw
            );
        }
        if requested
            .insert(state_name.to_string(), locator.to_string())
            .is_some()
        {
            anyhow::bail!(
                "state '{}' was bound more than once via --state",
                state_name
            );
        }
    }
    Ok(requested)
}

/// Resolve explicitly-requested `--state` bindings without erroring on declared
/// persistent state that has no binding. Used by the headless auto-provisioning
/// path (#687), which fills any remaining declared state immediately after.
/// Validation of the requested bindings themselves (undeclared / non-persistent)
/// is still enforced.
fn resolve_requested_state_source_overrides_lenient(
    manifest: &CapsuleManifest,
    requested: &HashMap<String, String>,
    store: Option<&RegistryStore>,
) -> Result<HashMap<String, String>> {
    for state_name in requested.keys() {
        let requirement = manifest.state.get(state_name).ok_or_else(|| {
            anyhow::anyhow!(
                "--state references undeclared manifest state '{}'",
                state_name
            )
        })?;
        if requirement.durability != StateDurability::Persistent {
            anyhow::bail!(
                "--state only supports persistent manifest state; '{}' is {:?}",
                state_name,
                requirement.durability
            );
        }
    }

    let mut resolved = HashMap::new();
    for (state_name, locator) in requested {
        let record = if parse_state_reference(locator).is_some() {
            match store {
                Some(store) => resolve_registered_state_reference_in_store(
                    manifest, state_name, locator, store,
                )?,
                None => resolve_registered_state_reference(manifest, state_name, locator)?,
            }
        } else {
            match store {
                Some(store) => {
                    ensure_registered_state_binding_in_store(manifest, state_name, locator, store)?
                }
                None => ensure_registered_state_binding(manifest, state_name, locator)?,
            }
        };
        resolved.insert(state_name.clone(), record.backend_locator);
    }

    Ok(resolved)
}

/// A managed state root plus the effective target, used to derive stable
/// per-capsule state directories for runner/server runs.
struct ManagedStateRoot<'a> {
    root: &'a Path,
    target: &'a str,
}

/// Directory for a managed persistent-state binding: `<root>/<target>/<state_key>`,
/// each appended segment made path-safe and collision-free by [`path_segment`].
///
/// **Namespace contract — `root` MUST already be scoped** by the server-confirmed
/// owner/account AND a stable, immutable capsule identity (e.g.
/// `<base>/<owner_id>/<capsule_revision>`). This resolver only appends
/// `target`/profile + `state_key`; it deliberately does NOT derive owner or
/// capsule identity here, and in particular does NOT key off `name`/`version`,
/// which are capsule-controlled and not globally unique (two different capsules
/// could share them and would otherwise collide). `lease_id` or any
/// session-local id MUST NOT appear in `root` — that would lose persistent data
/// on every re-lease.
fn managed_state_dir(root: &Path, target: &str, state_key: &str) -> PathBuf {
    root.join(path_segment(target))
        .join(path_segment(state_key))
}

/// A path-safe, collision-free single directory segment: `<sanitized>-<hash16>`.
///
/// Characters outside `[A-Za-z0-9_-]` (including `.` and `/`) are mapped to `_`
/// so `.`, `..`, and embedded separators can never escape the parent directory.
/// The blake3 suffix is taken over the RAW input, so two distinct inputs that
/// would otherwise sanitize to the same string (e.g. `a/b` vs `a_b`, `.` vs `_`)
/// still resolve to distinct segments.
///
/// Shared with the Connected Runner agent, which builds the owner/identity
/// portion of a managed state root with the same scheme so the namespace is
/// consistent end to end.
pub(crate) fn path_segment(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let hash = &blake3::hash(raw.as_bytes()).to_hex()[..16];
    if sanitized.is_empty() {
        format!("seg-{hash}")
    } else {
        format!("{sanitized}-{hash}")
    }
}

fn resolve_state_source_overrides_from_requested(
    manifest: &CapsuleManifest,
    requested: &HashMap<String, String>,
    store: Option<&RegistryStore>,
    managed: Option<ManagedStateRoot<'_>>,
) -> Result<HashMap<String, String>> {
    for state_name in requested.keys() {
        let requirement = manifest.state.get(state_name).ok_or_else(|| {
            anyhow::anyhow!(
                "--state references undeclared manifest state '{}'",
                state_name
            )
        })?;
        if requirement.durability != StateDurability::Persistent {
            anyhow::bail!(
                "--state only supports persistent manifest state; '{}' is {:?}",
                state_name,
                requirement.durability
            );
        }
    }

    let persistent_states: Vec<_> = manifest
        .state
        .iter()
        .filter(|(_, requirement)| requirement.durability == StateDurability::Persistent)
        .collect();
    if persistent_states.is_empty() {
        if requested.is_empty() {
            return Ok(HashMap::new());
        }
        anyhow::bail!(
            "--state was provided but the manifest declares no persistent [state] entries"
        );
    }

    // Auto-bind any unbound persistent state under the managed root (runner /
    // server context, where no interactive folder prompt is possible). Explicit
    // `--state` entries in `requested` always win. Each synthesized directory is
    // created up front and then flows through the SAME path-binding resolution
    // as an explicit `--state <key>=<dir>` below (no parallel logic).
    let mut effective = requested.clone();
    if let Some(managed) = managed.as_ref() {
        for (state_name, _) in &persistent_states {
            if effective.contains_key(state_name.as_str()) {
                continue;
            }
            let dir = managed_state_dir(managed.root, managed.target, state_name);
            std::fs::create_dir_all(&dir).map_err(|e| {
                anyhow::anyhow!(
                    "failed to create managed state directory {}: {e}",
                    dir.display()
                )
            })?;
            effective.insert((*state_name).clone(), dir.to_string_lossy().into_owned());
        }
    }

    let mut resolved = HashMap::new();

    for (state_name, _) in persistent_states {
        let locator = effective.get(state_name.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "persistent state '{}' requires an explicit --state {}=/absolute/path or --state {}=state-... binding",
                state_name,
                state_name,
                state_name
            )
        })?;
        let record = if parse_state_reference(locator).is_some() {
            match store {
                Some(store) => resolve_registered_state_reference_in_store(
                    manifest, state_name, locator, store,
                )?,
                None => resolve_registered_state_reference(manifest, state_name, locator)?,
            }
        } else {
            match store {
                Some(store) => {
                    ensure_registered_state_binding_in_store(manifest, state_name, locator, store)?
                }
                None => ensure_registered_state_binding(manifest, state_name, locator)?,
            }
        };

        resolved.insert(state_name.clone(), record.backend_locator);
    }

    Ok(resolved)
}

/// Stable identity inputs for headless state auto-provisioning (#700).
///
/// These are the inputs to the `headless_state_instance_id` derivation. Every
/// field is deliberately *stable across runs and revisions* of the same source:
/// the persistent state directory must be reused by re-runs of the same source
/// and shared across revisions, but it must NOT collide with a *different* source
/// that happens to declare the same `manifest.name`.
///
/// Inputs that change per execution — `execution_id`, allocated ports, session
/// id, process / container id, dynamic env — are intentionally NOT part of this
/// struct. (That is exactly why `capsule_instance_key` — derived from
/// `install_profile_key + install_revision_id + execution_id` — is the wrong key
/// here: it changes per revision/execution, whereas instance-scoped persistent
/// state is shared across revisions.)
#[derive(Debug, Clone)]
pub(crate) struct HeadlessStateKeyInputs<'a> {
    /// Canonical, stable reference to the source being run (e.g.
    /// `github.com/owner/repo`, or a stable local source path). When the source
    /// reference is unstable (a per-run materialization dir under one of the
    /// ato-managed ephemeral roots, or an anonymous local checkout), the caller
    /// passes the workspace root in `workspace_root_for_fallback` so the
    /// derivation can substitute the materialized-source tree hash instead.
    pub(crate) normalized_source_ref: &'a str,
    /// Selected target label (`default` when none was chosen). Different targets
    /// of the same source get distinct state instances.
    pub(crate) selected_target_label: &'a str,
    /// Profile id, or `None` to fall back to the `"default"` profile namespace.
    pub(crate) profile_id: Option<&'a str>,
    /// Runner / account namespace when one is available (e.g. `ATO_SCOPED_ID`).
    /// `None` for a normal `ato run` and for the runner-spawned child today —
    /// `ato runner serve` does NOT export an account/runner namespace into the
    /// `ato run <source> --sandbox -y` child env (the runner token is kept out of
    /// the child entirely), so this is normally absent; it is threaded through so
    /// a future multi-tenant runner can isolate per account without changing the
    /// path scheme.
    pub(crate) runner_namespace: Option<&'a str>,
    /// Workspace root used ONLY to compute the `source_tree_hash` fallback when
    /// `normalized_source_ref` is unstable.
    pub(crate) workspace_root_for_fallback: &'a Path,
}

/// Serializable view of the stable key inputs, fed through the repo's existing
/// versioned content-hash helper (`canonical_hash` → JCS + `blake3:<hex>`). Using
/// the shared helper inherits its documented "never hash session ids / ports /
/// pids / timestamps" contract rather than inventing a new ad-hoc hash.
#[derive(serde::Serialize)]
struct HeadlessStateKeyMaterial<'a> {
    /// Version tag so the derivation can evolve without silently re-keying old
    /// state: bump this and existing instances rebind to a fresh directory.
    v: u8,
    source_ref: &'a str,
    name: &'a str,
    target: &'a str,
    profile: &'a str,
    namespace: &'a str,
}

/// Outcome of headless state auto-provisioning: the resolved override map plus
/// the set of ephemeral directories the provisioner created. The caller registers
/// the ephemeral dirs with the run-attempt cleanup scope so they are removed when
/// the run ends (ephemeral state must not persist across runs — #700).
#[derive(Debug, Default)]
pub(crate) struct HeadlessStateProvisionOutcome {
    pub(crate) overrides: HashMap<String, String>,
    pub(crate) ephemeral_dirs: Vec<PathBuf>,
}

/// True when `source_ref` is not a stable cross-run source identity: an empty
/// ref, or a path that lives under one of the ato-managed ephemeral roots
/// (`~/.ato/runs`, `~/.ato/cache`, `~/.ato/projections`). A github ref, a
/// registry ref, or a stable user-owned local path are all considered stable.
fn headless_source_ref_is_unstable(source_ref: &str) -> bool {
    let trimmed = source_ref.trim();
    if trimmed.is_empty() {
        return true;
    }
    use capsule::common::paths;
    let unstable_roots = [
        paths::ato_runs_dir(),
        paths::ato_cache_dir(),
        paths::ato_projections_dir(),
    ];
    let candidate = Path::new(trimmed);
    unstable_roots
        .iter()
        .any(|root| candidate.starts_with(root))
}

/// Headless / Connected Runner state auto-provisioning.
///
/// `ato run <source> --sandbox` (what `ato runner serve` spawns for every lease)
/// never receives a `--state` binding, so any recipe that declares a `[state.*]`
/// block would hard-error in `state_source_path` (`requires an explicit
/// persistent binding`) or fail container creation on the un-creatable
/// `/var/lib/ato/state` ephemeral base. The desktop / session path already
/// auto-provisions a per-source directory; this provides the equivalent for the
/// headless path so stateful capsules can run on a runner (#687).
///
/// State is keyed on a stable, source-derived `headless_state_instance_id`
/// (#700) — NOT on `manifest.name` alone, which would make two different sources
/// that share a `name` collide on the same state directory. The id is a
/// `canonical_hash` (JCS + `blake3`) over the stable inputs in
/// [`HeadlessStateKeyInputs`]; per-execution facts (execution id, ports, session
/// / process / container ids, dynamic env) are deliberately excluded so the same
/// source reuses its state across runs and revisions.
///
/// Durability splits the path scheme (#700):
/// - `persistent` → `~/.ato/state/run/<headless_state_instance_id>/<state-name>`
///   (stable, reused across runs).
/// - `ephemeral`  → `~/.ato/runs/<run-session-token>/state/<state-name>` (under
///   the per-run cleanup scope; the directory is returned in
///   [`HeadlessStateProvisionOutcome::ephemeral_dirs`] so the caller registers it
///   for removal when the run ends — ephemeral state must not survive the run).
///
/// An explicit `--state` binding (or workspace / lock seed) always wins: any
/// state already present in `existing_overrides` is left untouched.
///
/// Fails closed on any state `kind` other than `filesystem`: an auto-provisioned
/// host directory is only a meaningful backend for a filesystem state, and a
/// future non-filesystem kind must opt in explicitly rather than be silently
/// mis-provisioned as a directory.
fn auto_provision_headless_state_overrides(
    manifest: &CapsuleManifest,
    existing_overrides: &HashMap<String, String>,
    key_inputs: &HeadlessStateKeyInputs<'_>,
) -> Result<HeadlessStateProvisionOutcome> {
    use capsule::types::{StateDurability, StateKind, StateRequirement};

    let mut overrides = existing_overrides.clone();
    let mut ephemeral_dirs = Vec::new();

    // Short-circuit BEFORE any disk read or key derivation (#700 follow-up):
    // collect the states this run will actually auto-provision (declared,
    // not already bound). Fail closed here on any non-filesystem kind. If
    // nothing remains, return a pure no-op — we must NOT read `capsule.toml`,
    // compute `headless_state_instance_id` (which may `hash_tree` the source
    // tree), or touch `ato_state_dir()` for a capsule that has no unbound
    // filesystem state (e.g. pgweb). This keeps the no-state path free of the
    // source-read / canonicalize work that was platform-path-fragile.
    let mut unbound_filesystem_states: Vec<(&String, &StateRequirement)> = Vec::new();
    for (state_name, requirement) in &manifest.state {
        if overrides.contains_key(state_name) {
            continue;
        }
        // Fail closed: only filesystem state can be auto-provisioned as a host
        // directory. Any other kind must be bound explicitly.
        if requirement.kind != StateKind::Filesystem {
            anyhow::bail!(
                "state '{}' has kind {:?}, which cannot be auto-provisioned for a headless run; bind it explicitly with --state {}=...",
                state_name,
                requirement.kind,
                state_name
            );
        }
        unbound_filesystem_states.push((state_name, requirement));
    }
    if unbound_filesystem_states.is_empty() {
        return Ok(HeadlessStateProvisionOutcome {
            overrides,
            ephemeral_dirs,
        });
    }

    // Stable per-source persistent root, computed only when at least one unbound
    // persistent filesystem state actually needs it — `headless_state_instance_id`
    // can read the source tree (`hash_tree`) in the unstable-ref fallback, so it
    // must not run on the no-state / ephemeral-only paths. The instance id is the
    // same across re-runs of the same source (so persistent state is reused) and
    // differs between sources that share a `manifest.name` (so they never collide).
    let needs_persistent = unbound_filesystem_states
        .iter()
        .any(|(_, requirement)| requirement.durability == StateDurability::Persistent);
    let persistent_root = if needs_persistent {
        let instance_id = headless_state_instance_id(manifest, key_inputs)?;
        Some(
            capsule::common::paths::ato_state_dir()
                .join("run")
                .join(instance_id),
        )
    } else {
        None
    };

    // Per-run ephemeral root. The token provides filesystem uniqueness only and
    // lives under `~/.ato/runs`, which is the run cleanup root. Computed lazily so
    // a source with only persistent state never creates a runs/ subtree.
    let mut ephemeral_root: Option<PathBuf> = None;

    for (state_name, requirement) in unbound_filesystem_states {
        let path = match requirement.durability {
            StateDurability::Persistent => persistent_root
                .as_ref()
                .expect("persistent_root is computed when a persistent state exists")
                .join(state_name),
            StateDurability::Ephemeral => {
                let root = match ephemeral_root.as_ref() {
                    Some(root) => root.clone(),
                    None => {
                        // `ato_run_layout` mints a fresh `~/.ato/runs/state-…`
                        // root; we use its `root` as the per-run cleanup-scoped
                        // base and place state under `<root>/state/<name>`.
                        let root = capsule::common::paths::ato_run_layout("headless-state")
                            .root
                            .join("state");
                        ephemeral_root = Some(root.clone());
                        root
                    }
                };
                let dir = root.join(state_name);
                // Register the ephemeral *root* (the `~/.ato/runs/<token>` dir,
                // i.e. the parent of `state/`) so cleanup removes the whole
                // per-run subtree, matching the run cleanup scope.
                if let Some(run_root) = dir.ancestors().nth(2) {
                    let run_root = run_root.to_path_buf();
                    if !ephemeral_dirs.contains(&run_root) {
                        ephemeral_dirs.push(run_root);
                    }
                }
                dir
            }
        };

        fs::create_dir_all(&path).with_context(|| {
            format!(
                "failed to auto-provision headless state directory {}",
                path.display()
            )
        })?;
        let locator = path
            .canonicalize()
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        overrides.insert(state_name.clone(), locator);
    }

    Ok(HeadlessStateProvisionOutcome {
        overrides,
        ephemeral_dirs,
    })
}

/// Compute the stable `headless_state_instance_id` for a source's persistent
/// state root (#700). See [`HeadlessStateKeyInputs`] for the input contract.
fn headless_state_instance_id(
    manifest: &CapsuleManifest,
    inputs: &HeadlessStateKeyInputs<'_>,
) -> Result<String> {
    let fallback_tree_hash;
    let effective_source_ref = if headless_source_ref_is_unstable(inputs.normalized_source_ref) {
        // The source reference is not a stable identity (local path / anonymous
        // materialized source). Fall back to the content hash of the materialized
        // source tree so two different sources never collide, while re-runs of the
        // same tree resolve to the same instance. (Git commit SHA is deliberately
        // not used: it is provenance, not materialized-source identity.)
        fallback_tree_hash = capsule::blob::hash_tree(inputs.workspace_root_for_fallback)
            .map(|tree| tree.blob_hash)
            .with_context(|| {
                format!(
                    "failed to hash source tree for headless state identity at {}",
                    inputs.workspace_root_for_fallback.display()
                )
            })?;
        fallback_tree_hash.as_str()
    } else {
        inputs.normalized_source_ref.trim()
    };

    // The owner scope is the manifest's explicit `state_owner_scope` when set,
    // otherwise the manifest name (matching the documented default-owner-scope
    // semantics on `CapsuleManifest::state_owner_scope`).
    let owner_scope = manifest
        .state_owner_scope
        .as_deref()
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .unwrap_or_else(|| manifest.name.trim());

    let material = HeadlessStateKeyMaterial {
        v: 1,
        source_ref: effective_source_ref,
        name: owner_scope,
        target: inputs.selected_target_label.trim(),
        profile: inputs.profile_id.unwrap_or("default"),
        namespace: inputs.runner_namespace.unwrap_or("default"),
    };

    // `canonical_hash` returns `blake3:<hex>`; strip the algorithm prefix so the
    // value is a single path-safe segment.
    let hashed = capsule::foundation::install_lifecycle::canonical_hash(&material)
        .context("failed to derive headless state instance id")?;
    let id = hashed.split(':').next_back().unwrap_or(&hashed).to_string();
    Ok(id)
}

/// Resolve the stable `normalized_source_ref` for headless state identity from
/// the run request (#700).
///
/// Preference order:
/// 1. The preview session's `target_reference` (the canonical source ref the user
///    / runner asked for, e.g. `github.com/owner/repo`), when present.
/// 2. `use_existing_toml` when it names a source ref rather than a bare flag.
/// 3. The `request.target` as the user supplied it.
///
/// The returned string may be an unstable per-run materialization path; the
/// caller passes it through [`HeadlessStateKeyInputs`] together with the
/// workspace root, and [`headless_state_instance_id`] substitutes the source tree
/// hash when it detects an unstable ref.
fn headless_normalized_source_ref(
    request: &ConsumerRunRequest,
    preview_session: Option<&preview::PreviewSession>,
) -> String {
    if let Some(reference) = preview_session
        .map(|session| session.target_reference.trim())
        .filter(|reference| !reference.is_empty())
    {
        return reference.to_string();
    }
    if let Some(reference) = request
        .use_existing_toml
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
    {
        return reference.to_string();
    }
    request.target.to_string_lossy().to_string()
}

pub(crate) fn resolve_compatibility_host_mode(
    executor_kind: ExecutorKind,
    compatibility_fallback: Option<&str>,
) -> Result<CompatibilityHostMode> {
    match compatibility_fallback {
        None => Ok(CompatibilityHostMode::Disabled),
        Some("host")
            if matches!(
                executor_kind,
                ExecutorKind::Native | ExecutorKind::NodeCompat
            ) =>
        {
            Ok(CompatibilityHostMode::Enabled)
        }
        Some("host") => anyhow::bail!(
            "--compatibility-fallback host is only supported for native and node-compatible source targets"
        ),
        Some(other) => anyhow::bail!("unsupported compatibility fallback backend: {other}"),
    }
}

fn build_target_launch_options(
    request: &ConsumerRunRequest,
    preview_mode: bool,
) -> TargetLaunchOptions {
    TargetLaunchOptions {
        enforcement: request.enforcement.clone(),
        sandbox_mode: request.sandbox_mode,
        dangerously_skip_permissions: request.dangerously_skip_permissions,
        assume_yes: request.assume_yes,
        preview_mode,
        defer_consent: true,
    }
}

fn render_execution_roots_note(
    plan: &capsule::router::ManifestData,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
) -> Result<()> {
    let writable_mounts = launch_ctx
        .injected_mounts()
        .iter()
        .filter(|mount| !mount.readonly)
        .map(|mount| {
            format!(
                "{} <- {}",
                mount.target,
                crate::progressive_ui::format_path_for_note(&mount.source)
            )
        })
        .collect::<Vec<_>>();

    let body = format!(
        "Source Root       : {}\nMaterialized Root : {}\nEffective CWD     : {}\nWritable Mounts   : {}",
        crate::progressive_ui::format_path_for_note(&plan.workspace_root),
        crate::progressive_ui::format_path_for_note(&plan.manifest_dir),
        launch_ctx
            .effective_cwd()
            .map(|cwd| crate::progressive_ui::format_path_for_note(cwd.as_path()))
            .unwrap_or_else(|| "<none>".to_string()),
        if writable_mounts.is_empty() {
            "none".to_string()
        } else {
            writable_mounts.join("\n                  ")
        }
    );

    crate::progressive_ui::show_note("Run Context", body)
}

/// Build a stable identity key for port allocation.
/// Uses scoped_id (publisher/slug) when available, otherwise manifest path.
/// Appends target label when non-default to give each target its own port.
fn build_port_identity(
    manifest_path: &std::path::Path,
    target_label: &str,
    scoped_id: Option<&str>,
) -> String {
    let base = scoped_id
        .map(String::from)
        .unwrap_or_else(|| manifest_path.to_string_lossy().to_string());
    if target_label.is_empty() || target_label == "default" {
        base
    } else {
        format!("{}:{}", base, target_label)
    }
}

/// Reconcile a capsule.toml `toml::Value` so that the lock's selected-target
/// label can be used to look up lifecycle fields (build, runtime_tools, install)
/// that the manifest declares under a different target name.
///
/// Source inference always assigns `label = "default"` to the inferred lock
/// target regardless of the capsule.toml `default_target` value.  When the
/// two labels differ (e.g. lock has `"default"` but capsule.toml declares
/// `[targets.main]`), all `compat_str(&["targets", "default", …])` lookups
/// return `None` and the lifecycle steps are silently skipped.
///
/// This function inserts `[targets.<lock_target>]` as an alias of the
/// capsule.toml target so that both the lock-label path and the manifest-label
/// path resolve correctly.  The alias is only added in-memory; the file on
/// disk is never touched.
fn reconcile_compat_manifest_targets(raw: &toml::Value, lock_target: &str) -> toml::Value {
    let mut value = raw.clone();

    // Determine the capsule.toml's intended target label: prefer explicit
    // default_target, then fall back to the sole entry in [targets] if there
    // is exactly one.
    let capsule_target = value
        .get("default_target")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("targets")
                .and_then(toml::Value::as_table)
                .filter(|t| t.len() == 1)
                .and_then(|t| t.keys().next())
                .map(|s| s.to_string())
        });

    if let Some(capsule_target) = capsule_target
        && capsule_target != lock_target
        && let Some(targets) = value.get_mut("targets").and_then(toml::Value::as_table_mut)
        && let Some(real_target) = targets.get(capsule_target.as_str()).cloned()
    {
        // Only add the alias if the lock-label slot is not already occupied.
        targets
            .entry(lock_target.to_string())
            .or_insert(real_target);
    }

    value
}

#[cfg(test)]
mod tests {
    use super::{
        ConsumerRunRequest, DerivedBridgeManifest, ExternalServiceContract,
        ExternalServiceHealthcheck, ExternalServiceHealthcheckKind, ExternalServiceMode,
        HeadlessStateKeyInputs, PreparedRunContext, RunPipelineState, ServiceRequiredAsset,
        collect_port_preferences, headless_state_instance_id, normalize_existing_path,
        normalize_write_path, parent_package_id, parse_external_service_contracts,
        parse_reuse_if_present_service_preflights, reconcile_compat_manifest_targets,
        resolve_sandbox_grants, sandbox_session_data_env, sandbox_session_data_env_dir,
        unavailable_service_message, validate_sandbox_grants_best_effort,
    };
    use capsule::ato_lock::AtoLock;
    use capsule::types::{CapsuleManifest, ParamValue};
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::reporters::CliReporter;

    // ── #748: native-inference (host execution) skips the nacelle sandbox preflight ──
    #[test]
    fn native_inference_skips_sandbox_preflight() {
        use super::should_run_native_sandbox_preflight;
        use capsule::execution_plan::guard::ExecutorKind;
        // Sandboxed Tier2 (source/python): runs the preflight.
        assert!(should_run_native_sandbox_preflight(
            ExecutorKind::Native,
            false,
            false,
            false
        ));
        // native-inference is ExecutorKind::Native but host-native → MUST skip (the #748 fix).
        assert!(!should_run_native_sandbox_preflight(
            ExecutorKind::Native,
            false,
            false,
            true
        ));
        // dangerously-skip / host-fallback also skip (host execution).
        assert!(!should_run_native_sandbox_preflight(
            ExecutorKind::Native,
            true,
            false,
            false
        ));
        assert!(!should_run_native_sandbox_preflight(
            ExecutorKind::Native,
            false,
            true,
            false
        ));
    }

    // native-inference engine_variant fail-closed tests moved to
    // `capsule::routing::native_inference::tests` (now exercised through the
    // `LlamaCppEngine::plan_variant` trait method).

    #[test]
    fn sandbox_session_data_env_sets_absent_keys_only() {
        // Nothing set: both runtime keys injected, pointing at the guest dir.
        let env = sandbox_session_data_env("/runs/ato/session", |_| false);
        assert_eq!(
            env.get("ATO_DATA_DIR").map(String::as_str),
            Some("/runs/ato/session")
        );
        assert_eq!(
            env.get("DATABASE_PATH").map(String::as_str),
            Some("/runs/ato/session/app.db")
        );

        // Capsule/user already set DATABASE_PATH: do NOT override it; still set
        // ATO_DATA_DIR.
        let env = sandbox_session_data_env("/runs/ato/session", |k| k == "DATABASE_PATH");
        assert!(
            !env.contains_key("DATABASE_PATH"),
            "must not override DATABASE_PATH"
        );
        assert!(env.contains_key("ATO_DATA_DIR"));

        // Both already set: inject nothing.
        let env = sandbox_session_data_env("/runs/ato/session", |_| true);
        assert!(env.is_empty());
    }

    #[test]
    fn sandbox_session_data_env_dir_is_platform_correct() {
        // #628: macOS seatbelt has no mount namespace, so the data-path env must
        // reference the writable HOST session dir — not the guest `/runs/...`
        // path (which would make a stateful capsule mkdir `/runs` on the
        // read-only host root and exit before readiness). Linux/other backends
        // remap the host dir to the guest path, so the env uses the guest path.
        let host = std::path::Path::new("/Users/x/.ato/runs/session-data/123-456");
        let chosen = sandbox_session_data_env_dir("/runs/ato/session", host);

        if cfg!(target_os = "macos") {
            assert_eq!(chosen, host.to_string_lossy());
            // The whole point: the env value is NOT the guest root path.
            assert_ne!(chosen, "/runs/ato/session");
            // And the derived DB path lives under the writable host dir.
            let env = sandbox_session_data_env(&chosen, |_| false);
            assert_eq!(
                env.get("DATABASE_PATH").map(String::as_str),
                Some(format!("{}/app.db", host.to_string_lossy()).as_str())
            );
        } else {
            assert_eq!(chosen, "/runs/ato/session");
        }
    }

    fn empty_host_env() -> crate::application::dependency_credentials::MapHostEnv {
        crate::application::dependency_credentials::MapHostEnv::new(&[])
    }

    #[test]
    fn collect_port_preferences_records_concrete_ports_and_explicit_auto() {
        use crate::executors::launch_context::PortPreference;
        use capsule::installed_state::{
            LaunchConditionInput, LaunchConditionInputKind, LaunchConditionInputValue,
        };
        let port = |key: &str, value: &str| LaunchConditionInput {
            kind: LaunchConditionInputKind::Port,
            key: key.to_string(),
            value: LaunchConditionInputValue::Literal(value.to_string()),
        };
        let inputs = vec![
            port("main", "3001"),
            port("admin", "auto"),
            port("api", "8080"),
            // Non-port inputs are ignored.
            LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: "OPENAI_API_KEY".to_string(),
                value: LaunchConditionInputValue::Grant("g1".to_string()),
            },
        ];
        let prefs = collect_port_preferences(&inputs);
        assert_eq!(prefs.get("main"), Some(&PortPreference::Concrete(3001)));
        assert_eq!(prefs.get("api"), Some(&PortPreference::Concrete(8080)));
        assert_eq!(
            prefs.get("admin"),
            Some(&PortPreference::Auto),
            "port=auto is recorded as explicit Auto (suppresses env-PORT fallback)"
        );
        assert_eq!(prefs.len(), 3);
    }

    #[test]
    fn collect_port_preferences_drops_unparseable_literal() {
        use capsule::installed_state::{
            LaunchConditionInput, LaunchConditionInputKind, LaunchConditionInputValue,
        };
        // Parsing already rejects non-numeric / out-of-range port literals, but the
        // collector defensively drops anything that is neither `auto` nor a u16.
        let inputs = vec![LaunchConditionInput {
            kind: LaunchConditionInputKind::Port,
            key: "main".to_string(),
            value: LaunchConditionInputValue::Literal("not-a-port".to_string()),
        }];
        assert!(collect_port_preferences(&inputs).is_empty());
    }

    #[test]
    fn collect_port_preferences_empty_for_no_port_inputs() {
        assert!(collect_port_preferences(&[]).is_empty());
    }

    fn workspace_tempdir(name: &str) -> tempfile::TempDir {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".ato")
            .join("test-scratch");
        fs::create_dir_all(&root).expect("create workspace .ato/test-scratch");
        tempfile::Builder::new()
            .prefix(name)
            .tempdir_in(root)
            .expect("workspace tempdir")
    }

    /// PR-3b (PR #180 review fix): contract for the boundary plumbing
    /// helper. The helper sets `receipt_graph_id_sink: Some(...)` AND
    /// the resulting state shares the same `Arc`-backed cell as the
    /// sink passed in — so a `sink.set(...)` call from the outer
    /// wrapper is observable through the state-side handle the
    /// Execute phase reads.
    #[test]
    fn attach_receipt_graph_id_sink_populates_pipeline_state() {
        use crate::application::receipt_boundary::{GraphIds, ReceiptGraphIdSink};

        let workspace = workspace_tempdir("attach-sink-fixture");
        let manifest_dir = workspace.path().to_path_buf();
        // Minimal manifest with a default target so
        // `execution_descriptor_from_manifest_parts` succeeds.
        let mut manifest = toml::map::Map::new();
        manifest.insert(
            "schema_version".to_string(),
            toml::Value::String("0.3".to_string()),
        );
        manifest.insert(
            "name".to_string(),
            toml::Value::String("attach-sink-demo".to_string()),
        );
        manifest.insert(
            "version".to_string(),
            toml::Value::String("0.1.0".to_string()),
        );
        manifest.insert("type".to_string(), toml::Value::String("app".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );
        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("native".to_string()),
        );
        target.insert(
            "run".to_string(),
            toml::Value::String("/usr/bin/true".to_string()),
        );
        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        let plan = capsule::router::execution_descriptor_from_manifest_parts(
            toml::Value::Table(manifest),
            manifest_dir.join("capsule.toml"),
            manifest_dir.clone(),
            capsule::router::ExecutionProfile::Dev,
            Some("default"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor");

        let decision = capsule::router::RuntimeDecision {
            kind: capsule::router::RuntimeKind::Source,
            reason: "test fixture".to_string(),
            plan,
        };
        let prepared = PreparedRunContext::from_authoritative_input(
            None,
            &manifest_dir,
            capsule::types::ValidationMode::Strict,
            Some("default"),
        )
        .expect("prepared run context");

        let state = RunPipelineState {
            preview_session: None,
            preview_mode: false,
            use_progressive_ui: false,
            prepared,
            decision,
            launch_ctx: crate::executors::launch_context::RuntimeLaunchContext::empty(),
            external_capsules: None,
            dep_contracts: None,
            agent_attempted: false,
            derived_execution: None,
            compatibility_host_mode: None,
            native_nacelle: None,
            build_observation: None,
            build_decision_kind: None,
            receipt_graph_id_sink: None,
        };

        assert!(
            state.receipt_graph_id_sink.is_none(),
            "fixture sanity: freshly built state has no sink"
        );

        let sink = ReceiptGraphIdSink::new();
        let state = super::attach_receipt_graph_id_sink(state, sink.clone());

        assert!(
            state.receipt_graph_id_sink.is_some(),
            "PR-3b: attach_receipt_graph_id_sink must populate the field"
        );

        // Cross-Arc contract: a publish to the original `sink` must
        // be visible to the state-side handle. This is what makes the
        // helper meaningful — both sides observe the same cell.
        sink.set(GraphIds {
            declared_execution_id: Some("blake3:test-declared".to_string()),
            resolved_execution_id: Some("blake3:test-resolved".to_string()),
        });
        let state_sink = state.receipt_graph_id_sink.as_ref().unwrap();
        let snapshot = state_sink.snapshot();
        assert_eq!(
            snapshot.declared_execution_id.as_deref(),
            Some("blake3:test-declared"),
            "PR-3b: state-side sink must share the Arc with the input sink"
        );
        assert_eq!(
            snapshot.resolved_execution_id.as_deref(),
            Some("blake3:test-resolved")
        );
    }

    #[test]
    fn parent_package_id_uses_manifest_name_and_version() {
        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "demo"
version = "1.2.3"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "native"
run = "/usr/bin/true"
"#,
        )
        .expect("manifest");

        assert_eq!(parent_package_id(&manifest), "demo@1.2.3");
    }

    /// Scoped `ATO_HOME` guard so the auto-provisioning tests resolve
    /// `ato_state_dir()` under a tempdir and restore the prior value on drop.
    struct AtoHomeGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl AtoHomeGuard {
        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("ATO_HOME");
            // SAFETY: tests touching ATO_HOME run under `#[serial]`.
            unsafe { std::env::set_var("ATO_HOME", path) };
            Self { previous }
        }
    }

    impl Drop for AtoHomeGuard {
        fn drop(&mut self) {
            // SAFETY: tests touching ATO_HOME run under `#[serial]`.
            unsafe {
                match self.previous.take() {
                    Some(value) => std::env::set_var("ATO_HOME", value),
                    None => std::env::remove_var("ATO_HOME"),
                }
            }
        }
    }

    fn persistent_state_manifest() -> CapsuleManifest {
        CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "gitea"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/go-gitea/gitea:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/data"
"#,
        )
        .expect("manifest")
    }

    /// Same manifest shape as `persistent_state_manifest`, but the state is
    /// declared `durability = "ephemeral"` (#700 ephemeral path test).
    fn ephemeral_state_manifest() -> CapsuleManifest {
        CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "scratch"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/scratch:latest"

[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "scratch"
attach = "explicit"
schema_id = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/data"
"#,
        )
        .expect("manifest")
    }

    /// Build `HeadlessStateKeyInputs` for a stable source ref. The fallback
    /// workspace root is irrelevant for a stable ref but must point somewhere.
    fn stable_key_inputs<'a>(
        source_ref: &'a str,
        workspace_root: &'a Path,
    ) -> HeadlessStateKeyInputs<'a> {
        HeadlessStateKeyInputs {
            normalized_source_ref: source_ref,
            selected_target_label: "default",
            profile_id: None,
            runner_namespace: None,
            workspace_root_for_fallback: workspace_root,
        }
    }

    #[test]
    #[serial_test::serial]
    fn headless_auto_provision_creates_dir_for_unbound_persistent_state() {
        // #687: `ato run <source> --sandbox` has no `--state`, so a recipe with a
        // `[state.*]` block would hard-error. Auto-provisioning binds a writable
        // per-source directory under `~/.ato/state/run/<instance-id>/` instead.
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = AtoHomeGuard::set(home.path());

        let manifest = persistent_state_manifest();
        let workspace = home.path().to_path_buf();
        let inputs = stable_key_inputs("github.com/owner/gitea", &workspace);
        let outcome =
            super::auto_provision_headless_state_overrides(&manifest, &HashMap::new(), &inputs)
                .expect("auto-provision");

        let instance_id =
            headless_state_instance_id(&manifest, &inputs).expect("derive instance id");
        let bound = outcome.overrides.get("data").expect("data is auto-bound");
        let expected = home
            .path()
            .join("state")
            .join("run")
            .join(&instance_id)
            .join("data");
        assert_eq!(
            fs::canonicalize(bound).expect("bound dir exists"),
            fs::canonicalize(&expected).expect("expected dir exists"),
        );
        assert!(expected.is_dir(), "auto-provisioned dir must be created");
        assert!(
            outcome.ephemeral_dirs.is_empty(),
            "persistent state must not register ephemeral cleanup dirs"
        );
    }

    #[test]
    #[serial_test::serial]
    fn headless_auto_provision_preserves_existing_binding() {
        // #700: an explicit `--state data=/path` (or workspace/lock binding) wins;
        // auto-provisioning must NEVER override an explicit binding, regardless of
        // durability.
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = AtoHomeGuard::set(home.path());

        let manifest = persistent_state_manifest();
        let mut existing = HashMap::new();
        existing.insert("data".to_string(), "/explicit/path".to_string());
        let workspace = home.path().to_path_buf();
        let inputs = stable_key_inputs("github.com/owner/gitea", &workspace);

        let outcome = super::auto_provision_headless_state_overrides(&manifest, &existing, &inputs)
            .expect("auto-provision");

        assert_eq!(
            outcome.overrides.get("data").map(String::as_str),
            Some("/explicit/path"),
            "explicit binding must take precedence over auto-provisioning"
        );
        let instance_id =
            headless_state_instance_id(&manifest, &inputs).expect("derive instance id");
        assert!(
            !home
                .path()
                .join("state")
                .join("run")
                .join(&instance_id)
                .exists(),
            "must not create a dir when the state is already bound"
        );
        assert!(outcome.ephemeral_dirs.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn headless_auto_provision_is_noop_without_state_block() {
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = AtoHomeGuard::set(home.path());
        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "stateless"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
"#,
        )
        .expect("manifest");

        let workspace = home.path().to_path_buf();
        let inputs = stable_key_inputs("github.com/owner/stateless", &workspace);
        let outcome =
            super::auto_provision_headless_state_overrides(&manifest, &HashMap::new(), &inputs)
                .expect("auto-provision");
        assert!(outcome.overrides.is_empty());
        assert!(outcome.ephemeral_dirs.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn headless_auto_provision_no_state_path_reads_nothing() {
        // #700 follow-up (CI regression on macos/windows): the no-provision path
        // must short-circuit BEFORE any source read or key derivation. We prove it
        // here by handing the function an UNSTABLE source ref together with a
        // workspace root that DOES NOT EXIST: if the function reached
        // `headless_state_instance_id` it would `hash_tree` that path and return
        // Err. A clean Ok with empty overrides proves no source read happened.
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = AtoHomeGuard::set(home.path());

        let missing_workspace = home.path().join("does-not-exist");
        assert!(!missing_workspace.exists());
        // Unstable ref (empty string) so the fallback WOULD trigger a hash_tree if
        // the function ever derived the key.
        let inputs = HeadlessStateKeyInputs {
            normalized_source_ref: "",
            selected_target_label: "default",
            profile_id: None,
            runner_namespace: None,
            workspace_root_for_fallback: &missing_workspace,
        };

        // Case A: no `[state.*]` block at all.
        let stateless = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "stateless"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
"#,
        )
        .expect("manifest");
        let outcome =
            super::auto_provision_headless_state_overrides(&stateless, &HashMap::new(), &inputs)
                .expect("no-state path must be a pure no-op (no source read)");
        assert!(outcome.overrides.is_empty());
        assert!(outcome.ephemeral_dirs.is_empty());

        // Case B: a persistent state that is ALREADY bound — also nothing to
        // provision, so still no source read despite the unstable ref + missing
        // workspace.
        let manifest = persistent_state_manifest();
        let mut existing = HashMap::new();
        existing.insert("data".to_string(), "/explicit/path".to_string());
        let outcome = super::auto_provision_headless_state_overrides(&manifest, &existing, &inputs)
            .expect("fully-bound path must be a pure no-op (no source read)");
        assert_eq!(
            outcome.overrides.get("data").map(String::as_str),
            Some("/explicit/path")
        );
        assert!(outcome.ephemeral_dirs.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn headless_ephemeral_only_path_does_not_derive_persistent_key() {
        // An ephemeral-only capsule must NOT compute the persistent instance id
        // (which can read the source tree). Prove it the same way: unstable ref +
        // missing workspace would error if `headless_state_instance_id` ran, but
        // the ephemeral branch never calls it.
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = AtoHomeGuard::set(home.path());

        let missing_workspace = home.path().join("does-not-exist");
        let inputs = HeadlessStateKeyInputs {
            normalized_source_ref: "",
            selected_target_label: "default",
            profile_id: None,
            runner_namespace: None,
            workspace_root_for_fallback: &missing_workspace,
        };

        let manifest = ephemeral_state_manifest();
        let outcome =
            super::auto_provision_headless_state_overrides(&manifest, &HashMap::new(), &inputs)
                .expect("ephemeral-only path must not derive the persistent key (no source read)");
        // Ephemeral state is still provisioned (under ~/.ato/runs), it just does
        // not need the persistent key derivation.
        assert!(outcome.overrides.contains_key("data"));
        assert!(!outcome.ephemeral_dirs.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn headless_state_instance_id_differs_for_same_name_different_source() {
        // #700 defect 1: keying state on `manifest.name` alone makes two DIFFERENT
        // sources that happen to share a `name` collide on the SAME state directory
        // (an isolation hazard). The source-derived instance id must distinguish
        // them so they resolve to DIFFERENT state dirs.
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = AtoHomeGuard::set(home.path());

        // Same manifest (same `name = "gitea"`), two distinct stable source refs.
        let manifest = persistent_state_manifest();
        let workspace = home.path().to_path_buf();

        let inputs_a = stable_key_inputs("github.com/alice/gitea", &workspace);
        let inputs_b = stable_key_inputs("github.com/bob/gitea", &workspace);

        let outcome_a =
            super::auto_provision_headless_state_overrides(&manifest, &HashMap::new(), &inputs_a)
                .expect("auto-provision A");
        let outcome_b =
            super::auto_provision_headless_state_overrides(&manifest, &HashMap::new(), &inputs_b)
                .expect("auto-provision B");

        let dir_a = outcome_a.overrides.get("data").expect("A bound");
        let dir_b = outcome_b.overrides.get("data").expect("B bound");
        assert_ne!(
            dir_a, dir_b,
            "two different sources sharing manifest.name must NOT share a state dir"
        );

        // And re-running the SAME source must reuse the SAME dir (stability).
        let outcome_a2 =
            super::auto_provision_headless_state_overrides(&manifest, &HashMap::new(), &inputs_a)
                .expect("auto-provision A re-run");
        assert_eq!(
            dir_a,
            outcome_a2.overrides.get("data").expect("A re-run bound"),
            "re-running the same source must reuse its persistent state dir"
        );
    }

    #[test]
    #[serial_test::serial]
    fn headless_ephemeral_state_does_not_land_in_persistent_root() {
        // #700 defect 2: `durability = "ephemeral"` state must NOT be bound to the
        // stable persistent root (`~/.ato/state/run/...`); it must land under a
        // per-run/session-scoped path (`~/.ato/runs/...`) that is registered for
        // cleanup so it does not persist across runs.
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = AtoHomeGuard::set(home.path());

        let manifest = ephemeral_state_manifest();
        let workspace = home.path().to_path_buf();
        let inputs = stable_key_inputs("github.com/owner/scratch", &workspace);

        let outcome =
            super::auto_provision_headless_state_overrides(&manifest, &HashMap::new(), &inputs)
                .expect("auto-provision");

        let bound = outcome.overrides.get("data").expect("data is auto-bound");
        let bound_path = fs::canonicalize(bound).expect("bound dir exists");

        let persistent_root = fs::canonicalize(home.path().join("state").join("run"))
            .unwrap_or_else(|_| home.path().join("state").join("run"));
        assert!(
            !bound_path.starts_with(&persistent_root),
            "ephemeral state must NOT live under the persistent state root: {}",
            bound_path.display()
        );

        let runs_root = fs::canonicalize(home.path().join("runs"))
            .expect("runs root exists once an ephemeral dir is provisioned");
        assert!(
            bound_path.starts_with(&runs_root),
            "ephemeral state must live under the per-run runs root: {}",
            bound_path.display()
        );

        // The per-run root must be registered for cleanup so it is removed when the
        // run ends (ephemeral state must not survive the run), and that root must
        // be an ancestor of the bound dir.
        assert!(
            !outcome.ephemeral_dirs.is_empty(),
            "ephemeral state must register a cleanup dir"
        );
        assert!(
            outcome
                .ephemeral_dirs
                .iter()
                .any(|dir| bound_path
                    .starts_with(fs::canonicalize(dir).unwrap_or_else(|_| dir.clone()))),
            "a registered cleanup dir must be an ancestor of the ephemeral state dir"
        );
    }

    #[test]
    fn resolve_state_overrides_sandbox_tolerates_unbound_persistent_state() {
        // #687 fix wiring: the non-authoritative `ato run <source> --sandbox`
        // branch routes through `resolve_explicit_or_auto_state_source_overrides`
        // with `sandbox_mode = true` and no `--state`. It must NOT hard-error on
        // the declared-but-unbound persistent state (the bug); it defers that
        // state to the auto-provisioner, which fills it immediately after.
        //
        // This pins the actual fix: if the sandbox guard in
        // `resolve_explicit_or_auto_state_source_overrides` were removed, this
        // run would route through the strict resolver and reintroduce the
        // "requires an explicit --state" error this test asserts is gone.
        let manifest = persistent_state_manifest();
        let mut request = sandbox_request(
            std::env::temp_dir(),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        request.sandbox_mode = true;
        request.state_bindings = Vec::new();

        let overrides = super::resolve_explicit_or_auto_state_source_overrides(&manifest, &request)
            .expect("sandbox run must not error on unbound persistent state");
        // The unbound persistent state is intentionally left for the
        // auto-provisioner, so the lenient resolver returns nothing for it.
        assert!(
            !overrides.contains_key("data"),
            "lenient resolver leaves unbound state to the auto-provisioner"
        );
    }

    #[test]
    fn resolve_state_overrides_non_sandbox_still_fails_closed() {
        // Fail-closed contract preserved for non-sandbox runs: a declared
        // persistent state with no `--state` binding is still an error. This
        // guards against the #687 fix accidentally loosening the normal path.
        let manifest = persistent_state_manifest();
        let mut request = sandbox_request(
            std::env::temp_dir(),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        request.sandbox_mode = false;
        request.state_bindings = Vec::new();

        let err = super::resolve_explicit_or_auto_state_source_overrides(&manifest, &request)
            .expect_err("non-sandbox run with unbound persistent state must fail closed");
        assert!(
            err.to_string().contains("requires an explicit --state"),
            "expected fail-closed binding error, got: {err}"
        );
    }

    #[test]
    fn locked_dependency_resolved_ref_prefers_content_digest() {
        let locked = capsule::lockfile::LockedCapsuleDependency {
            name: "db".to_string(),
            source: "capsule://ato/postgres@16".to_string(),
            source_type: "store".to_string(),
            contract: Some("service@1".to_string()),
            injection_bindings: BTreeMap::new(),
            parameters: BTreeMap::from([(
                "database".to_string(),
                ParamValue::String("app".to_string()),
            )]),
            credentials: BTreeMap::new(),
            identity_exports: BTreeMap::new(),
            resolved_version: Some("16.1.0".to_string()),
            digest: Some("blake3:abc".to_string()),
            sha256: Some("sha256:def".to_string()),
            artifact_url: Some("https://example.test/postgres.capsule".to_string()),
        };

        assert_eq!(
            super::locked_dependency_resolved_ref(&locked),
            "capsule://ato/postgres@16#blake3:abc"
        );
    }

    #[test]
    fn dependency_contract_start_error_surfaces_alive_other_session_owner() {
        let error = super::dependency_contract_start_error(
            "app",
            crate::application::dependency_runtime::orchestrator::OrchestratorError::OrphanAliveOtherSession {
                alias: "db".to_string(),
                session_pid: 4242,
                resolved: "capsule://github.com/Koh0920/ato-postgres@65b3ee5".to_string(),
                state_dir: PathBuf::from("/Users/example/.ato/state/wasedap2p/db"),
            },
        );
        let message = error.to_string();
        assert!(message.contains("dep 'db' state.dir is owned by ato session pid 4242"));
        assert!(message.contains("capsule://github.com/Koh0920/ato-postgres@65b3ee5"));
    }

    #[test]
    fn prepared_run_context_with_bridge_manifest_retains_authority() {
        let prepared = PreparedRunContext {
            authoritative_lock: Some(AtoLock::default()),
            lock_path: None,
            workspace_root: PathBuf::from("."),
            effective_state: Some(
                crate::application::workspace::state::EffectiveLockState::default(),
            ),
            execution_override: None,
            bridge_manifest: DerivedBridgeManifest::new(toml::Value::String("old".to_string())),
            validation_mode: capsule::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
            install_profile_key: Some("ipk_authority".to_string()),
        };

        let rerouted = prepared.with_bridge_manifest(
            toml::Value::String("new".to_string()),
            capsule::types::ValidationMode::Preview,
            true,
        );

        assert!(rerouted.authoritative_lock.is_some());
        assert!(rerouted.lock_path.is_none());
        assert_eq!(rerouted.workspace_root, PathBuf::from("."));
        assert!(rerouted.effective_state.is_some());
        assert_eq!(
            rerouted.bridge_manifest.as_toml(),
            &toml::Value::String("new".to_string())
        );
        assert_eq!(
            rerouted.validation_mode,
            capsule::types::ValidationMode::Preview
        );
        assert!(rerouted.engine_override_declared);
        // Install identity must survive the reroute so the rerouted launch
        // context resolution still stamps it (#508).
        assert_eq!(
            rerouted.install_profile_key.as_deref(),
            Some("ipk_authority")
        );
    }

    #[test]
    fn dependency_contract_env_preflight_covers_parameters_credentials_and_top_level_required_env()
    {
        let manifest_value: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
default_target = "app"
required_env = ["ATO_TEST_TOP_LEVEL_REQUIRED", "ATO_TEST_CRED_REQUIRED"]

[dependencies.db]
capsule = "capsule://ato/postgres@16"
contract = "service@1"

  [dependencies.db.parameters]
  password = "{{env.ATO_TEST_PARAM_REQUIRED}}"

  [dependencies.db.credentials]
  token = "{{env.ATO_TEST_CRED_REQUIRED}}"

[targets.app]
runtime = "source"
driver = "python"
run = "python main.py"
"#,
        )
        .expect("parse manifest");

        let report = super::collect_missing_dependency_contract_manifest_env(
            &manifest_value,
            &empty_host_env(),
        )
        .expect("collect env");

        assert_eq!(
            report.keys,
            vec![
                "ATO_TEST_PARAM_REQUIRED".to_string(),
                "ATO_TEST_CRED_REQUIRED".to_string(),
                "ATO_TEST_TOP_LEVEL_REQUIRED".to_string(),
            ]
        );
        assert_eq!(
            report.schema[0].label.as_deref(),
            Some("dep 'db'.parameters.password → {env.ATO_TEST_PARAM_REQUIRED}")
        );
        assert_eq!(
            report.schema[1].label.as_deref(),
            Some("dep 'db'.credentials.token → {env.ATO_TEST_CRED_REQUIRED}")
        );
        assert_eq!(report.schema[2].label, None);
    }

    #[test]
    fn dependency_contract_env_preflight_deduplicates_top_level_scope_and_dependency_reference() {
        let manifest_value: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
default_target = "app"
required_env = ["ATO_TEST_SHARED_REQUIRED"]

[dependencies.db]
capsule = "capsule://ato/postgres@16"
contract = "service@1"

  [dependencies.db.credentials]
  password = "{{env.ATO_TEST_SHARED_REQUIRED}}"

[targets.app]
runtime = "source"
driver = "python"
run = "python main.py"
"#,
        )
        .expect("parse manifest");

        let report = super::collect_missing_dependency_contract_manifest_env(
            &manifest_value,
            &empty_host_env(),
        )
        .expect("collect env");

        assert_eq!(report.keys, vec!["ATO_TEST_SHARED_REQUIRED".to_string()]);
        assert_eq!(
            report.schema[0].label.as_deref(),
            Some("dep 'db'.credentials.password → {env.ATO_TEST_SHARED_REQUIRED}")
        );
    }

    /// Creates `link -> target`, returning false when the host cannot create
    /// symlinks (Windows without Developer Mode/admin reports error 1314) so
    /// callers can skip rather than fail. CI runners are elevated, so the
    /// assertions still run there.
    fn symlink_dir_or_skip(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let result = std::os::windows::fs::symlink_dir(target, link);
        match result {
            Ok(()) => true,
            Err(err) if err.raw_os_error() == Some(1314) => {
                eprintln!("skipping: creating symlinks needs Developer Mode or admin rights");
                false
            }
            Err(err) => panic!("create symlink: {err:?}"),
        }
    }

    #[test]
    fn existing_grant_rejects_symlink_traversal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside_dir = tempfile::tempdir().expect("outside tempdir");
        let link_path = temp.path().join("outside-link");

        if !symlink_dir_or_skip(outside_dir.path(), &link_path) {
            return;
        }

        let err = normalize_existing_path(&link_path).expect_err("must reject symlink grants");
        assert!(err.to_string().contains("traverses symlink"));
    }

    #[test]
    fn write_grant_rejects_missing_file_under_symlink_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside_dir = tempfile::tempdir().expect("outside tempdir");
        let link_path = temp.path().join("outside-link");

        if !symlink_dir_or_skip(outside_dir.path(), &link_path) {
            return;
        }

        let err = normalize_write_path(&link_path.join("output.txt"))
            .expect_err("must reject symlink parent traversal");
        assert!(err.to_string().contains("traverses symlink"));
    }

    #[test]
    fn parse_reuse_if_present_service_preflights_reads_healthcheck_and_model() {
        let manifest: toml::Value = toml::from_str(
            r#"
[services.ollama]
from = "dependency:ollama"
mode = "reuse-if-present"

[services.ollama.healthcheck]
kind = "http"
url = "http://127.0.0.1:11434/api/tags"

[bootstrap.defaults]
ollama_model = "qwen2:7b"
"#,
        )
        .expect("parse manifest");

        let preflights = parse_reuse_if_present_service_preflights(&manifest);
        assert_eq!(preflights.len(), 1);
        let preflight = &preflights[0];
        assert_eq!(preflight.service_name, "ollama");
        assert_eq!(preflight.source_ref, "dependency:ollama");
        assert_eq!(preflight.mode, ExternalServiceMode::ReuseIfPresent);
        assert_eq!(
            preflight
                .healthcheck
                .as_ref()
                .map(|value| value.endpoint.as_str()),
            Some("http://127.0.0.1:11434/api/tags")
        );
        assert_eq!(
            preflight.required_assets,
            vec![ServiceRequiredAsset::OllamaModel {
                model: "qwen2:7b".to_string()
            }]
        );
    }

    #[test]
    fn parse_reuse_if_present_service_preflights_ignores_other_service_modes() {
        let manifest: toml::Value = toml::from_str(
            r#"
[services.ollama]
from = "dependency:ollama"
mode = "managed"
"#,
        )
        .expect("parse manifest");

        assert!(parse_reuse_if_present_service_preflights(&manifest).is_empty());
    }

    #[test]
    fn parse_external_service_contracts_reads_generic_service_without_ollama_defaults() {
        let manifest: toml::Value = toml::from_str(
            r#"
[services.cache]
from = "dependency:cache"
mode = "reuse-if-present"

[services.cache.healthcheck]
kind = "tcp"
url = "127.0.0.1:6380"
"#,
        )
        .expect("parse manifest");

        let services = parse_external_service_contracts(&manifest);
        assert_eq!(services.len(), 1);
        let service = &services[0];
        assert_eq!(service.service_name, "cache");
        assert_eq!(service.source_ref, "dependency:cache");
        assert_eq!(service.mode, ExternalServiceMode::ReuseIfPresent);
        assert_eq!(
            service.healthcheck,
            Some(ExternalServiceHealthcheck {
                kind: ExternalServiceHealthcheckKind::Tcp,
                endpoint: "127.0.0.1:6380".to_string(),
            })
        );
        assert!(service.required_assets.is_empty());
    }

    #[test]
    fn parse_external_service_contracts_preserves_managed_and_required_external_modes() {
        let manifest: toml::Value = toml::from_str(
            r#"
[services.cache]
from = "dependency:cache"
mode = "managed"

[services.cache.healthcheck]
kind = "tcp"
url = "127.0.0.1:6380"

[services.catalog]
from = "dependency:catalog"
mode = "required-external"

[services.catalog.healthcheck]
kind = "http"
url = "http://127.0.0.1:8787/health"
"#,
        )
        .expect("parse manifest");

        let services = parse_external_service_contracts(&manifest);
        assert_eq!(services.len(), 2);
        let cache = services
            .iter()
            .find(|service| service.service_name == "cache")
            .expect("cache service");
        let catalog = services
            .iter()
            .find(|service| service.service_name == "catalog")
            .expect("catalog service");
        assert_eq!(cache.mode, ExternalServiceMode::Managed);
        assert_eq!(catalog.mode, ExternalServiceMode::RequiredExternal);
    }

    #[test]
    fn unavailable_service_message_is_generic_for_managed_mode() {
        let service = ExternalServiceContract {
            service_name: "cache".to_string(),
            source_ref: "dependency:cache".to_string(),
            mode: ExternalServiceMode::Managed,
            healthcheck: Some(ExternalServiceHealthcheck {
                kind: ExternalServiceHealthcheckKind::Tcp,
                endpoint: "127.0.0.1:6380".to_string(),
            }),
            required_assets: Vec::new(),
        };

        let message = unavailable_service_message(&service, "127.0.0.1:6380");
        assert!(message.contains("Service is unavailable"));
        assert!(message.contains("service: cache"));
        assert!(message.contains("mode: managed"));
        assert!(message.contains("source: dependency:cache"));
        assert!(message.contains("Automatic startup is not available in this run path yet"));
        assert!(!message.contains("Ollama"));
    }

    fn sandbox_request(
        caller_cwd: PathBuf,
        effective_cwd: Option<PathBuf>,
        args: Vec<String>,
        read_grants: Vec<String>,
        write_grants: Vec<String>,
    ) -> ConsumerRunRequest {
        ConsumerRunRequest {
            target: caller_cwd.join("tool.py"),
            target_label: None,
            args,
            read_grants,
            write_grants,
            read_write_grants: Vec::new(),
            caller_cwd,
            effective_cwd,
            authoritative_input: None,
            desktop_open_path: None,
            background: false,
            nacelle: None,
            enforcement: "strict".to_string(),
            sandbox_mode: true,
            dangerously_skip_permissions: false,
            allow_unsafe: false,
            compatibility_fallback: None,
            provider_toolchain_requested: crate::ProviderToolchain::Auto,
            use_existing_toml: None,
            explicit_commit: None,
            assume_yes: true,
            verbose: false,
            agent_mode: crate::RunAgentMode::Off,
            agent_local_root: None,
            registry: None,
            keep_failed_artifacts: false,
            auto_fix_mode: None,
            allow_unverified: false,
            export_request: None,
            state_bindings: Vec::new(),
            managed_state_root: None,
            inject_bindings: Vec::new(),
            build_policy: crate::application::build_materialization::BuildPolicy::IfStale,
            cache_strategy: crate::application::dependency_materializer::CacheStrategy::None,
            reporter: Arc::new(CliReporter::new(false)),
            preview_mode: false,
            strict_realization: false,
            pinned_revision_output_dir: None,
            install_lifecycle_context: None,
            capsule_launch_inputs: Vec::new(),
        }
    }

    #[test]
    fn relative_grants_use_effective_cwd_for_host_and_guest_projection() {
        let caller = workspace_tempdir("caller-cwd-");
        let explicit = workspace_tempdir("effective-cwd-");
        let guest_manifest = workspace_tempdir("guest-manifest-");
        let input = explicit.path().join("in.pdf");
        std::fs::write(&input, b"pdf").expect("write input");

        let request = sandbox_request(
            caller.path().to_path_buf(),
            Some(explicit.path().to_path_buf()),
            vec!["./in.pdf".to_string()],
            vec!["./in.pdf".to_string()],
            Vec::new(),
        );

        let grants = resolve_sandbox_grants(&request, guest_manifest.path()).expect("grants");
        assert_eq!(grants.len(), 1);
        // Grant sources are recorded in `\\?\`-stripped canonical form.
        assert_eq!(
            grants[0].source_path,
            capsule::common::paths::windows_child_compatible_path(
                &input.canonicalize().expect("canonical input")
            )
        );
        assert_eq!(grants[0].guest_target, explicit.path().join("in.pdf"));
    }

    #[test]
    fn relative_write_grants_project_to_effective_cwd() {
        let caller = workspace_tempdir("caller-cwd-");
        let effective = workspace_tempdir("effective-cwd-");
        let guest_manifest = workspace_tempdir("guest-manifest-");

        let request = sandbox_request(
            caller.path().to_path_buf(),
            Some(effective.path().to_path_buf()),
            vec!["-o".to_string(), "./out.md".to_string()],
            Vec::new(),
            vec!["./out.md".to_string()],
        );

        let grants = resolve_sandbox_grants(&request, guest_manifest.path()).expect("grants");
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].source_path, effective.path().join("out.md"));
        assert_eq!(grants[0].guest_target, effective.path().join("out.md"));
    }

    #[test]
    fn best_effort_validation_uses_effective_cwd_for_relative_args() {
        let caller = workspace_tempdir("caller-cwd-");
        let effective = workspace_tempdir("effective-cwd-");
        let guest_manifest = workspace_tempdir("guest-manifest-");
        let input = effective.path().join("in.pdf");
        std::fs::write(&input, b"pdf").expect("write input");

        let request = sandbox_request(
            caller.path().to_path_buf(),
            Some(effective.path().to_path_buf()),
            vec!["./in.pdf".to_string()],
            vec!["./in.pdf".to_string()],
            Vec::new(),
        );

        let grants = resolve_sandbox_grants(&request, guest_manifest.path()).expect("grants");
        validate_sandbox_grants_best_effort(&request, &grants).expect("validation passes");
    }

    #[test]
    fn missing_grant_reports_effective_cwd() {
        let caller = workspace_tempdir("caller-cwd-");
        let effective = workspace_tempdir("effective-cwd-");
        let guest_manifest = workspace_tempdir("guest-manifest-");
        let input = effective.path().join("in.pdf");
        std::fs::write(&input, b"pdf").expect("write input");

        let request = sandbox_request(
            caller.path().to_path_buf(),
            Some(effective.path().to_path_buf()),
            vec!["./in.pdf".to_string()],
            Vec::new(),
            Vec::new(),
        );

        let grants = resolve_sandbox_grants(&request, guest_manifest.path()).expect("grants");
        let err = validate_sandbox_grants_best_effort(&request, &grants)
            .expect_err("missing read grant must fail");
        let message = err.to_string();
        assert!(message.contains("Missing read grant for ./in.pdf"));
        assert!(message.contains(&format!(
            "Resolved against effective cwd: {}",
            effective.path().display()
        )));
    }

    // ── reconcile_compat_manifest_targets tests ──────────────────────────────

    /// When capsule.toml declares `default_target = "main"` but the lock's
    /// selected target is `"default"`, the helper must add `[targets.default]`
    /// as an alias of `[targets.main]` so that build/runtime_tools lookups
    /// succeed through the lock-label path.
    #[test]
    fn reconcile_adds_alias_when_lock_and_manifest_target_differ() {
        let raw: toml::Value = toml::from_str(
            r#"
name = "demo"
schema_version = "0.3"
default_target = "main"

[targets.main]
runtime = "source"
driver = "python"
build = "npm install && npm run build"

[targets.main.runtime_tools]
node = "20"
"#,
        )
        .expect("parse");

        let reconciled = reconcile_compat_manifest_targets(&raw, "default");

        // [targets.default] must be present after aliasing
        let targets = reconciled
            .get("targets")
            .and_then(toml::Value::as_table)
            .expect("targets table");
        assert!(
            targets.contains_key("default"),
            "alias [targets.default] should have been inserted"
        );
        let default_target = targets
            .get("default")
            .and_then(toml::Value::as_table)
            .unwrap();
        assert_eq!(
            default_target.get("build").and_then(toml::Value::as_str),
            Some("npm install && npm run build")
        );
        let node = default_target
            .get("runtime_tools")
            .and_then(toml::Value::as_table)
            .and_then(|rt| rt.get("node"))
            .and_then(toml::Value::as_str);
        assert_eq!(node, Some("20"));
        // Original [targets.main] must still be present
        assert!(targets.contains_key("main"));
    }

    /// When capsule.toml `default_target` already matches the lock target, the
    /// helper must be a no-op (no spurious alias added).
    #[test]
    fn reconcile_is_noop_when_labels_match() {
        let raw: toml::Value = toml::from_str(
            r#"
name = "demo"
schema_version = "0.3"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
build = "pip install -r requirements.txt"
"#,
        )
        .expect("parse");

        let reconciled = reconcile_compat_manifest_targets(&raw, "default");

        let targets = reconciled
            .get("targets")
            .and_then(toml::Value::as_table)
            .expect("targets table");
        assert_eq!(targets.len(), 1, "no extra alias should be added");
        assert!(targets.contains_key("default"));
    }

    /// When capsule.toml has no explicit `default_target` but has a single
    /// named target (e.g. `[targets.app]`), the helper infers it as the
    /// capsule target and adds the alias.
    #[test]
    fn reconcile_infers_single_target_when_no_default_target_field() {
        let raw: toml::Value = toml::from_str(
            r#"
name = "demo"
schema_version = "0.3"

[targets.app]
runtime = "source"
driver = "node"
build = "npm run build"
"#,
        )
        .expect("parse");

        let reconciled = reconcile_compat_manifest_targets(&raw, "default");

        let targets = reconciled
            .get("targets")
            .and_then(toml::Value::as_table)
            .expect("targets table");
        assert!(
            targets.contains_key("default"),
            "alias [targets.default] should be inferred from the sole target"
        );
        let build = targets
            .get("default")
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("build"))
            .and_then(toml::Value::as_str);
        assert_eq!(build, Some("npm run build"));
    }

    /// After reconciliation, a `CompatManifestBridge` built from the aliased
    /// manifest correctly exposes the build command via `build_lifecycle_build`
    /// when the execution descriptor uses the lock target label.
    #[test]
    fn reconcile_enables_build_lifecycle_build_via_lock_target() {
        use capsule::router::{ExecutionProfile, execution_descriptor_from_manifest_parts};

        let tmp = workspace_tempdir("reconcile-build-test-");
        let manifest_dir = tmp.path().to_path_buf();

        // Raw capsule.toml: target is named "main", not "default"
        let raw: toml::Value = toml::from_str(
            r#"
name = "myapp"
version = "0.1.0"
type = "app"
schema_version = "0.3"
default_target = "main"

[targets.main]
runtime = "source"
driver = "python"
run_command = "uvicorn app:main"
build = "npm install && npm run build"

[targets.main.runtime_tools]
node = "20"
"#,
        )
        .expect("parse manifest");

        // Build a plan using the lock target label "default"
        let plan = execution_descriptor_from_manifest_parts(
            reconcile_compat_manifest_targets(&raw, "default"),
            manifest_dir.join("capsule.toml"),
            manifest_dir.clone(),
            ExecutionProfile::Dev,
            Some("default"),
            std::collections::HashMap::new(),
        )
        .expect("build plan");

        assert_eq!(
            plan.build_lifecycle_build().as_deref(),
            Some("npm install && npm run build"),
            "build command should be visible via the aliased target label"
        );
        assert_eq!(
            plan.execution_runtime_tool_version("node").as_deref(),
            Some("20"),
            "node runtime_tools should be visible via the aliased target label"
        );
    }

    fn managed_state_manifest(name: &str, version: &str) -> CapsuleManifest {
        CapsuleManifest::from_toml(&format!(
            r#"
schema_version = "0.3"
name = "{name}"
version = "{version}"
type = "app"
default_target = "app"
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#
        ))
        .expect("parse managed-state manifest")
    }

    #[test]
    fn path_segment_is_path_safe_and_collision_free() {
        // Stable for a given input.
        assert_eq!(super::path_segment("data"), super::path_segment("data"));
        // Path-safe: separators / dots are neutralized and cannot escape.
        for raw in ["a/b", "..", ".", "a/../b", "x\\y", "../../etc"] {
            let seg = super::path_segment(raw);
            assert!(
                !seg.contains('/') && !seg.contains('\\') && !seg.contains(".."),
                "segment must be path-safe: {raw:?} -> {seg}"
            );
        }
        // Collision-free: inputs that sanitize to the same string still differ.
        assert_ne!(super::path_segment("a/b"), super::path_segment("a_b"));
        assert_ne!(super::path_segment("."), super::path_segment("_"));
        // Readable prefix preserved; empty still yields a valid segment.
        assert!(super::path_segment("data").starts_with("data-"));
        assert!(!super::path_segment("").is_empty());
    }

    #[test]
    fn managed_state_dir_appends_only_target_and_state_key() {
        // Per the namespace contract, `root` already carries owner + capsule
        // identity; the resolver only appends target + state_key.
        let root = Path::new("/managed/owner-1/cap-rev-abc");

        let a = super::managed_state_dir(root, "app", "data");
        assert_eq!(
            a,
            super::managed_state_dir(root, "app", "data"),
            "same inputs -> same dir (reused across re-leases)"
        );
        assert!(a.starts_with(root));
        assert!(a.ends_with(super::path_segment("data")));

        // Distinct target / state_key each yield a distinct directory.
        assert_ne!(a, super::managed_state_dir(root, "worker", "data"));
        assert_ne!(a, super::managed_state_dir(root, "app", "cache"));

        // Collision-free even when sanitization would collapse inputs.
        assert_ne!(
            super::managed_state_dir(root, "app", "a/b"),
            super::managed_state_dir(root, "app", "a_b"),
        );
    }

    #[test]
    fn resolve_managed_auto_binds_unbound_persistent_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store =
            super::RegistryStore::open(&tmp.path().join("state-store")).expect("open store");
        let root = tmp.path().join("owner-123");
        let m = managed_state_manifest("demo-app", "0.1.0");

        let overrides = super::resolve_state_source_overrides_managed(
            &m,
            &[],
            Some(&store),
            Some(root.as_path()),
            Some("app"),
        )
        .expect("managed resolve");
        assert!(overrides.contains_key("data"), "data auto-bound");

        // The directory was created at the derived, stable location under root.
        let expected = super::managed_state_dir(root.as_path(), "app", "data");
        assert!(
            expected.exists(),
            "managed state dir created at derived path"
        );
        assert!(expected.starts_with(&root));

        // Stable across runs (re-lease reuse): identical returned binding.
        let again = super::resolve_state_source_overrides_managed(
            &m,
            &[],
            Some(&store),
            Some(root.as_path()),
            Some("app"),
        )
        .expect("managed resolve 2");
        assert_eq!(overrides.get("data"), again.get("data"));
    }

    #[test]
    fn resolve_managed_explicit_state_wins_over_managed_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store =
            super::RegistryStore::open(&tmp.path().join("state-store")).expect("open store");
        let root = tmp.path().join("owner-123");
        let explicit = tmp.path().join("explicit-data");
        std::fs::create_dir_all(&explicit).unwrap();
        let m = managed_state_manifest("demo-app", "0.1.0");

        let overrides = super::resolve_state_source_overrides_managed(
            &m,
            &[format!("data={}", explicit.display())],
            Some(&store),
            Some(root.as_path()),
            Some("app"),
        )
        .expect("managed resolve");
        assert!(overrides.contains_key("data"));

        // Explicit `--state` wins: the managed dir is never created.
        let managed_dir = super::managed_state_dir(root.as_path(), "app", "data");
        assert!(
            !managed_dir.exists(),
            "managed dir must not be created when --state binds the state"
        );
    }

    #[test]
    fn resolve_managed_without_root_still_requires_explicit_binding() {
        let m = managed_state_manifest("demo-app", "0.1.0");
        let err = super::resolve_state_source_overrides_managed(&m, &[], None, None, Some("app"))
            .expect_err("unbound persistent state without a managed root must fail");
        assert!(
            err.to_string().contains("requires an explicit"),
            "expected explicit-binding error, got: {err}"
        );
    }
}
