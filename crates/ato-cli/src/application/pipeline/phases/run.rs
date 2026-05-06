use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use capsule_core::ato_lock::AtoLock;
use capsule_core::dependency_contracts::{
    verify_and_lock, DependencyLock, DependencyLockInput, ResolvedProviderManifest,
};
use capsule_core::execution_identity::EnvOrigin;
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::guard::ExecutorKind;
use capsule_core::lockfile::{
    manifest_external_capsule_dependencies, verify_lockfile_external_dependencies, CapsuleLock,
    CAPSULE_LOCK_FILE_NAME,
};
use capsule_core::types::{CapsuleManifest, CapsuleType, StateDurability};
use capsule_core::CapsuleReporter;
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::application::dependency_credentials::{ProcessHostEnv, RedactionRegistry};
use crate::application::dependency_materializer::{
    digest_file, AttestationStrategy, CacheStrategy, DependencyMaterializationRequest,
    DependencyMaterializer, DependencyProjection, InstallPolicies, ManifestInputs, PlatformTriple,
    RuntimeSelection, SessionDependencyMaterializer,
};
use crate::application::dependency_runtime::orchestrator::{
    start_all as start_dependency_graph, OrchestratorError, OrchestratorInput,
    OrchestratorProvider, RunningGraph,
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
use capsule_core::router;

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
    pub(crate) validation_mode: capsule_core::types::ValidationMode,
    pub(crate) engine_override_declared: bool,
    pub(crate) compatibility_legacy_lock: Option<CompatibilityLegacyLockContext>,
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
    pub(crate) execution_plan: capsule_core::execution_plan::model::ExecutionPlan,
    pub(crate) tier: capsule_core::execution_plan::model::ExecutionTier,
    pub(crate) guard_result: capsule_core::execution_plan::guard::RuntimeGuardResult,
}

impl PreparedRunContext {
    pub(crate) fn from_authoritative_input(
        authoritative_input: Option<&RunAuthoritativeInput>,
        workspace_root: &Path,
        validation_mode: capsule_core::types::ValidationMode,
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
        })
    }

    pub(crate) fn with_bridge_manifest(
        &self,
        bridge_manifest: toml::Value,
        validation_mode: capsule_core::types::ValidationMode,
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
    pub(crate) compatibility_fallback: Option<String>,
    pub(crate) provider_toolchain_requested: ProviderToolchain,
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
    pub(crate) inject_bindings: Vec<String>,
    pub(crate) build_policy: crate::application::build_materialization::BuildPolicy,
    pub(crate) cache_strategy: CacheStrategy,
    pub(crate) reporter: Arc<CliReporter>,
    pub(crate) preview_mode: bool,
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

    match kind {
        InferredIoKind::Read => fs::canonicalize(&absolute).ok(),
        InferredIoKind::Write => {
            if absolute.exists() {
                fs::canonicalize(&absolute).ok()
            } else {
                let parent = absolute.parent()?;
                let file_name = absolute.file_name()?;
                let canonical_parent = fs::canonicalize(parent).ok()?;
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
        if matches!(current.as_str(), "-o" | "--output") {
            if let Some(next) = args.get(index + 1) {
                inferred.push((next.clone(), InferredIoKind::Write));
                index += 2;
                continue;
            }
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
    pub(crate) decision: capsule_core::router::RuntimeDecision,
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
}

#[derive(Debug)]
pub(crate) struct DependencyContractGuard {
    graph: Option<RunningGraph>,
    lock: DependencyLock,
}

impl DependencyContractGuard {
    fn new(graph: RunningGraph, lock: DependencyLock) -> Self {
        Self {
            graph: Some(graph),
            lock,
        }
    }

    fn graph(&self) -> Option<&RunningGraph> {
        self.graph.as_ref()
    }

    fn lock(&self) -> &DependencyLock {
        &self.lock
    }

    pub(crate) fn shutdown_now(&mut self) {
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
        if let Some(graph) = self.graph.take() {
            std::mem::forget(graph);
        }
    }
}

fn dependency_contract_start_error(target_label: &str, error: OrchestratorError) -> anyhow::Error {
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

fn persist_background_dependency_contracts(
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

async fn start_dependency_contracts_for_run(
    prepared: &PreparedRunContext,
    plan: &capsule_core::router::ManifestData,
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
        let loaded = capsule_core::manifest::load_manifest_with_validation_mode(
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
        ato_home: capsule_core::common::paths::nacelle_home_dir_or_workspace_tmp(),
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

fn locked_dependency_resolved_ref(
    locked: &capsule_core::lockfile::LockedCapsuleDependency,
) -> String {
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

fn inject_dependency_contract_env(
    mut launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
    plan: &capsule_core::router::ManifestData,
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

fn render_consumer_dependency_template(
    raw: &str,
    lock: &DependencyLock,
    graph: &RunningGraph,
) -> Result<(String, Option<EnvOrigin>)> {
    use capsule_core::types::{TemplateExpr, TemplateSegment, TemplatedString};

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

    let resolved_target = crate::install::support::resolve_run_target_or_install(
        request.target.clone(),
        request.assume_yes,
        request.provider_toolchain_requested,
        request.explicit_commit.clone(),
        request.keep_failed_artifacts,
        request.auto_fix_mode,
        request.allow_unverified,
        request.registry.as_deref(),
        request.reporter.clone(),
    )
    .await?;
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

fn run_validation_mode(preview_mode: bool) -> capsule_core::types::ValidationMode {
    if preview_mode {
        capsule_core::types::ValidationMode::Preview
    } else {
        capsule_core::types::ValidationMode::Strict
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
    let state_source_overrides =
        if let Some(authoritative_input) = request.authoritative_input.as_ref() {
            authoritative_input
                .effective_state
                .state_source_overrides
                .clone()
        } else {
            HashMap::new()
        };
    let mut decision = if let Some(authoritative_input) = request.authoritative_input.as_ref() {
        let mut decision = capsule_core::router::route_lock_with_state_overrides(
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
        if capsule_toml.exists() {
            if let Ok(loaded) = capsule_core::manifest::load_manifest_with_validation_mode(
                &capsule_toml,
                validation_mode,
            ) {
                if let Ok(bridge) =
                    capsule_core::router::CompatManifestBridge::from_manifest_value(&loaded.raw)
                {
                    decision.plan.compat_manifest = Some(bridge);
                }
                if let Ok(value) = toml::from_str::<toml::Value>(&loaded.raw_text) {
                    prepared.bridge_manifest = DerivedBridgeManifest::new(value);
                }
            }
        }
        decision
    } else {
        let loaded_manifest = capsule_core::manifest::load_manifest_with_validation_mode(
            &manifest_path,
            validation_mode,
        )?;
        prepared.bridge_manifest = DerivedBridgeManifest::new(
            toml::from_str(&loaded_manifest.raw_text)
                .unwrap_or_else(|_| loaded_manifest.raw.clone()),
        );
        prepared.engine_override_declared = loaded_manifest.raw.get("engine").is_some();
        let manifest = loaded_manifest.model.clone();
        if manifest.schema_version.trim() == "0.3" && manifest.capsule_type == CapsuleType::Library
        {
            anyhow::bail!(
                "schema_version=0.3 type=library package cannot be started with `ato run`"
            );
        }
        let state_source_overrides =
            resolve_state_source_overrides(&manifest, &request.state_bindings)?;
        capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
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
        verify_lockfile_external_dependencies(
            &decision.plan.manifest,
            &compatibility_legacy_lock.lock,
        )?;
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
    let mut dep_contracts = None;
    if has_dependency_contracts {
        // Auto-lock: if the consumer was fetched without a pre-existing
        // capsule.lock.json (typical for `ato run github.com/...`), the
        // dep-contract path needs derived lock entries. Generate them on
        // the fly here so the user does not have to run `ato lock` by
        // hand. The canonical `ato.lock.json` is left untouched — this
        // path only produces the compat-legacy derived lock that
        // verify_lockfile_external_dependencies + start_dependency_contracts_for_run
        // consume. RFC §13 Open Question "Local override" is a
        // follow-on; this auto-lock is the minimum needed for github
        // capsule provider sources.
        let compatibility_legacy_lock = match prepared.compatibility_legacy_lock.as_ref() {
            Some(ctx) => ctx.clone(),
            None => {
                let bridge = capsule_core::router::CompatManifestBridge::from_manifest_value(
                    prepared.bridge_manifest.as_toml(),
                )
                .context("failed to build compatibility bridge for auto-lock")?;
                let compat_input = capsule_core::router::CompatProjectInput::from_bridge(
                    prepared.workspace_root.clone(),
                    bridge,
                )
                .context("failed to build CompatProjectInput for auto-lock")?;
                let lock_path = capsule_core::contract::lockfile::ensure_lockfile_for_compat_input(
                    &compat_input,
                    request.reporter.clone(),
                    false,
                )
                .await
                .context("auto-lock for dependency contracts failed")?;
                let bytes = std::fs::read(&lock_path).with_context(|| {
                    format!("failed to read auto-generated lock {}", lock_path.display())
                })?;
                let lock: capsule_core::lockfile::CapsuleLock = serde_json::from_slice(&bytes)
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
        verify_lockfile_external_dependencies(
            &decision.plan.manifest,
            &compatibility_legacy_lock.lock,
        )?;
        let guard = start_dependency_contracts_for_run(
            &prepared,
            &decision.plan,
            &compatibility_legacy_lock.lock,
        )
        .await?;
        // Surface a secret-free summary of started deps before the
        // consumer launches. Helps `ato run` users (especially via
        // ato-desktop) see which provider was fetched, where its
        // process is logging, and which port the orchestrator
        // allocated. runtime_exports values are intentionally omitted
        // — only export key names are listed — because values may
        // contain credentials that the redaction filter is also
        // scrubbing from logs.
        if let Some(graph) = guard.graph() {
            register_dependency_contract_cleanup(attempt.as_deref_mut(), graph);
            for line in graph.summary_lines() {
                eprintln!("{line}");
            }
        }
        dep_contracts = Some(guard);
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
    if let Some(external_capsules) = external_capsules.as_ref() {
        for (dependency, env) in external_capsules.caller_envs() {
            launch_ctx = launch_ctx.with_injected_env_with_origin(
                env.clone(),
                EnvOrigin::DepRuntimeExport(dependency),
            );
        }
    }
    if let Some(dep_contracts) = dep_contracts.as_ref() {
        let graph = dep_contracts
            .graph()
            .context("dependency contract graph missing after startup")?;
        launch_ctx = inject_dependency_contract_env(
            launch_ctx,
            &decision.plan,
            dep_contracts.lock(),
            graph,
        )?;
        // Surface the providers' loopback endpoints to the consumer's
        // sandbox so `--sandbox` consumers can `connect(127.0.0.1:<port>)`
        // without tripping the egress-deny default. The orchestrator owns
        // port allocation; without this the consumer's psycopg/sqlalchemy
        // stack hits EPERM mid-startup even though the provider is happily
        // listening on the same loopback (#17).
        let dep_endpoints: Vec<String> = graph
            .deps()
            .iter()
            .filter_map(|dep| dep.allocated_port.map(|port| format!("127.0.0.1:{port}")))
            .collect();
        if !dep_endpoints.is_empty() {
            launch_ctx = launch_ctx.with_dep_endpoints(dep_endpoints);
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
    if use_progressive_ui {
        if let Some(audit_reporter) =
            provisioner::AuditReporter::from_outcome(&provisioning_outcome)
        {
            let body = audit_reporter.body();
            if !body.is_empty() {
                crate::progressive_ui::show_note(audit_reporter.title(), body)?;
            }
        }
    }
    launch_ctx = launch_ctx
        .with_injected_env(provisioning_outcome.additional_env)
        .with_injected_mounts(provisioning_outcome.additional_mounts);

    if let Some(shadow_workspace) = provisioning_outcome.shadow_workspace.as_ref() {
        if let Some(attempt) = attempt.as_deref_mut() {
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
            if capsule_toml.exists() {
                if let Ok(loaded) = capsule_core::manifest::load_manifest_with_validation_mode(
                    &capsule_toml,
                    validation_mode,
                ) {
                    if let Ok(bridge) =
                        capsule_core::router::CompatManifestBridge::from_manifest_value(&loaded.raw)
                    {
                        decision.plan.compat_manifest = Some(bridge);
                    }
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
                    if source_ref.eq_ignore_ascii_case("dependency:ollama") {
                        if let Some(model) = legacy_ollama_model.clone() {
                            required_assets.push(ServiceRequiredAsset::OllamaModel { model });
                        }
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
    use crate::application::build_materialization as bm;

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

    match prepared.decision.action {
        bm::DecisionAction::Skip => {
            maybe_apply_dependency_materialization(request, &mut state).await?;
            progress.ok(
                HourglassPhase::Build,
                "build materialization reused — executor skipped",
            );
            return Ok(state);
        }
        bm::DecisionAction::Fail => {
            return Err(bm::no_build_error(&prepared.decision));
        }
        bm::DecisionAction::Execute => {}
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
        bm::persist_after_execute(
            &state.decision.plan,
            &workspace_root,
            observation,
            request.reporter.is_json(),
        );
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

    if matches!(state.decision.kind, capsule_core::router::RuntimeKind::Oci) {
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

    if state.use_progressive_ui {
        if let Some(preview_session) = state.preview_session.as_ref() {
            crate::progressive_ui::render_preview_plan(preview_session)?;
            crate::progressive_ui::render_promotion_summary(
                &preview_session.derived_plan.promotion_eligibility,
            )?;
        }
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

    if matches!(state.decision.kind, capsule_core::router::RuntimeKind::Oci) {
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
    if matches!(guard_result.executor_kind, ExecutorKind::Native)
        && !request.dangerously_skip_permissions
        && !host_fallback_requested
    {
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

#[allow(clippy::too_many_arguments)]
#[async_trait(?Send)]
pub(crate) trait ConsumerRunExecuteHooks {
    fn preflight_native_sandbox(
        &self,
        nacelle_override: Option<PathBuf>,
        plan: &capsule_core::router::ManifestData,
        prepared: &PreparedRunContext,
        effective_cwd: Option<&Path>,
        reporter: &Arc<CliReporter>,
    ) -> Result<PathBuf>;

    async fn complete_background_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        plan: &capsule_core::router::ManifestData,
        runtime: String,
        scoped_id: Option<String>,
        is_one_shot: bool,
        ready_without_events: bool,
        desktop_open_only: bool,
        compatibility_host_mode: CompatibilityHostMode,
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
    ) -> Result<i32>;

    async fn cleanup_existing_scoped_processes_before_run(
        &self,
        scoped_id: &str,
        reporter: &Arc<CliReporter>,
    ) -> Result<()>;

    async fn notify_web_endpoint(
        &self,
        plan: &capsule_core::router::ManifestData,
        reporter: &Arc<CliReporter>,
    ) -> Result<()>;

    fn process_runtime_label(
        &self,
        plan: &capsule_core::router::ManifestData,
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
    } = state;

    if decision.plan.is_orchestration_mode() {
        if request.background {
            anyhow::bail!("--background is not supported for orchestration mode");
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

    if matches!(decision.kind, capsule_core::router::RuntimeKind::Oci) {
        if request.background {
            anyhow::bail!("--background is not supported for runtime=oci");
        }

        target_runner::preflight_required_environment_variables(&decision.plan, &launch_ctx)?;
        let exit =
            crate::executors::oci::execute(&decision.plan, request.reporter.clone(), &launch_ctx)
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
    let mode = if request.background {
        ExecuteMode::Background
    } else {
        ExecuteMode::Foreground
    };

    let run_scoped_id = runtime_overrides::scoped_id_override();

    // Auto-assign a unique port if none specified via manifest or override
    if runtime_overrides::override_port(decision.plan.execution_port()).is_none() {
        let identity = build_port_identity(
            &decision.plan.manifest_path,
            decision.plan.selected_target_label(),
            run_scoped_id.as_deref(),
        );
        if let Ok(mgr) = crate::runtime::port_manager::PortManager::new() {
            if let Ok(port) = mgr.resolve_port(&identity) {
                std::env::set_var("ATO_UI_OVERRIDE_PORT", port.to_string());
            }
        }
    }

    if request.background {
        if let Some(scoped_id) = run_scoped_id.as_deref() {
            hooks
                .cleanup_existing_scoped_processes_before_run(scoped_id, &request.reporter)
                .await?;
        }
    }

    if execution_plan.target.runtime == capsule_core::execution_plan::model::ExecutionRuntime::Web {
        hooks
            .notify_web_endpoint(&decision.plan, &request.reporter)
            .await?;
    }

    let execution_receipt_document =
        crate::application::execution_receipt_builder::build_prelaunch_receipt_document(
            &decision.plan,
            &execution_plan,
            &launch_ctx,
            build_observation.as_ref(),
        )?;
    let execution_receipt_path =
        crate::application::execution_receipts::write_receipt_document_atomic(
            &execution_receipt_document,
        )?;
    if request.verbose {
        let (execution_id, schema_label) = match &execution_receipt_document {
            capsule_core::execution_identity::ExecutionReceiptDocument::V1(receipt) => {
                (receipt.execution_id.clone(), "v1")
            }
            capsule_core::execution_identity::ExecutionReceiptDocument::V2(receipt) => {
                (receipt.execution_id.clone(), "v2-experimental")
            }
        };
        request
            .reporter
            .notify(format!(
                "🧾 Execution receipt ({}): {} ({})",
                schema_label,
                execution_id,
                execution_receipt_path.display()
            ))
            .await?;
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
                manifest_path: Some(decision.plan.manifest_path.clone()),
                scoped_id: run_scoped_id.clone(),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                requested_port: runtime_overrides::override_port(decision.plan.execution_port()),
                log_path: None,
                ready_at: Some(now),
                last_event: Some("spawned".to_string()),
                last_error: None,
                exit_code: None,
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

    let consent_already_granted = crate::consent_store::has_consent(&execution_plan)?;
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
                    capsule_core::AtoError::ExecutionContractInvalid {
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

    match guard_result.executor_kind {
        ExecutorKind::Native => {
            let host_execution = request.dangerously_skip_permissions
                || host_fallback_requested
                || desktop_native_open_only;
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
                let runtime = hooks.process_runtime_label(
                    &decision.plan,
                    request.dangerously_skip_permissions || desktop_native_open_only,
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
            let exit = crate::executors::node_compat::execute(
                &decision.plan,
                prepared.authoritative_lock.as_ref(),
                &execution_plan,
                &launch_ctx,
                request.dangerously_skip_permissions,
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

pub(crate) async fn reroute_auto_provisioned_execution(
    decision: capsule_core::router::RuntimeDecision,
    launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    preview_mode: bool,
    shadow_manifest_path: &Path,
) -> Result<(
    capsule_core::router::RuntimeDecision,
    crate::executors::launch_context::RuntimeLaunchContext,
    PreparedRunContext,
)> {
    let validation_mode = run_validation_mode(preview_mode);
    let loaded_manifest = capsule_core::manifest::load_manifest_with_validation_mode(
        shadow_manifest_path,
        validation_mode,
    )?;
    let rerouted_decision =
        capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
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
    decision: &capsule_core::router::RuntimeDecision,
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
        capsule_core::router::RuntimeDecision,
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

pub(crate) fn resolve_state_source_overrides_with_store(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
    store: Option<&RegistryStore>,
) -> Result<HashMap<String, String>> {
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

    resolve_state_source_overrides_from_requested(manifest, &requested, store)
}

fn resolve_state_source_overrides_from_requested(
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

    let mut resolved = HashMap::new();

    for (state_name, _) in persistent_states {
        let locator = requested.get(state_name.as_str()).ok_or_else(|| {
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

pub(crate) fn resolve_compatibility_host_mode(
    executor_kind: ExecutorKind,
    compatibility_fallback: Option<&str>,
) -> Result<CompatibilityHostMode> {
    match compatibility_fallback {
        None => Ok(CompatibilityHostMode::Disabled),
        Some("host") if matches!(executor_kind, ExecutorKind::Native | ExecutorKind::NodeCompat) => {
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
    plan: &capsule_core::router::ManifestData,
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

#[cfg(test)]
mod tests {
    use super::{
        normalize_existing_path, normalize_write_path, parent_package_id,
        parse_external_service_contracts, parse_reuse_if_present_service_preflights,
        resolve_sandbox_grants, unavailable_service_message, validate_sandbox_grants_best_effort,
        ConsumerRunRequest, DerivedBridgeManifest, ExternalServiceContract,
        ExternalServiceHealthcheck, ExternalServiceHealthcheckKind, ExternalServiceMode,
        PreparedRunContext, ServiceRequiredAsset,
    };
    use capsule_core::ato_lock::AtoLock;
    use capsule_core::types::{CapsuleManifest, ParamValue};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::reporters::CliReporter;

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

    #[test]
    fn locked_dependency_resolved_ref_prefers_content_digest() {
        let locked = capsule_core::lockfile::LockedCapsuleDependency {
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
            validation_mode: capsule_core::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
        };

        let rerouted = prepared.with_bridge_manifest(
            toml::Value::String("new".to_string()),
            capsule_core::types::ValidationMode::Preview,
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
            capsule_core::types::ValidationMode::Preview
        );
        assert!(rerouted.engine_override_declared);
    }

    #[test]
    fn existing_grant_rejects_symlink_traversal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside_dir = tempfile::tempdir().expect("outside tempdir");
        let link_path = temp.path().join("outside-link");

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside_dir.path(), &link_path).expect("create symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(outside_dir.path(), &link_path).expect("create symlink");

        let err = normalize_existing_path(&link_path).expect_err("must reject symlink grants");
        assert!(err.to_string().contains("traverses symlink"));
    }

    #[test]
    fn write_grant_rejects_missing_file_under_symlink_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside_dir = tempfile::tempdir().expect("outside tempdir");
        let link_path = temp.path().join("outside-link");

        #[cfg(unix)]
        std::os::unix::fs::symlink(outside_dir.path(), &link_path).expect("create symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(outside_dir.path(), &link_path).expect("create symlink");

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
            compatibility_fallback: None,
            provider_toolchain_requested: crate::ProviderToolchain::Auto,
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
            inject_bindings: Vec::new(),
            build_policy: crate::application::build_materialization::BuildPolicy::IfStale,
            cache_strategy: crate::application::dependency_materializer::CacheStrategy::None,
            reporter: Arc::new(CliReporter::new(false)),
            preview_mode: false,
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
        assert_eq!(
            grants[0].source_path,
            input.canonicalize().expect("canonical input")
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
}
