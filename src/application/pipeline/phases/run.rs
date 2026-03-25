use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use async_trait::async_trait;
use capsule_core::ato_lock::AtoLock;
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::guard::ExecutorKind;
use capsule_core::lockfile::{
    manifest_external_capsule_dependencies, verify_lockfile_external_dependencies, CapsuleLock,
    CAPSULE_LOCK_FILE_NAME,
};
use capsule_core::manifest::LoadedManifest;
use capsule_core::types::{CapsuleManifest, CapsuleType, StateDurability};
use capsule_core::CapsuleReporter;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::application::engine::install::support::{
    LocalRunManifestPreparationOutcome, ResolvedRunTarget,
};
use crate::application::pipeline::cleanup::PipelineAttemptContext;
use crate::application::workspace::state::EffectiveLockState;
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

use crate::RunAgentMode;

use crate::application::pipeline::hourglass::HourglassPhase;

pub(crate) trait ConsumerRunProgress {
    fn start(&self, phase: HourglassPhase);
    fn ok(&self, phase: HourglassPhase, detail: &str);
    fn skip(&self, phase: HourglassPhase, detail: &str);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunAuthoritativeInputKind {
    CanonicalLock,
    CompatibilityCompiledDraft,
    SourceOnly,
}

#[derive(Debug, Clone)]
pub(crate) struct CompatibilityLegacyLockContext {
    pub(crate) manifest_path: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) lock: CapsuleLock,
}

#[derive(Debug, Clone)]
pub(crate) struct RunAuthoritativeInput {
    pub(crate) kind: RunAuthoritativeInputKind,
    pub(crate) project_root: PathBuf,
    pub(crate) lock: AtoLock,
    pub(crate) lock_path: PathBuf,
    pub(crate) sidecar_path: PathBuf,
    pub(crate) bridge_manifest_path: PathBuf,
    pub(crate) bridge_manifest_sha256: String,
    pub(crate) effective_state: EffectiveLockState,
    pub(crate) compatibility_legacy_lock: Option<CompatibilityLegacyLockContext>,
}

// PreparedRunContext carries the already-fixed bridge artifact and compatibility-scoped
// validation context. Downstream phases may consume this data, but must not reinterpret
// manifest semantics or discover new authority from disk.
#[derive(Debug, Clone)]
pub(crate) struct PreparedRunContext {
    pub(crate) authoritative_lock: Option<AtoLock>,
    pub(crate) effective_state: Option<EffectiveLockState>,
    pub(crate) raw_manifest: toml::Value,
    pub(crate) validation_mode: capsule_core::types::ValidationMode,
    pub(crate) engine_override_declared: bool,
    pub(crate) compatibility_legacy_lock: Option<CompatibilityLegacyLockContext>,
}

#[derive(Clone)]
pub(crate) struct ConsumerRunRequest {
    pub(crate) target: PathBuf,
    pub(crate) target_label: Option<String>,
    pub(crate) authoritative_input: Option<RunAuthoritativeInput>,
    pub(crate) background: bool,
    pub(crate) nacelle: Option<PathBuf>,
    pub(crate) enforcement: String,
    pub(crate) sandbox_mode: bool,
    pub(crate) dangerously_skip_permissions: bool,
    pub(crate) compatibility_fallback: Option<String>,
    pub(crate) assume_yes: bool,
    pub(crate) agent_mode: RunAgentMode,
    pub(crate) agent_local_root: Option<PathBuf>,
    pub(crate) registry: Option<String>,
    pub(crate) keep_failed_artifacts: bool,
    pub(crate) auto_fix_mode: Option<crate::GitHubAutoFixMode>,
    pub(crate) allow_unverified: bool,
    pub(crate) state_bindings: Vec<String>,
    pub(crate) inject_bindings: Vec<String>,
    pub(crate) reporter: Arc<CliReporter>,
    pub(crate) preview_mode: bool,
}

pub(crate) struct RunInstallPhaseResult {
    pub(crate) resolved_target: ResolvedRunTarget,
    pub(crate) manifest_outcome: LocalRunManifestPreparationOutcome,
}

pub(crate) struct RunPipelineState {
    pub(crate) preview_session: Option<preview::PreviewSession>,
    pub(crate) preview_mode: bool,
    pub(crate) use_progressive_ui: bool,
    pub(crate) prepared: PreparedRunContext,
    pub(crate) decision: capsule_core::router::RuntimeDecision,
    pub(crate) launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
    pub(crate) external_capsules: Option<crate::external_capsule::ExternalCapsuleGuard>,
    pub(crate) agent_attempted: bool,
    pub(crate) execution_plan: Option<capsule_core::execution_plan::model::ExecutionPlan>,
    pub(crate) tier: Option<capsule_core::execution_plan::model::ExecutionTier>,
    pub(crate) guard_result: Option<capsule_core::execution_plan::guard::RuntimeGuardResult>,
    pub(crate) compatibility_host_mode: Option<CompatibilityHostMode>,
    pub(crate) native_nacelle: Option<PathBuf>,
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

    let detail = match manifest_outcome {
        LocalRunManifestPreparationOutcome::Ready => "target resolved and manifest ready",
        LocalRunManifestPreparationOutcome::CreatedManualManifest => {
            "manifest created; stopping before prepare"
        }
    };
    progress.ok(HourglassPhase::Install, detail);

    Ok(RunInstallPhaseResult {
        resolved_target,
        manifest_outcome,
    })
}

fn run_validation_mode(preview_mode: bool) -> capsule_core::types::ValidationMode {
    if preview_mode {
        capsule_core::types::ValidationMode::Preview
    } else {
        capsule_core::types::ValidationMode::Strict
    }
}

fn prepare_run_context(
    authoritative_input: Option<&RunAuthoritativeInput>,
    loaded_manifest: &LoadedManifest,
    validation_mode: capsule_core::types::ValidationMode,
) -> PreparedRunContext {
    let raw_manifest =
        toml::from_str(&loaded_manifest.raw_text).unwrap_or_else(|_| loaded_manifest.raw.clone());

    PreparedRunContext {
        authoritative_lock: authoritative_input.map(|input| input.lock.clone()),
        effective_state: authoritative_input.map(|input| input.effective_state.clone()),
        raw_manifest,
        validation_mode,
        engine_override_declared: loaded_manifest.raw.get("engine").is_some(),
        compatibility_legacy_lock: authoritative_input
            .and_then(|input| input.compatibility_legacy_lock.clone()),
    }
}

fn validate_authoritative_bridge(
    authoritative_input: Option<&RunAuthoritativeInput>,
    manifest_path: &Path,
    loaded_manifest: &LoadedManifest,
) -> Result<()> {
    let Some(authoritative_input) = authoritative_input else {
        return Ok(());
    };

    if manifest_path != authoritative_input.bridge_manifest_path {
        return Ok(());
    }

    let actual_sha256 = sha256_hex(loaded_manifest.raw_text.as_bytes());
    if actual_sha256 == authoritative_input.bridge_manifest_sha256 {
        return Ok(());
    }

    anyhow::bail!(AtoExecutionError::execution_contract_invalid(
        "generated manifest bridge no longer matches the authoritative lock-derived run request",
        Some("bridge_manifest"),
        None,
    ));
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) async fn run_prepare_phase<P>(
    request: &ConsumerRunRequest,
    progress: &P,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<RunPipelineState>
where
    P: ConsumerRunProgress,
{
    progress.start(HourglassPhase::Prepare);

    let manifest_path = if request.target.is_dir() {
        request.target.join("capsule.toml")
    } else {
        request.target.clone()
    };
    let preview_session = preview::load_preview_session_for_manifest(&manifest_path)?;
    let preview_mode = request.preview_mode || preview_session.is_some();
    let use_progressive_ui =
        crate::progressive_ui::can_use_progressive_ui(false) && !request.background;
    let source_label = preview_session
        .as_ref()
        .map(|session| session.target_reference.clone())
        .unwrap_or_else(|| manifest_path.display().to_string());

    if use_progressive_ui {
        crate::progressive_ui::show_run_intro(&source_label)?;
    }

    let validation_mode = run_validation_mode(preview_mode);
    let loaded_manifest = capsule_core::manifest::load_manifest_with_validation_mode(
        &manifest_path,
        validation_mode,
    )?;
    validate_authoritative_bridge(
        request.authoritative_input.as_ref(),
        &manifest_path,
        &loaded_manifest,
    )?;
    let mut prepared = prepare_run_context(
        request.authoritative_input.as_ref(),
        &loaded_manifest,
        validation_mode,
    );
    let manifest = loaded_manifest.model.clone();
    if manifest.schema_version.trim() == "0.3" && manifest.capsule_type == CapsuleType::Library {
        anyhow::bail!("schema_version=0.3 type=library package cannot be started with `ato run`");
    }

    let state_source_overrides =
        if let Some(authoritative_input) = request.authoritative_input.as_ref() {
            resolve_state_source_overrides_from_map(
                &manifest,
                &authoritative_input.effective_state.state_source_overrides,
            )?
        } else {
            resolve_state_source_overrides(&manifest, &request.state_bindings)?
        };
    let mut decision =
        capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
            &manifest_path,
            router::ExecutionProfile::Dev,
            request.target_label.as_deref(),
            state_source_overrides,
            validation_mode,
        )?;
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

    let external_dependencies = manifest_external_capsule_dependencies(&decision.plan.manifest)?;
    let mut external_capsules = None;
    if !external_dependencies.is_empty() {
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

    let injected_data =
        crate::data_injection::resolve_and_record(&decision.plan, &request.inject_bindings).await?;
    let mut merged_injected_env = injected_data.env;
    if let Some(external_capsules) = external_capsules.as_ref() {
        merged_injected_env.extend(external_capsules.caller_env().clone());
    }
    let mut launch_ctx =
        target_runner::resolve_launch_context(&decision.plan, &prepared, &request.reporter)
            .await?
            .with_injected_env(merged_injected_env)
            .with_injected_mounts(injected_data.mounts);
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
        if let Some(attempt) = attempt {
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
            (decision, launch_ctx, prepared) = reroute_auto_provisioned_execution(
                decision,
                launch_ctx,
                request.reporter.clone(),
                preview_mode,
                shadow_manifest_path,
            )
            .await?;
        }
    }

    if let Some((rerouted_decision, rerouted_launch_ctx, rerouted_prepared)) =
        maybe_run_agent_setup(
            request,
            &decision,
            &launch_ctx,
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
        agent_attempted,
        execution_plan: None,
        tier: None,
        guard_result: None,
        compatibility_host_mode: None,
        native_nacelle: None,
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
        crate::commands::run::run_v03_lifecycle_steps(
            &state.decision.plan,
            &request.reporter,
            &state.launch_ctx,
        )
        .await?;
    }

    progress.ok(HourglassPhase::Build, "build and lifecycle hooks completed");

    Ok(state)
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

    state.execution_plan = Some(prepared.execution_plan);
    state.decision = prepared.runtime_decision;
    state.tier = Some(prepared.tier);
    state.guard_result = Some(prepared.guard_result);
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
        .guard_result
        .as_ref()
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
        reporter: &Arc<CliReporter>,
    ) -> Result<PathBuf>;

    async fn complete_background_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        plan: &capsule_core::router::ManifestData,
        runtime: String,
        scoped_id: Option<String>,
        ready_without_events: bool,
        compatibility_host_mode: CompatibilityHostMode,
        reporter: &Arc<CliReporter>,
    ) -> Result<()>;

    async fn complete_foreground_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        reporter: Arc<CliReporter>,
        sandbox_initialized: bool,
        ipc_socket_mapped: bool,
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
        agent_attempted: _,
        execution_plan,
        tier,
        guard_result,
        compatibility_host_mode,
        native_nacelle,
    } = state;

    if decision.plan.is_orchestration_mode() {
        if request.background {
            anyhow::bail!("--background is not supported for orchestration mode");
        }

        let exit = crate::executors::orchestrator::execute(
            &decision.plan,
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
            std::process::exit(exit);
        }

        progress.ok(HourglassPhase::Execute, "oci runtime completed");
        return Ok(());
    }

    let execution_plan =
        execution_plan.context("run pipeline execute phase requires a prepared execution plan")?;
    let guard_result = guard_result
        .context("run pipeline execute phase requires a verified execution guard result")?;
    let compatibility_host_mode = compatibility_host_mode
        .context("run pipeline execute phase requires compatibility host mode")?;

    debug!(
        runtime = execution_plan.target.runtime.as_str(),
        driver = execution_plan.target.driver.as_str(),
        ?tier,
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

    let run_command_uses_specialized_executor = decision
        .plan
        .execution_driver()
        .map(|driver| {
            matches!(
                driver.trim().to_ascii_lowercase().as_str(),
                "deno" | "node" | "python"
            )
        })
        .unwrap_or(false);

    if decision.plan.execution_run_command().is_some() && !run_command_uses_specialized_executor {
        let mut process = crate::executors::shell::execute(&decision.plan, mode, &launch_ctx)?;
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
                requested_port: None,
                log_path: None,
                ready_at: Some(now),
                last_event: Some("spawned".to_string()),
                last_error: None,
                exit_code: None,
            };

            let process_manager = crate::runtime::process::ProcessManager::new()?;
            process_manager.write_pid(&info)?;
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
            std::process::exit(exit_code);
        }

        progress.ok(HourglassPhase::Execute, "shell execution completed");
        return Ok(());
    }

    let host_fallback_requested = matches!(compatibility_host_mode, CompatibilityHostMode::Enabled);
    if use_progressive_ui {
        if host_fallback_requested {
            crate::progressive_ui::render_host_fallback_warning()?;
        } else {
            crate::progressive_ui::render_security_context(
                guard_result.executor_kind,
                host_fallback_requested,
                request.dangerously_skip_permissions,
                runtime_overrides::override_port(decision.plan.execution_port()),
            )?;
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
            let host_execution = request.dangerously_skip_permissions || host_fallback_requested;
            let process = if host_execution {
                crate::executors::source::execute_host(
                    &decision.plan,
                    request.reporter.clone(),
                    mode,
                    &launch_ctx,
                )?
            } else {
                let nacelle = match native_nacelle {
                    Some(path) => path,
                    None => hooks.preflight_native_sandbox(
                        request.nacelle.clone(),
                        &decision.plan,
                        &prepared,
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

            if request.background {
                let runtime = hooks.process_runtime_label(
                    &decision.plan,
                    request.dangerously_skip_permissions,
                    compatibility_host_mode,
                );
                let ready_without_events = host_execution && process.event_rx.is_none();
                hooks
                    .complete_background_source_process(
                        process,
                        &decision.plan,
                        runtime,
                        run_scoped_id.clone(),
                        ready_without_events,
                        compatibility_host_mode,
                        &request.reporter,
                    )
                    .await?;
                sidecar_cleanup.stop_now();
                progress.ok(
                    HourglassPhase::Execute,
                    "background native execution started",
                );
                return Ok(());
            }

            let exit_code = hooks
                .complete_foreground_source_process(
                    process,
                    request.reporter.clone(),
                    !host_execution,
                    launch_ctx
                        .socket_paths()
                        .map(|paths| !paths.is_empty())
                        .unwrap_or(false),
                    use_progressive_ui,
                )
                .await?;
            sidecar_cleanup.stop_now();

            if exit_code != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                std::process::exit(exit_code);
            }
        }
        ExecutorKind::NodeCompat if host_fallback_requested => {
            let process = crate::executors::source::execute_host(
                &decision.plan,
                request.reporter.clone(),
                mode,
                &launch_ctx,
            )?;

            if request.background {
                let runtime =
                    hooks.process_runtime_label(&decision.plan, false, compatibility_host_mode);
                let ready_without_events = process.event_rx.is_none();
                hooks
                    .complete_background_source_process(
                        process,
                        &decision.plan,
                        runtime,
                        run_scoped_id.clone(),
                        ready_without_events,
                        compatibility_host_mode,
                        &request.reporter,
                    )
                    .await?;
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
                    false,
                    launch_ctx
                        .socket_paths()
                        .map(|paths| !paths.is_empty())
                        .unwrap_or(false),
                    use_progressive_ui,
                )
                .await?;
            sidecar_cleanup.stop_now();

            if exit_code != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
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
                    requested_port: None,
                    log_path: None,
                    ready_at: Some(now),
                    last_event: Some("spawned".to_string()),
                    last_error: None,
                    exit_code: None,
                };

                let process_manager = crate::runtime::process::ProcessManager::new()?;
                process_manager.write_pid(&info)?;

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
                std::process::exit(exit);
            }
        }
        ExecutorKind::NodeCompat => {
            let exit = crate::executors::node_compat::execute(
                &decision.plan,
                &execution_plan,
                &launch_ctx,
                request.dangerously_skip_permissions,
            )?;
            sidecar_cleanup.stop_now();
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                std::process::exit(exit);
            }
        }
    }

    progress.ok(HourglassPhase::Execute, "capsule execution completed");

    Ok(())
}

pub(crate) async fn reroute_auto_provisioned_execution(
    decision: capsule_core::router::RuntimeDecision,
    launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
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
    let rerouted_prepared = PreparedRunContext {
        authoritative_lock: None,
        effective_state: None,
        raw_manifest: toml::from_str(&loaded_manifest.raw_text)
            .unwrap_or_else(|_| loaded_manifest.raw.clone()),
        validation_mode,
        engine_override_declared,
        compatibility_legacy_lock: None,
    };
    let rerouted_launch_ctx = target_runner::resolve_launch_context(
        &rerouted_decision.plan,
        &rerouted_prepared,
        &reporter,
    )
    .await?
    .with_injected_env(launch_ctx.merged_env())
    .with_injected_mounts(launch_ctx.injected_mounts().to_vec());
    Ok((rerouted_decision, rerouted_launch_ctx, rerouted_prepared))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn maybe_run_agent_setup(
    request: &ConsumerRunRequest,
    decision: &capsule_core::router::RuntimeDecision,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
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
        request.reporter.clone(),
        preview_mode,
        &outcome.shadow_manifest_path,
    )
    .await?;
    Ok(Some(rerouted))
}

pub(crate) fn resolve_state_source_overrides(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
) -> Result<HashMap<String, String>> {
    resolve_state_source_overrides_with_store(manifest, raw_bindings, None)
}

pub(crate) fn resolve_state_source_overrides_from_map(
    manifest: &CapsuleManifest,
    requested: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    resolve_state_source_overrides_from_requested(manifest, requested, None)
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

#[cfg(test)]
mod tests {
    use super::{validate_authoritative_bridge, RunAuthoritativeInput, RunAuthoritativeInputKind};
    use capsule_core::ato_lock::AtoLock;
    use tempfile::tempdir;

    #[test]
    fn generated_bridge_hash_mismatch_fails_closed() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join(".ato.run.generated.capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
entrypoint = "main.ts"
"#,
        )
        .expect("write manifest");
        let loaded_manifest = capsule_core::manifest::load_manifest_with_validation_mode(
            &manifest_path,
            capsule_core::types::ValidationMode::Strict,
        )
        .expect("load manifest");

        let authoritative_input = RunAuthoritativeInput {
            kind: RunAuthoritativeInputKind::SourceOnly,
            project_root: dir.path().to_path_buf(),
            lock: AtoLock::default(),
            lock_path: dir.path().join("ato.lock.json"),
            sidecar_path: dir.path().join("provenance.json"),
            bridge_manifest_path: manifest_path.clone(),
            bridge_manifest_sha256: "deadbeef".to_string(),
            effective_state: crate::application::workspace::state::EffectiveLockState::default(),
            compatibility_legacy_lock: None,
        };

        assert_eq!(
            authoritative_input.kind,
            RunAuthoritativeInputKind::SourceOnly
        );
        assert_eq!(
            authoritative_input.lock_path,
            dir.path().join("ato.lock.json")
        );
        assert_eq!(
            authoritative_input.sidecar_path,
            dir.path().join("provenance.json")
        );
        assert!(authoritative_input.lock.contract.entries.is_empty());

        let error = validate_authoritative_bridge(
            Some(&authoritative_input),
            &manifest_path,
            &loaded_manifest,
        )
        .expect_err("bridge mismatch must fail closed");

        assert!(error.to_string().contains("generated manifest bridge"));
    }
}
