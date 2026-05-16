use anyhow::{Context, Result};
use async_trait::async_trait;
use cliclack::ProgressBar;
use ctrlc;
use goblin::elf::dynamic::DT_VERNEED;
use goblin::elf::Elf;
use goblin::mach::load_command::CommandVariant;
use goblin::mach::{Mach, SingleArch};
use regex::Regex;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};
use tracing::debug;

use crate::application::pipeline::cleanup::{run_sigint_cleanup, PipelineAttemptContext};
use crate::application::pipeline::consumer::ConsumerRunPipeline;
use crate::application::pipeline::executor::{HourglassPhaseRunner, PhaseAnnotation};
use crate::application::pipeline::hourglass;
use crate::application::pipeline::hourglass::{HourglassPhase, HourglassPhaseState};
use crate::application::pipeline::phases::run as run_phase;
use crate::application::ports::OutputPort;
use crate::application::source_inference;
use crate::application::workspace::state;
use crate::install::support::ResolvedCliExportRequest;
use crate::preview;
#[cfg(test)]
use crate::registry::store::RegistryStore;
use crate::reporters::CliReporter;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;
use crate::runtime::tree as runtime_tree;
use capsule_core::execution_plan::error::AtoExecutionError;
#[cfg(test)]
use capsule_core::execution_plan::guard::ExecutorKind;
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput, ATO_LOCK_FILE_NAME,
};
use capsule_core::lifecycle::LifecycleEvent;
use capsule_core::lockfile::{CAPSULE_LOCK_FILE_NAME, LEGACY_CAPSULE_LOCK_FILE_NAME};
use capsule_core::types::CapsuleManifest;
use capsule_core::{router, CapsuleReporter};

mod background;
mod preflight;
mod watch;

use background::*;
#[cfg(test)]
use preflight::*;
pub(crate) use preflight::{preflight_native_sandbox, run_v03_lifecycle_steps};

const BACKGROUND_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const BACKGROUND_READY_WAIT_TIMEOUT_ENV: &str = "ATO_BACKGROUND_READY_WAIT_TIMEOUT_SECS";

type RunPipelineState = run_phase::RunPipelineState;

pub struct RunArgs {
    pub target: PathBuf,
    pub target_label: Option<String>,
    pub args: Vec<String>,
    pub watch: bool,
    pub background: bool,
    pub nacelle: Option<PathBuf>,
    pub registry: Option<String>,
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub compatibility_fallback: Option<String>,
    pub provider_toolchain_requested: crate::ProviderToolchain,
    pub explicit_commit: Option<String>,
    pub assume_yes: bool,
    pub verbose: bool,
    pub agent_mode: crate::RunAgentMode,
    pub agent_local_root: Option<PathBuf>,
    pub keep_failed_artifacts: bool,
    pub auto_fix_mode: Option<crate::GitHubAutoFixMode>,
    pub allow_unverified: bool,
    pub read_grants: Vec<String>,
    pub write_grants: Vec<String>,
    pub read_write_grants: Vec<String>,
    pub caller_cwd: PathBuf,
    pub effective_cwd: Option<PathBuf>,
    pub export_request: Option<ResolvedCliExportRequest>,
    pub state_bindings: Vec<String>,
    pub inject_bindings: Vec<String>,
    pub build_policy: crate::application::build_materialization::BuildPolicy,
    pub cache_strategy: crate::application::dependency_materializer::CacheStrategy,
    pub reporter: Arc<CliReporter>,
    pub preview_mode: bool,
}

pub async fn execute(args: RunArgs) -> Result<()> {
    // Boundary-level receipt emission (refs #74, #99). Wraps the inner
    // pipeline so that on the recoverable-failure / aborted path a
    // *partial* execution receipt is emitted to
    // `~/.ato/executions/<id>/receipt.json`, even though the inner
    // pipeline never reached `build_prelaunch_receipt_v2`. On the
    // happy path the inner pipeline already emitted a full v2
    // receipt; the wrapper observes `Ok(_)` and returns it unchanged.
    let ctx = crate::application::receipt_boundary::ReceiptEmissionContext::for_boundary("ato run");
    crate::application::receipt_boundary::emit_receipt_on_result(ctx, move |sink| async move {
        if args.watch {
            execute_watch_mode_with_install(args, sink).await
        } else {
            execute_normal_mode(args, sink).await
        }
    })
    .await
}

async fn execute_watch_mode_with_install(
    args: RunArgs,
    _receipt_graph_id_sink: crate::application::receipt_boundary::ReceiptGraphIdSink,
) -> Result<()> {
    // Watch mode bails before the receipt emit site (no provider-backed
    // workspace, no v2 bundle build). The sink stays empty, and the
    // partial-receipt boundary falls back to ctx-level ids if any.
    let install = run_install_phase(&args).await?;
    report_dependency_projection(&args, &install.dependency_projection)?;
    if matches!(
        install.manifest_outcome,
        crate::install::support::LocalRunManifestPreparationOutcome::CreatedManualManifest
    ) {
        return Ok(());
    }

    if let Some(provider_workspace) = install.resolved_target.provider_workspace.as_ref() {
        if !args.keep_failed_artifacts {
            let _ = std::fs::remove_dir_all(&provider_workspace.workspace_root);
        } else {
            crate::install::provider_target::maybe_report_kept_failed_provider_workspace(
                &provider_workspace.workspace_root,
                args.reporter.is_json(),
            );
        }
        anyhow::bail!("--watch is not supported for provider-backed targets in this MVP");
    }

    // Create a temporary attempt context for the pre-watch normalization.
    // The attempt dir it creates is ephemeral: execute_watch_mode() performs
    // its own independent normalization (L1217), so this one is never used.
    // unwind_cleanup() is called unconditionally — before the `?` propagation —
    // so the attempt dir is removed whether normalization succeeds or fails.
    let mut pre_watch_attempt = PipelineAttemptContext::default();
    let result = normalize_run_target_after_install(
        &args,
        &install.resolved_target,
        Some(&mut pre_watch_attempt),
    )
    .await;
    pre_watch_attempt.unwind_cleanup();
    let normalized = result?;
    execute_watch_mode(RunArgs {
        target: normalized.target,
        agent_local_root: install.resolved_target.agent_local_root,
        export_request: install.resolved_target.export_request,
        ..args
    })
}

async fn prepare_capsule_target(
    args: &RunArgs,
    capsule_path: &PathBuf,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<PathBuf> {
    if let Some(manifest_path) = runtime_tree::prepare_store_runtime_for_capsule(capsule_path)? {
        debug!(
            manifest_path = %manifest_path.display(),
            "Running capsule from isolated runtime tree"
        );
        return Ok(manifest_path);
    }

    debug!(capsule = %capsule_path.display(), "Extracting capsule archive");

    let extract_dir = capsule_path
        .parent()
        .map(|p| {
            p.join(format!(
                "{}-extracted",
                capsule_path.file_stem().unwrap().to_string_lossy()
            ))
        })
        .context("Failed to determine extraction directory")?;

    if let Some(attempt) = attempt {
        let mut scope = attempt.cleanup_scope();
        scope.register_remove_dir(extract_dir.clone());
    }

    if extract_dir.exists() {
        debug!(
            extract_dir = %extract_dir.display(),
            "Removing existing extracted directory before extraction"
        );
        fs::remove_dir_all(&extract_dir)?;
    }

    fs::create_dir_all(&extract_dir).with_context(|| {
        format!(
            "Failed to create extraction directory: {}",
            extract_dir.display()
        )
    })?;

    let mut archive = fs::File::open(capsule_path)
        .with_context(|| format!("Failed to open capsule file: {}", capsule_path.display()))?;

    let mut ar = tar::Archive::new(&mut archive);
    ar.unpack(&extract_dir)
        .with_context(|| format!("Failed to extract capsule to: {}", extract_dir.display()))?;

    debug!(extract_dir = %extract_dir.display(), "Capsule extracted");

    let cas_provider = capsule_core::capsule::CasProvider::from_env();
    capsule_core::capsule::unpack_payload_from_capsule_root_with_provider(
        &extract_dir,
        &extract_dir,
        &cas_provider,
    )
    .with_context(|| "Failed to extract payload from capsule root")?;
    fs::remove_file(extract_dir.join("payload.tar.zst")).ok();
    fs::remove_file(extract_dir.join("payload.tar")).ok();
    debug!("Payload extracted");

    let manifest_path = extract_dir.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!("Extracted capsule does not contain capsule.toml");
    }

    let original_dir = capsule_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty() && *parent != std::path::Path::new("."))
        .map(std::path::Path::to_path_buf)
        .unwrap_or(std::env::current_dir().context("Failed to get current directory")?);

    let has_source_files = check_has_source_files(&extract_dir);
    let original_has_source = check_has_source_files(&original_dir);

    if !has_source_files && original_has_source {
        debug!("Copying source files to extracted directory");

        copy_source_files(&original_dir, &extract_dir, &args.reporter).await?;

        debug!("Source files copied");
    }

    Ok(manifest_path)
}

async fn copy_source_files(
    original_dir: &Path,
    extract_dir: &Path,
    _reporter: &Arc<CliReporter>,
) -> Result<()> {
    let entries = fs::read_dir(original_dir).with_context(|| {
        format!(
            "Failed to read original directory: {}",
            original_dir.display()
        )
    })?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();

        if path == extract_dir || path.starts_with(extract_dir) {
            continue;
        }

        if file_name == "capsule.toml"
            || file_name == CAPSULE_LOCK_FILE_NAME
            || file_name == LEGACY_CAPSULE_LOCK_FILE_NAME
            || file_name == "config.json"
        {
            continue;
        }

        if path.is_dir() && file_name.to_string_lossy().ends_with("-extracted") {
            continue;
        }

        if path.is_file() {
            let should_skip_artifact = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "capsule" | "sig" | "bundle" | "zst" | "tar"
                    )
                })
                .unwrap_or(false);
            if should_skip_artifact {
                continue;
            }
        }

        if file_name == "source" && path.is_dir() {
            let dest = extract_dir.join("source");
            crate::fs_copy::copy_path_recursive(&path, &dest)?;
            debug!("Copied source/");
        } else if path.is_file() {
            let dest = extract_dir.join(&file_name);
            fs::copy(&path, &dest)?;
            debug!(file = %file_name.to_string_lossy(), "Copied file into extracted capsule");
        } else if path.is_dir() && !is_hidden(&file_name) {
            let dest = extract_dir.join(&file_name);
            crate::fs_copy::copy_path_recursive(&path, &dest)?;
            debug!(dir = %file_name.to_string_lossy(), "Copied directory into extracted capsule");
        }
    }

    Ok(())
}

fn check_has_source_files(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    let mut file_count = 0usize;
    let mut has_actual_source_files = false;

    for entry in entries.flatten() {
        file_count += 1;
        let file_name = entry.file_name();
        let path = entry.path();

        if file_name == "capsule.toml"
            || file_name == CAPSULE_LOCK_FILE_NAME
            || file_name == LEGACY_CAPSULE_LOCK_FILE_NAME
            || file_name == "config.json"
            || file_name == "signature.json"
        {
            continue;
        }

        if path.is_file() {
            let name = file_name.to_string_lossy();
            if name == "package.json"
                || name == "pyproject.toml"
                || name == "requirements.txt"
                || name == "go.mod"
                || name == "Cargo.toml"
            {
                return true;
            }
            if is_source_file(&file_name) {
                return true;
            }
            has_actual_source_files = true;
        }

        if path.is_dir() && !is_hidden(&file_name) {
            if file_name == "source"
                && fs::read_dir(&path)
                    .ok()
                    .and_then(|mut it| it.next())
                    .is_some()
            {
                return true;
            }

            if path.join("package.json").exists()
                || path.join("pyproject.toml").exists()
                || path.join("index.js").exists()
                || path.join("main.py").exists()
            {
                return true;
            }
        }
    }

    has_actual_source_files || (file_count > 5)
}

fn is_source_file(file_name: &std::ffi::OsString) -> bool {
    let exts = [
        "js", "ts", "py", "go", "rs", "json", "html", "css", "mjs", "cjs",
    ];
    if let Some(ext) = file_name.to_str().and_then(|s| s.rsplit('.').next()) {
        exts.contains(&ext)
    } else {
        false
    }
}

fn is_hidden(file_name: &std::ffi::OsString) -> bool {
    let bytes = file_name.as_os_str().as_encoded_bytes();
    bytes.first() == Some(&b'.') && bytes.len() > 1
}

fn build_consumer_run_request(
    args: &RunArgs,
    export_request: Option<ResolvedCliExportRequest>,
) -> run_phase::ConsumerRunRequest {
    run_phase::ConsumerRunRequest {
        target: args.target.clone(),
        target_label: args.target_label.clone(),
        args: args.args.clone(),
        read_grants: args.read_grants.clone(),
        write_grants: args.write_grants.clone(),
        read_write_grants: args.read_write_grants.clone(),
        caller_cwd: args.caller_cwd.clone(),
        effective_cwd: args.effective_cwd.clone(),
        authoritative_input: None,
        desktop_open_path: None,
        background: args.background,
        nacelle: args.nacelle.clone(),
        enforcement: args.enforcement.clone(),
        sandbox_mode: args.sandbox_mode,
        dangerously_skip_permissions: args.dangerously_skip_permissions,
        // Single read of CAPSULE_ALLOW_UNSAFE for the run pipeline (#73 PR-C).
        // Downstream code must consume `allow_unsafe` from the request rather
        // than re-reading the env. The historical argv `--dangerously-skip-permissions`
        // injection into a child supervisor (session.rs) is removed in the same PR.
        allow_unsafe: args.dangerously_skip_permissions
            || std::env::var("CAPSULE_ALLOW_UNSAFE").as_deref() == Ok("1"),
        compatibility_fallback: args.compatibility_fallback.clone(),
        provider_toolchain_requested: args.provider_toolchain_requested,
        explicit_commit: args.explicit_commit.clone(),
        assume_yes: args.assume_yes,
        verbose: args.verbose,
        agent_mode: args.agent_mode,
        agent_local_root: args.agent_local_root.clone(),
        registry: args.registry.clone(),
        keep_failed_artifacts: args.keep_failed_artifacts,
        auto_fix_mode: args.auto_fix_mode,
        allow_unverified: args.allow_unverified,
        export_request,
        state_bindings: args.state_bindings.clone(),
        inject_bindings: args.inject_bindings.clone(),
        build_policy: args.build_policy,
        cache_strategy: args.cache_strategy,
        reporter: args.reporter.clone(),
        preview_mode: args.preview_mode,
    }
}

fn build_consumer_run_request_with_target(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
    export_request: Option<ResolvedCliExportRequest>,
) -> run_phase::ConsumerRunRequest {
    let mut request = build_consumer_run_request(args, export_request);
    request.target = target.to_path_buf();
    request.agent_local_root = agent_local_root;
    request.authoritative_input = authoritative_input;
    request.desktop_open_path = desktop_open_path;
    request
}

#[derive(Debug, Clone)]
struct NormalizedRunTarget {
    target: PathBuf,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
}

fn authoritative_input_from_materialization(
    materialized: source_inference::RunMaterialization,
    cli_state_bindings: &[String],
    compatibility_legacy_lock: Option<run_phase::CompatibilityLegacyLockContext>,
) -> Result<run_phase::RunAuthoritativeInput> {
    let effective_state = state::resolve_effective_lock_state(
        &materialized.workspace_root,
        &materialized.lock,
        cli_state_bindings,
    )?;

    Ok(run_phase::RunAuthoritativeInput {
        lock: materialized.lock,
        lock_path: materialized.lock_path,
        workspace_root: materialized.workspace_root,
        materialization_root: materialized.project_root,
        effective_state,
        compatibility_legacy_lock,
    })
}

fn persist_provider_authoritative_lock_if_needed(
    resolved_target: &crate::install::support::ResolvedRunTarget,
    authoritative_input: &run_phase::RunAuthoritativeInput,
) -> Result<()> {
    let Some(provider_workspace) = resolved_target.provider_workspace.as_ref() else {
        return Ok(());
    };

    crate::install::provider_target::persist_provider_authoritative_lock(
        &provider_workspace.workspace_root,
        &provider_workspace.resolution_metadata_path,
        &authoritative_input.lock,
    )?;

    Ok(())
}

fn normalized_target_from_resolved_input(
    args: &RunArgs,
    resolved_input: ResolvedInput,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<NormalizedRunTarget> {
    match resolved_input {
        ResolvedInput::CanonicalLock { canonical, .. } => {
            let mut cleanup_scope = attempt.map(|attempt| attempt.cleanup_scope());
            let materialized = source_inference::materialize_run_from_canonical_lock(
                &canonical,
                cleanup_scope.as_mut(),
                args.reporter.clone(),
                args.assume_yes,
            )?;
            let target = materialized.project_root.clone();
            Ok(NormalizedRunTarget {
                target,
                authoritative_input: Some(authoritative_input_from_materialization(
                    materialized,
                    &args.state_bindings,
                    None,
                )?),
                desktop_open_path: None,
            })
        }
        ResolvedInput::CompatibilityProject { project, .. } => {
            let mut cleanup_scope = attempt.map(|attempt| attempt.cleanup_scope());
            let materialized = source_inference::materialize_run_from_compatibility(
                &project,
                cleanup_scope.as_mut(),
                args.reporter.clone(),
                args.assume_yes,
            )?;
            let target = materialized.project_root.clone();
            let compatibility_legacy_lock = project.legacy_lock.clone().map(|legacy_lock| {
                run_phase::CompatibilityLegacyLockContext {
                    manifest_path: project.manifest.path.clone(),
                    path: legacy_lock.path,
                    lock: legacy_lock.lock,
                }
            });
            Ok(NormalizedRunTarget {
                target,
                authoritative_input: Some(authoritative_input_from_materialization(
                    materialized,
                    &args.state_bindings,
                    compatibility_legacy_lock,
                )?),
                desktop_open_path: None,
            })
        }
        ResolvedInput::SourceOnly { source, .. } => {
            let mut cleanup_scope = attempt.map(|attempt| attempt.cleanup_scope());
            let materialized = source_inference::materialize_run_from_source_only(
                &source,
                cleanup_scope.as_mut(),
                args.reporter.clone(),
                args.assume_yes,
            )?;
            let target = materialized.project_root.clone();
            Ok(NormalizedRunTarget {
                target,
                authoritative_input: Some(authoritative_input_from_materialization(
                    materialized,
                    &args.state_bindings,
                    None,
                )?),
                desktop_open_path: None,
            })
        }
    }
}

struct RunProgress<'a> {
    args: &'a RunArgs,
}

#[derive(Default)]
struct RunExecuteHooks;

#[async_trait(?Send)]
impl run_phase::ConsumerRunExecuteHooks for RunExecuteHooks {
    fn preflight_native_sandbox(
        &self,
        nacelle_override: Option<PathBuf>,
        plan: &capsule_core::router::ManifestData,
        prepared: &run_phase::PreparedRunContext,
        effective_cwd: Option<&Path>,
        reporter: &Arc<CliReporter>,
    ) -> Result<PathBuf> {
        preflight_native_sandbox(nacelle_override, plan, prepared, effective_cwd, reporter)
    }

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
    ) -> Result<()> {
        complete_background_source_process(
            process,
            plan,
            runtime,
            scoped_id,
            BackgroundCompletionOptions {
                is_one_shot,
                ready_without_events,
                desktop_open_only,
                compatibility_host_mode,
            },
            reporter,
        )
        .await
    }

    async fn complete_foreground_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        reporter: Arc<CliReporter>,
        is_one_shot: bool,
        sandbox_initialized: bool,
        ipc_socket_mapped: bool,
        desktop_open_only: bool,
        use_progressive_ui: bool,
    ) -> Result<i32> {
        complete_foreground_source_process(
            process,
            reporter,
            is_one_shot,
            sandbox_initialized,
            ipc_socket_mapped,
            desktop_open_only,
            use_progressive_ui,
        )
        .await
    }
    async fn cleanup_existing_scoped_processes_before_run(
        &self,
        scoped_id: &str,
        reporter: &Arc<CliReporter>,
    ) -> Result<()> {
        cleanup_existing_scoped_processes_before_run(scoped_id, reporter).await
    }

    async fn notify_web_endpoint(
        &self,
        plan: &capsule_core::router::ManifestData,
        reporter: &Arc<CliReporter>,
    ) -> Result<()> {
        notify_web_endpoint(plan, reporter).await
    }

    fn process_runtime_label(
        &self,
        plan: &capsule_core::router::ManifestData,
        dangerous_skip_permissions: bool,
        compatibility_host_mode: CompatibilityHostMode,
    ) -> String {
        process_runtime_label(plan, dangerous_skip_permissions, compatibility_host_mode)
    }
}

impl run_phase::ConsumerRunProgress for RunProgress<'_> {
    fn start(&self, phase: HourglassPhase) {
        emit_run_phase_start(self.args, phase);
    }

    fn ok(&self, phase: HourglassPhase, detail: &str) {
        emit_run_phase_ok(self.args, phase, detail);
    }

    fn skip(&self, phase: HourglassPhase, detail: &str) {
        emit_run_phase_skip(self.args, phase, detail);
    }
}

struct ConsumerRunPhaseRunner<'a> {
    args: &'a RunArgs,
    state: Option<RunPipelineState>,
    target: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
    export_request: Option<ResolvedCliExportRequest>,
    agent_local_root: Option<PathBuf>,
    transient_workspace_root: Option<PathBuf>,
    provider_backed_target: bool,
    should_stop_after_install: bool,
    phase_annotations: std::collections::HashMap<HourglassPhase, PhaseAnnotation>,
    /// PR-3b boundary plumbing: handle to the `ReceiptEmissionContext`'s
    /// graph-id sink, owned by the wrapper in `execute()`. The Execute
    /// phase writes declared/resolved ids here immediately after
    /// `build_prelaunch_receipt_document_with_graph` so the partial
    /// receipt boundary observes the same ids on the failure path.
    receipt_graph_id_sink: crate::application::receipt_boundary::ReceiptGraphIdSink,
}

impl ConsumerRunPhaseRunner<'_> {
    fn take_state(&mut self, phase: HourglassPhase) -> Result<RunPipelineState> {
        self.state.take().with_context(|| {
            format!(
                "run pipeline phase {} requires the previous phase state",
                phase.as_str()
            )
        })
    }

    fn resolved_target(&self) -> &Path {
        self.target.as_deref().unwrap_or(self.args.target.as_path())
    }

    fn record_phase_annotation(&mut self, phase: HourglassPhase, annotation: PhaseAnnotation) {
        self.phase_annotations.insert(phase, annotation);
    }
}

#[async_trait(?Send)]
impl HourglassPhaseRunner for ConsumerRunPhaseRunner<'_> {
    fn should_continue(&self) -> bool {
        !self.should_stop_after_install
    }

    fn phase_annotation(&self, phase: HourglassPhase) -> Option<PhaseAnnotation> {
        self.phase_annotations.get(&phase).cloned()
    }

    async fn run_phase(
        &mut self,
        phase: HourglassPhase,
        attempt: &mut PipelineAttemptContext,
    ) -> Result<()> {
        match phase {
            HourglassPhase::Install => {
                let install = run_install_phase(self.args).await.inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Install, err);
                })?;
                report_dependency_projection(self.args, &install.dependency_projection)?;
                let normalized = normalize_run_target_after_install(
                    self.args,
                    &install.resolved_target,
                    Some(attempt),
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Install, err);
                })?;
                self.target = Some(normalized.target);
                self.authoritative_input = normalized.authoritative_input;
                self.desktop_open_path = normalized
                    .desktop_open_path
                    .or(install.resolved_target.desktop_open_path);
                self.export_request = install.resolved_target.export_request;
                self.agent_local_root = install.resolved_target.agent_local_root;
                self.transient_workspace_root =
                    install.resolved_target.transient_workspace_root.clone();
                self.provider_backed_target = install.resolved_target.provider_workspace.is_some();
                self.should_stop_after_install = matches!(
                    install.manifest_outcome,
                    crate::install::support::LocalRunManifestPreparationOutcome::CreatedManualManifest
                );
                let mut annotation = PhaseAnnotation::with_result_kind("executed");
                annotation.add_extra(
                    "run_workspace",
                    install
                        .dependency_projection
                        .run_workspace
                        .display()
                        .to_string(),
                );
                annotation.add_extra(
                    "dependency_cache.status",
                    install.dependency_projection.dependency_cache_status,
                );
                if let Some(hash) = install.dependency_projection.derivation_hash {
                    annotation.add_extra("derivation_hash", hash);
                }
                self.record_phase_annotation(HourglassPhase::Install, annotation);
                Ok(())
            }
            HourglassPhase::Prepare => {
                let state = run_prepare_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    self.desktop_open_path.clone(),
                    self.export_request.clone(),
                    Some(attempt),
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Prepare, err);
                })?;
                // PR-3b: hand the boundary's graph-id sink to the
                // pipeline state so the Execute phase can publish ids
                // immediately after building the LaunchGraphBundle.
                let state = run_phase::attach_receipt_graph_id_sink(
                    state,
                    self.receipt_graph_id_sink.clone(),
                );
                self.state = Some(state);
                self.record_phase_annotation(
                    HourglassPhase::Prepare,
                    PhaseAnnotation::with_result_kind("executed"),
                );
                Ok(())
            }
            HourglassPhase::Build => {
                // Pre-compute the observation+decision so diagnostic
                // annotations remain accurate even if `run_build_phase`
                // bails (e.g. `--no-build` without an existing record).
                let preview = self.state.as_ref().and_then(|state| {
                    let obs = crate::application::build_materialization::observe_for_plan(
                        &state.decision.plan,
                        &state.launch_ctx,
                    )
                    .ok()
                    .flatten()?;
                    let decision = crate::application::build_materialization::decide(
                        self.args.build_policy,
                        &obs,
                        &state.prepared.workspace_root,
                    );
                    Some((obs, decision.result_kind))
                });

                let input = self.take_state(HourglassPhase::Build)?;
                let result = run_build_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    self.desktop_open_path.clone(),
                    self.export_request.clone(),
                    input,
                )
                .await;

                // Prefer the post-build state's recorded kind (it accounts
                // for executor success / failure), but fall back to the
                // pre-computed decision so `--no-build` failures still emit
                // the right `result_kind=missing-materialization` etc.
                let observation_for_annotation = match &result {
                    Ok(state) => state
                        .build_observation
                        .clone()
                        .or_else(|| preview.as_ref().map(|(obs, _)| obs.clone())),
                    Err(_) => preview.as_ref().map(|(obs, _)| obs.clone()),
                };
                let decision_for_annotation = match &result {
                    Ok(state) => state
                        .build_decision_kind
                        .or_else(|| preview.as_ref().map(|(_, kind)| *kind)),
                    Err(_) => preview.as_ref().map(|(_, kind)| *kind),
                };

                let mut annotation = PhaseAnnotation::with_result_kind(
                    decision_for_annotation
                        .map(|kind| kind.as_str())
                        .unwrap_or("executed"),
                );
                if let Some(observation) = observation_for_annotation {
                    annotation.add_extra("source", observation.source.timing_label());
                    if let Some(label) = observation.source.heuristic_label() {
                        annotation.add_extra("heuristic", label);
                    }
                    annotation.add_extra("target", observation.target);
                    annotation.add_extra("digest", observation.input_digest);
                }
                self.record_phase_annotation(HourglassPhase::Build, annotation);

                let state = result.inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Build, err);
                })?;
                self.state = Some(state);
                Ok(())
            }
            HourglassPhase::Verify => {
                let input = self.take_state(HourglassPhase::Verify)?;
                let state = run_verify_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    self.desktop_open_path.clone(),
                    self.export_request.clone(),
                    input,
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Verify, err);
                })?;
                self.state = Some(state);
                self.record_phase_annotation(
                    HourglassPhase::Verify,
                    PhaseAnnotation::with_result_kind("executed"),
                );
                Ok(())
            }
            HourglassPhase::DryRun => {
                let input = self.take_state(HourglassPhase::DryRun)?;
                let state = run_dry_run_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    self.desktop_open_path.clone(),
                    self.export_request.clone(),
                    input,
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::DryRun, err);
                })?;
                self.state = Some(state);
                self.record_phase_annotation(
                    HourglassPhase::DryRun,
                    PhaseAnnotation::with_result_kind("executed"),
                );
                Ok(())
            }
            HourglassPhase::Execute => {
                // PR-3b (PR #180 review fix): defensively re-inject the
                // boundary sink at Execute entry. Prepare already set
                // it, but a future Build / Verify / DryRun refactor
                // that reconstructs `RunPipelineState` would silently
                // drop the field; using the helper here pins the
                // wire-up at the consumer site.
                let input = run_phase::attach_receipt_graph_id_sink(
                    self.take_state(HourglassPhase::Execute)?,
                    self.receipt_graph_id_sink.clone(),
                );
                let result = run_execute_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    self.desktop_open_path.clone(),
                    self.export_request.clone(),
                    input,
                    Some(attempt),
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Execute, err);
                });
                if result.is_ok() {
                    self.record_phase_annotation(
                        HourglassPhase::Execute,
                        PhaseAnnotation::with_result_kind("executed"),
                    );
                }
                result
            }
            HourglassPhase::Finalize | HourglassPhase::Publish => anyhow::bail!(
                "unsupported run pipeline phase {} in run command",
                phase.as_str()
            ),
        }
    }
}

fn report_dependency_projection(
    args: &RunArgs,
    projection: &crate::application::dependency_materializer::DependencyProjection,
) -> Result<()> {
    if args.reporter.is_json() {
        return Ok(());
    }

    futures::executor::block_on(args.reporter.notify(format!(
        "Using isolated run workspace: {}",
        projection.run_workspace.display()
    )))?;
    futures::executor::block_on(args.reporter.notify(format!(
        "Dependency cache: {}",
        projection.dependency_cache_status
    )))?;
    Ok(())
}

async fn execute_normal_mode(
    args: RunArgs,
    receipt_graph_id_sink: crate::application::receipt_boundary::ReceiptGraphIdSink,
) -> Result<()> {
    // Register a Ctrl+C handler so in-flight run artifacts are cleaned up on SIGINT.
    let _ = ctrlc::set_handler(|| {
        run_sigint_cleanup();
        std::process::exit(130);
    });

    let pipeline = ConsumerRunPipeline::standard();
    let mut runner = ConsumerRunPhaseRunner {
        args: &args,
        state: None,
        target: None,
        authoritative_input: None,
        desktop_open_path: None,
        export_request: args.export_request.clone(),
        agent_local_root: args.agent_local_root.clone(),
        transient_workspace_root: None,
        provider_backed_target: false,
        should_stop_after_install: false,
        phase_annotations: std::collections::HashMap::new(),
        receipt_graph_id_sink,
    };

    let result = pipeline.run(&mut runner).await;
    if result.is_ok() {
        if !args.background {
            if let Some(transient_workspace_root) = runner.transient_workspace_root.as_ref() {
                let _ = fs::remove_dir_all(transient_workspace_root);
            }
        }
    } else if args.keep_failed_artifacts {
        if let Some(transient_workspace_root) = runner.transient_workspace_root.as_ref() {
            if runner.provider_backed_target {
                crate::install::provider_target::maybe_report_kept_failed_provider_workspace(
                    transient_workspace_root,
                    args.reporter.is_json(),
                );
            } else if !args.reporter.is_json() {
                eprintln!(
                    "⚠️  Kept transient run workspace for debugging: {}",
                    transient_workspace_root.display()
                );
            }
        }
    }
    result
}

fn run_phase_detail(boundary: HourglassPhase) -> &'static str {
    match boundary {
        HourglassPhase::Install => "target resolution and install",
        HourglassPhase::Prepare => "manifest and launch context resolution",
        HourglassPhase::Build => "build and lifecycle hooks",
        HourglassPhase::Finalize => {
            panic!("unsupported run phase {}", boundary.as_str())
        }
        HourglassPhase::Verify => "execution plan verification",
        HourglassPhase::DryRun => "runtime preflight",
        HourglassPhase::Execute => "capsule execution",
        HourglassPhase::Publish => {
            panic!("unsupported run phase {}", boundary.as_str())
        }
    }
}

fn emit_run_phase(
    args: &RunArgs,
    boundary: HourglassPhase,
    state: HourglassPhaseState,
    detail: &str,
) {
    debug!(
        phase = boundary.as_str(),
        state = state.as_str(),
        "Running run pipeline phase"
    );
    if args.verbose {
        hourglass::eprint_phase_line(args.reporter.is_json(), boundary, state, detail);
    }
}

fn emit_run_phase_start(args: &RunArgs, boundary: HourglassPhase) {
    emit_run_phase(
        args,
        boundary,
        HourglassPhaseState::Run,
        run_phase_detail(boundary),
    );
}

fn emit_run_phase_ok(args: &RunArgs, boundary: HourglassPhase, detail: &str) {
    emit_run_phase(args, boundary, HourglassPhaseState::Ok, detail);
}

fn emit_run_phase_skip(args: &RunArgs, boundary: HourglassPhase, detail: &str) {
    emit_run_phase(args, boundary, HourglassPhaseState::Skip, detail);
}

fn emit_run_phase_failure(args: &RunArgs, boundary: HourglassPhase, error: &anyhow::Error) {
    emit_run_phase(
        args,
        boundary,
        HourglassPhaseState::Fail,
        &error.to_string(),
    );
}

async fn normalize_run_target_after_install(
    args: &RunArgs,
    resolved_target: &crate::install::support::ResolvedRunTarget,
    mut attempt: Option<&mut PipelineAttemptContext>,
) -> Result<NormalizedRunTarget> {
    if let Some(transient_workspace_root) = resolved_target.transient_workspace_root.as_ref() {
        if !args.keep_failed_artifacts {
            if let Some(attempt) = attempt.as_mut() {
                let mut scope = (*attempt).cleanup_scope();
                scope.register_remove_dir(transient_workspace_root.clone());
            }
        }
    }

    let target_path = resolved_target.path.as_path();

    if resolved_target.provider_workspace.is_some() {
        let normalized = normalized_target_from_resolved_input(
            args,
            resolve_authoritative_input(target_path, ResolveInputOptions::default())?,
            attempt.as_deref_mut(),
        )?;
        if let Some(authoritative_input) = normalized.authoritative_input.as_ref() {
            persist_provider_authoritative_lock_if_needed(resolved_target, authoritative_input)?;
        }
        return Ok(normalized);
    }

    if resolved_target.transient_workspace_root.is_some()
        && resolved_target.provider_workspace.is_none()
        && target_path.is_dir()
        && target_path.join("capsule.toml").exists()
    {
        return Ok(NormalizedRunTarget {
            target: resolved_target.path.clone(),
            authoritative_input: None,
            desktop_open_path: None,
        });
    }

    if target_path
        .extension()
        .map(|value| value.eq_ignore_ascii_case("capsule"))
        .unwrap_or(false)
    {
        let target = prepare_capsule_target(args, &resolved_target.path, attempt).await?;
        return Ok(NormalizedRunTarget {
            target,
            authoritative_input: None,
            desktop_open_path: None,
        });
    }

    if target_path.is_file()
        && target_path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| {
                value.eq_ignore_ascii_case("exe") || value.eq_ignore_ascii_case("AppImage")
            })
            .unwrap_or(false)
    {
        let mut cleanup_scope = attempt.as_mut().map(|attempt| (*attempt).cleanup_scope());
        let materialized = source_inference::materialize_run_from_explicit_native_artifact(
            target_path,
            cleanup_scope.as_mut(),
            args.reporter.clone(),
            args.assume_yes,
        )?;
        let target = materialized.project_root.clone();
        return Ok(NormalizedRunTarget {
            target,
            authoritative_input: Some(authoritative_input_from_materialization(
                materialized,
                &args.state_bindings,
                None,
            )?),
            desktop_open_path: None,
        });
    }

    if target_path.is_dir()
        || target_path.file_name().and_then(|value| value.to_str()) == Some("capsule.toml")
        || target_path.file_name().and_then(|value| value.to_str()) == Some(ATO_LOCK_FILE_NAME)
        || target_path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| {
                value.eq_ignore_ascii_case("py")
                    || value.eq_ignore_ascii_case("ts")
                    || value.eq_ignore_ascii_case("tsx")
                    || value.eq_ignore_ascii_case("js")
                    || value.eq_ignore_ascii_case("jsx")
            })
            .unwrap_or(false)
    {
        return normalized_target_from_resolved_input(
            args,
            resolve_authoritative_input(target_path, ResolveInputOptions::default())?,
            attempt,
        );
    }

    Ok(NormalizedRunTarget {
        target: resolved_target.path.clone(),
        authoritative_input: None,
        desktop_open_path: None,
    })
}

async fn run_install_phase(args: &RunArgs) -> Result<run_phase::RunInstallPhaseResult> {
    let request = build_consumer_run_request(args, args.export_request.clone());
    let progress = RunProgress { args };
    run_phase::run_install_phase(&request, &progress).await
}

async fn run_prepare_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
    export_request: Option<ResolvedCliExportRequest>,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<RunPipelineState> {
    let request = build_consumer_run_request_with_target(
        args,
        target,
        agent_local_root,
        authoritative_input,
        desktop_open_path,
        export_request,
    );
    let progress = RunProgress { args };
    run_phase::run_prepare_phase(&request, &progress, attempt).await
}

async fn run_build_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
    export_request: Option<ResolvedCliExportRequest>,
    state: RunPipelineState,
) -> Result<RunPipelineState> {
    let request = build_consumer_run_request_with_target(
        args,
        target,
        agent_local_root,
        authoritative_input,
        desktop_open_path,
        export_request,
    );
    let progress = RunProgress { args };
    run_phase::run_build_phase(&request, &progress, state).await
}

async fn run_verify_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
    export_request: Option<ResolvedCliExportRequest>,
    state: RunPipelineState,
) -> Result<RunPipelineState> {
    let request = build_consumer_run_request_with_target(
        args,
        target,
        agent_local_root,
        authoritative_input,
        desktop_open_path,
        export_request,
    );
    let progress = RunProgress { args };
    run_phase::run_verify_phase(&request, &progress, state).await
}

async fn run_dry_run_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
    export_request: Option<ResolvedCliExportRequest>,
    state: RunPipelineState,
) -> Result<RunPipelineState> {
    let request = build_consumer_run_request_with_target(
        args,
        target,
        agent_local_root,
        authoritative_input,
        desktop_open_path,
        export_request,
    );
    let progress = RunProgress { args };
    run_phase::run_dry_run_phase(&request, &progress, state).await
}

#[allow(clippy::too_many_arguments)]
async fn run_execute_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    desktop_open_path: Option<PathBuf>,
    export_request: Option<ResolvedCliExportRequest>,
    state: RunPipelineState,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<()> {
    let request = build_consumer_run_request_with_target(
        args,
        target,
        agent_local_root,
        authoritative_input,
        desktop_open_path,
        export_request,
    );
    let progress = RunProgress { args };
    run_phase::run_execute_phase(&request, &progress, state, attempt, &RunExecuteHooks).await
}

#[cfg(test)]
async fn reroute_auto_provisioned_execution(
    decision: capsule_core::router::RuntimeDecision,
    launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
    prepared: &run_phase::PreparedRunContext,
    reporter: Arc<CliReporter>,
    preview_mode: bool,
    shadow_manifest_path: &Path,
) -> Result<(
    capsule_core::router::RuntimeDecision,
    crate::executors::launch_context::RuntimeLaunchContext,
    run_phase::PreparedRunContext,
)> {
    run_phase::reroute_auto_provisioned_execution(
        decision,
        launch_ctx,
        prepared,
        reporter,
        preview_mode,
        shadow_manifest_path,
    )
    .await
}

fn resolve_state_source_overrides(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    run_phase::resolve_state_source_overrides(manifest, raw_bindings)
}

#[cfg(test)]
fn resolve_state_source_overrides_with_store(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
    store: Option<&RegistryStore>,
) -> Result<std::collections::HashMap<String, String>> {
    run_phase::resolve_state_source_overrides_with_store(manifest, raw_bindings, store)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ForegroundEventMessage {
    Notify(String),
    Warn(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundStartupOutcome {
    Ready,
    CompletedSuccessfully,
    TimedOut,
    FailedBeforeReady,
}

type CompatibilityHostMode = run_phase::CompatibilityHostMode;

#[cfg(test)]
fn resolve_compatibility_host_mode(
    executor_kind: ExecutorKind,
    compatibility_fallback: Option<&str>,
) -> Result<CompatibilityHostMode> {
    run_phase::resolve_compatibility_host_mode(executor_kind, compatibility_fallback)
}

fn execute_watch_mode(args: RunArgs) -> Result<()> {
    let manifest_path = if args.target.is_dir() {
        args.target.join("capsule.toml")
    } else {
        args.target.clone()
    };
    let preview_mode = args.preview_mode
        || (manifest_path.exists()
            && preview::load_preview_session_for_manifest(&manifest_path)?.is_some());
    let resolved = crate::install::support::ResolvedRunTarget {
        path: args.target.clone(),
        agent_local_root: args.agent_local_root.clone(),
        desktop_open_path: None,
        export_request: args.export_request.clone(),
        provider_workspace: None,
        transient_workspace_root: None,
    };
    let normalized =
        futures::executor::block_on(normalize_run_target_after_install(&args, &resolved, None))?;
    let decision = if let Some(authoritative_input) = normalized.authoritative_input.as_ref() {
        let mut decision = capsule_core::router::route_lock_with_state_overrides(
            &authoritative_input.lock_path,
            &authoritative_input.lock,
            &authoritative_input.materialization_root,
            router::ExecutionProfile::Dev,
            args.target_label.as_deref(),
            authoritative_input
                .effective_state
                .state_source_overrides
                .clone(),
        )?;
        decision.plan.workspace_root = authoritative_input.workspace_root.clone();
        decision
    } else {
        let manifest = if preview_mode {
            capsule_core::manifest::load_manifest_with_validation_mode(
                &manifest_path,
                capsule_core::types::ValidationMode::Preview,
            )?
            .model
        } else {
            CapsuleManifest::load_from_file(&manifest_path)?
        };
        let state_source_overrides =
            resolve_state_source_overrides(&manifest, &args.state_bindings)?;
        capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
            &manifest_path,
            router::ExecutionProfile::Dev,
            args.target_label.as_deref(),
            state_source_overrides,
            if preview_mode {
                capsule_core::types::ValidationMode::Preview
            } else {
                capsule_core::types::ValidationMode::Strict
            },
        )?
    };
    if decision.plan.is_orchestration_mode() {
        anyhow::bail!("--watch is not supported for orchestration mode");
    }
    if matches!(decision.kind, capsule_core::router::RuntimeKind::Oci) {
        anyhow::bail!("--watch is not supported for runtime=oci");
    }

    futures::executor::block_on(CapsuleReporter::notify(
        &*args.reporter,
        "👀 Starting watch mode (foreground)".to_string(),
    ))?;

    let config = watch::WatchConfig::default();

    futures::executor::block_on(CapsuleReporter::notify(
        &*args.reporter,
        format!(
            "📊 Watch config: patterns={}, ignore={}, debounce={}ms",
            config.watch_patterns.join(", "),
            config.ignore_patterns.join(", "),
            config.debounce_ms
        ),
    ))?;

    let (_watcher, capsule_handle) =
        watch::watch_directory(args.target.clone(), config, args.reporter.clone())?;

    let reporter_for_cleanup = args.reporter.clone();

    ctrlc::set_handler(move || {
        run_sigint_cleanup();
        let _ = capsule_handle.stop();
        let _ = futures::executor::block_on(CapsuleReporter::warn(
            &*reporter_for_cleanup,
            "👋 Watch mode stopped".to_string(),
        ));
        std::process::exit(0);
    })
    .map_err(|e| anyhow::anyhow!("Failed to set Ctrl+C handler: {:?}", e))?;

    std::thread::park();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        background_ready_message, foreground_native_event_messages,
        initial_foreground_native_messages, normalize_run_target_after_install,
        plan_v03_provision_command, preflight_required_environment_variables,
        process_runtime_label, reroute_auto_provisioned_execution, resolve_compatibility_host_mode,
        resolve_python_dependency_lock_path, resolve_state_source_overrides_with_store,
        run_phase_detail, CompatibilityHostMode, ForegroundEventMessage, RunArgs,
    };
    use crate::executors::launch_context::{InjectedMount, RuntimeLaunchContext};
    use crate::registry::store::RegistryStore;
    use crate::reporters::CliReporter;
    use capsule_core::ato_lock::{self, AtoLock};
    use capsule_core::execution_plan::guard::ExecutorKind;
    use capsule_core::lifecycle::LifecycleEvent;
    use capsule_core::router::{self, ExecutionProfile, ManifestData};
    use capsule_core::types::CapsuleManifest;
    use serde_json::{json, Value};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn resolve_python_dependency_lock_path_prefers_source_uv_lock() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(tmp.path().join("source").join("uv.lock"), "").expect("write uv.lock");

        let found = resolve_python_dependency_lock_path(tmp.path()).expect("must resolve uv.lock");
        assert_eq!(found, tmp.path().join("source").join("uv.lock"));
    }

    #[test]
    fn v03_node_provision_prefers_single_detected_lockfile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package.json"), "{}\n").expect("write package.json");
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                (
                    "run_command",
                    toml::Value::String("pnpm start -- --port $PORT".to_string()),
                ),
            ],
        );

        let command = plan_v03_provision_command(&plan).expect("plan provision");
        assert_eq!(command.as_deref(), Some("pnpm install"));
    }

    #[test]
    fn v03_node_provision_supports_yarn_lockfile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package.json"), "{}\n").expect("write package.json");
        std::fs::write(tmp.path().join("yarn.lock"), "# yarn lockfile v1\n")
            .expect("write yarn lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                (
                    "run_command",
                    toml::Value::String("yarn dev -- --port $PORT".to_string()),
                ),
            ],
        );

        let command = plan_v03_provision_command(&plan).expect("plan provision");
        assert_eq!(command.as_deref(), Some("yarn install"));
    }

    #[test]
    fn v03_node_provision_prefers_pnpm_on_ambiguous_lockfiles() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package.json"), "{}\n").expect("write package.json");
        std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write package lock");
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                (
                    "run_command",
                    toml::Value::String("npm start -- --port $PORT".to_string()),
                ),
            ],
        );

        // Multiple lockfiles: pnpm takes priority over npm
        let command = plan_v03_provision_command(&plan).expect("must resolve ambiguity");
        assert_eq!(command.as_deref(), Some("pnpm install"));
    }

    #[test]
    fn v03_node_provision_uses_target_working_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("apps").join("web");
        std::fs::create_dir_all(&app_dir).expect("create app dir");
        std::fs::write(app_dir.join("package.json"), "{}\n").expect("write package.json");
        std::fs::write(app_dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                ("working_dir", toml::Value::String("apps/web".to_string())),
                (
                    "run_command",
                    toml::Value::String("pnpm start -- --port $PORT".to_string()),
                ),
            ],
        );

        let command = plan_v03_provision_command(&plan).expect("plan provision");
        assert_eq!(command.as_deref(), Some("pnpm install"));
    }

    #[test]
    fn preflight_required_env_fails_when_missing_or_empty() {
        let key_missing = "ATO_TEST_REQUIRED_ENV_MISSING";
        let key_empty = "ATO_TEST_REQUIRED_ENV_EMPTY";
        std::env::remove_var(key_missing);
        std::env::set_var(key_empty, "");

        let plan = manifest_with_required_env(vec![key_missing, key_empty]);
        let err = preflight_required_environment_variables(&plan).expect_err("must fail-closed");
        let msg = err.to_string();
        assert!(msg.contains(key_missing), "msg={msg}");
        assert!(msg.contains(key_empty), "msg={msg}");

        std::env::remove_var(key_empty);
    }

    #[test]
    fn preflight_required_env_passes_when_set() {
        let key = "ATO_TEST_REQUIRED_ENV_SET";
        std::env::set_var(key, "ok");

        let plan = manifest_with_required_env(vec![key]);
        assert!(preflight_required_environment_variables(&plan).is_ok());

        std::env::remove_var(key);
    }

    #[test]
    fn preflight_required_env_passes_with_runtime_override() {
        let key = "ATO_TEST_REQUIRED_ENV_FROM_OVERRIDE";
        std::env::set_var("ATO_UI_OVERRIDE_ENV_JSON", format!(r#"{{"{}":"ok"}}"#, key));

        let plan = manifest_with_required_env(vec![key]);
        assert!(preflight_required_environment_variables(&plan).is_ok());

        std::env::remove_var("ATO_UI_OVERRIDE_ENV_JSON");
    }

    #[test]
    fn foreground_native_messages_include_boot_sequence() {
        let messages = initial_foreground_native_messages(true, true);
        assert_eq!(
            messages,
            vec![
                "[✓] Sandbox initialized".to_string(),
                "[✓] IPC socket mapped".to_string()
            ]
        );
    }

    #[test]
    fn foreground_native_ipc_ready_message_matches_expected_copy() {
        let message = foreground_native_event_messages(
            &LifecycleEvent::Ready {
                service: "main".to_string(),
                endpoint: Some("unix:///tmp/main.sock".to_string()),
                port: None,
            },
            false,
            false,
        );

        assert_eq!(
            message,
            vec![
                ForegroundEventMessage::Notify(
                    "[✓] Service is ready (ready event received)".to_string()
                ),
                ForegroundEventMessage::Notify("    Streaming logs...".to_string())
            ]
        );
    }

    #[test]
    fn foreground_native_service_exited_warns_before_readiness() {
        let message = foreground_native_event_messages(
            &LifecycleEvent::Exited {
                service: "main".to_string(),
                exit_code: Some(42),
            },
            false,
            false,
        );

        assert_eq!(
            message,
            vec![ForegroundEventMessage::Warn(
                "❌ Service 'main' exited before readiness (exit code: 42)".to_string()
            )]
        );
    }

    #[test]
    fn foreground_native_one_shot_exit_zero_is_success() {
        let message = foreground_native_event_messages(
            &LifecycleEvent::Exited {
                service: "main".to_string(),
                exit_code: Some(0),
            },
            false,
            true,
        );

        assert_eq!(
            message,
            vec![ForegroundEventMessage::Notify(
                "[✓] Command completed successfully (exit code: 0)".to_string()
            )]
        );
    }

    #[test]
    fn compatibility_host_mode_enables_nodecompat_fallback() {
        let mode = resolve_compatibility_host_mode(ExecutorKind::NodeCompat, Some("host"))
            .expect("resolve fallback mode");
        assert_eq!(mode, CompatibilityHostMode::Enabled);
    }

    #[test]
    fn compatibility_host_mode_rejects_deno_fallback() {
        let err = resolve_compatibility_host_mode(ExecutorKind::Deno, Some("host"))
            .expect_err("must reject deno fallback");
        assert!(err.to_string().contains("native and node-compatible"));
    }

    #[test]
    fn compatibility_host_mode_changes_ready_copy() {
        let message =
            background_ready_message("capsule-42", CompatibilityHostMode::Enabled, false, false);
        assert_eq!(
            message,
            "✔ Capsule is ready (Host Fallback, ID: capsule-42)"
        );
    }

    #[test]
    fn desktop_open_only_changes_ready_copy() {
        let message =
            background_ready_message("capsule-42", CompatibilityHostMode::Disabled, true, false);
        assert_eq!(
            message,
            "🚀 Desktop app launch requested in background (ID: capsule-42)"
        );
    }

    #[test]
    fn foreground_desktop_open_spinner_copy_matches_expected() {
        assert_eq!(
            super::foreground_run_spinner_labels(true),
            ("Opening desktop app...", "Desktop app launch requested.")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reroute_auto_provisioned_execution_preserves_injected_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let original_manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(
            &original_manifest_path,
            r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
runtime_version = "20.11.0"
run = "node server.js""#,
        )
        .expect("write original manifest");
        let shadow_root = tmp
            .path()
            .join(".ato")
            .join("test-scratch")
            .join("ato-auto-provision")
            .join("run-1");
        std::fs::create_dir_all(&shadow_root).expect("shadow root");
        let shadow_manifest_path = shadow_root.join("capsule.toml");
        std::fs::write(
            &shadow_manifest_path,
            r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
runtime_version = "20.11.0"
working_dir = "workspace"
run = "node server.js""#,
        )
        .expect("write shadow manifest");

        let mut state_overrides = std::collections::HashMap::new();
        state_overrides.insert("data".to_string(), "/var/lib/demo".to_string());
        let decision = router::route_manifest_with_state_overrides_and_validation_mode(
            &original_manifest_path,
            ExecutionProfile::Dev,
            Some("app"),
            state_overrides.clone(),
            capsule_core::types::ValidationMode::Strict,
        )
        .expect("route original manifest");
        let mount = InjectedMount {
            source: tmp.path().join("db"),
            target: "/var/run/ato/injected/db".to_string(),
            readonly: true,
        };
        let prepared = crate::commands::run::run_phase::PreparedRunContext {
            authoritative_lock: None,
            lock_path: None,
            workspace_root: tmp.path().to_path_buf(),
            effective_state: None,
            execution_override: None,
            bridge_manifest: crate::application::pipeline::phases::run::DerivedBridgeManifest::new(
                toml::Value::Table(toml::map::Map::new()),
            ),
            validation_mode: capsule_core::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
        };
        for preview_mode in [false, true] {
            let launch_ctx = RuntimeLaunchContext::empty()
                .with_injected_env(
                    [("DATABASE_URL".to_string(), "sqlite://shadow.db".to_string())]
                        .into_iter()
                        .collect(),
                )
                .with_injected_mounts(vec![mount.clone()]);

            let (rerouted, rerouted_ctx, _rerouted_prepared) = reroute_auto_provisioned_execution(
                decision.clone(),
                launch_ctx,
                &prepared,
                Arc::new(CliReporter::new(false)),
                preview_mode,
                &shadow_manifest_path,
            )
            .await
            .expect("reroute");

            assert_eq!(rerouted.plan.manifest_path, shadow_manifest_path);
            assert_eq!(rerouted.plan.state_source_overrides, state_overrides);
            assert_eq!(
                rerouted_ctx
                    .injected_env()
                    .get("DATABASE_URL")
                    .map(String::as_str),
                Some("sqlite://shadow.db")
            );
            assert_eq!(rerouted_ctx.injected_mounts(), std::slice::from_ref(&mount));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalize_run_target_accepts_direct_canonical_lock_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_path = tmp.path().join("ato.lock.json");
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "node", "cmd": ["index.js"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "node", "cmd": ["index.js"]}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "node", "version": "20.11.0"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "default", "runtime": "source", "driver": "node", "entrypoint": "node", "cmd": ["index.js"], "compatible": true}
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete", "observed_lockfiles": []}),
        );
        ato_lock::write_pretty_to_path(&lock, &lock_path).expect("write lock");

        let args = RunArgs {
            target: lock_path.clone(),
            target_label: None,
            args: Vec::new(),
            watch: false,
            background: false,
            nacelle: None,
            registry: None,
            enforcement: "best_effort".to_string(),
            sandbox_mode: false,
            dangerously_skip_permissions: false,
            compatibility_fallback: None,
            provider_toolchain_requested: crate::ProviderToolchain::Auto,
            explicit_commit: None,
            assume_yes: true,
            verbose: false,
            agent_mode: crate::RunAgentMode::Off,
            agent_local_root: Some(tmp.path().to_path_buf()),
            keep_failed_artifacts: false,
            auto_fix_mode: None,
            allow_unverified: false,
            read_grants: Vec::new(),
            write_grants: Vec::new(),
            read_write_grants: Vec::new(),
            caller_cwd: tmp.path().to_path_buf(),
            effective_cwd: None,
            export_request: None,
            state_bindings: Vec::new(),
            inject_bindings: Vec::new(),
            build_policy: crate::application::build_materialization::BuildPolicy::IfStale,
            cache_strategy: crate::application::dependency_materializer::CacheStrategy::None,
            reporter: Arc::new(CliReporter::new(true)),
            preview_mode: false,
        };

        let resolved = crate::install::support::ResolvedRunTarget {
            path: lock_path.clone(),
            agent_local_root: Some(tmp.path().to_path_buf()),
            desktop_open_path: None,
            export_request: None,
            provider_workspace: None,
            transient_workspace_root: None,
        };

        let normalized = normalize_run_target_after_install(&args, &resolved, None)
            .await
            .expect("normalize target");

        assert!(normalized.authoritative_input.is_some());
        assert_eq!(normalized.target, tmp.path().canonicalize().unwrap());
        assert!(normalized.target.exists());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalize_provider_target_persists_authoritative_lock_in_workspace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace_root = tmp.path().join("provider-workspace");
        std::fs::create_dir_all(workspace_root.join(".ato").join("provider"))
            .expect("create provider metadata dir");
        std::fs::write(
            workspace_root.join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "demo-provider"
version = "0.1.0"
type = "job"

runtime = "source/python"
runtime_version = "3.11.10"
source_layout = "anchored_entrypoint"
run = "main.py""#,
        )
        .expect("write provider manifest");
        std::fs::write(workspace_root.join("main.py"), "print('ok')\n")
            .expect("write provider entrypoint");

        let resolution_metadata_path = workspace_root.join("resolution.json");
        std::fs::write(
            &resolution_metadata_path,
            serde_json::to_string_pretty(&json!({
                "provider": "pypi",
                "ref": "demo-provider",
                "resolution_role": "audit_provenance_only",
                "requested_provider_toolchain": "auto",
                "effective_provider_toolchain": "uv",
                "requested_package_name": "demo-provider",
                "requested_extras": [],
                "resolved_package_name": "demo-provider",
                "resolved_package_version": "0.1.0",
                "selected_entrypoint": "demo_provider.cli:main",
                "generated_capsule_root": workspace_root.display().to_string(),
                "generated_manifest_path": workspace_root.join("capsule.toml").display().to_string(),
                "generated_wrapper_path": workspace_root.join("main.py").display().to_string(),
                "index_source": "https://example.invalid/simple",
                "requested_runtime_version": "3.11.10",
                "effective_runtime_version": "3.11.10",
                "materialization_runtime_selector": "3.11.10"
            }))
            .expect("serialize provider resolution metadata")
                + "\n",
        )
        .expect("write provider resolution metadata");

        let args = RunArgs {
            target: workspace_root.clone(),
            target_label: None,
            args: Vec::new(),
            watch: false,
            background: false,
            nacelle: None,
            registry: None,
            enforcement: "best_effort".to_string(),
            sandbox_mode: false,
            dangerously_skip_permissions: false,
            compatibility_fallback: None,
            provider_toolchain_requested: crate::ProviderToolchain::Auto,
            explicit_commit: None,
            assume_yes: true,
            verbose: false,
            agent_mode: crate::RunAgentMode::Off,
            agent_local_root: Some(workspace_root.clone()),
            keep_failed_artifacts: false,
            auto_fix_mode: None,
            allow_unverified: false,
            read_grants: Vec::new(),
            write_grants: Vec::new(),
            read_write_grants: Vec::new(),
            caller_cwd: tmp.path().to_path_buf(),
            effective_cwd: None,
            export_request: None,
            state_bindings: Vec::new(),
            inject_bindings: Vec::new(),
            build_policy: crate::application::build_materialization::BuildPolicy::IfStale,
            cache_strategy: crate::application::dependency_materializer::CacheStrategy::None,
            reporter: Arc::new(CliReporter::new(true)),
            preview_mode: false,
        };

        let resolved = crate::install::support::ResolvedRunTarget {
            path: workspace_root.clone(),
            agent_local_root: Some(workspace_root.clone()),
            desktop_open_path: None,
            export_request: None,
            provider_workspace: Some(crate::install::provider_target::ProviderRunWorkspace {
                target: crate::install::provider_target::ProviderTargetRef {
                    provider: crate::install::provider_target::ProviderKind::PyPI,
                    ref_string: "demo-provider".to_string(),
                },
                workspace_root: workspace_root.clone(),
                resolution_metadata_path: resolution_metadata_path.clone(),
            }),
            transient_workspace_root: Some(workspace_root.clone()),
        };

        let normalized = normalize_run_target_after_install(&args, &resolved, None)
            .await
            .expect("normalize provider target");
        let authoritative = normalized
            .authoritative_input
            .as_ref()
            .expect("provider authoritative input");
        let persisted_lock_path = workspace_root.join("ato.lock.json");
        assert!(
            persisted_lock_path.exists(),
            "persisted provider ato.lock.json missing"
        );

        let persisted_lock = ato_lock::load_unvalidated_from_path(&persisted_lock_path)
            .expect("load persisted provider lock");
        assert!(persisted_lock.lock_id.is_some());
        assert_eq!(
            persisted_lock.schema_version,
            authoritative.lock.schema_version
        );
        assert_eq!(persisted_lock.resolution, authoritative.lock.resolution);
        assert_eq!(persisted_lock.contract, authoritative.lock.contract);
        assert_eq!(persisted_lock.binding, authoritative.lock.binding);
        assert_eq!(persisted_lock.policy, authoritative.lock.policy);
        assert_eq!(persisted_lock.attestations, authoritative.lock.attestations);

        let metadata: Value = serde_json::from_str(
            &std::fs::read_to_string(&resolution_metadata_path)
                .expect("read updated provider resolution metadata"),
        )
        .expect("parse updated provider resolution metadata");
        let persisted_lock_path_str = persisted_lock_path.display().to_string();
        assert_eq!(
            metadata["resolution_role"].as_str(),
            Some("audit_provenance_only")
        );
        assert_eq!(
            metadata["generated_authoritative_lock_path"].as_str(),
            Some(persisted_lock_path_str.as_str())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normalize_run_target_accepts_direct_appimage_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let appimage_path = tmp.path().join("dist").join("MyApp.AppImage");
        std::fs::create_dir_all(appimage_path.parent().expect("dist parent")).expect("create dist");
        std::fs::write(&appimage_path, "appimage").expect("write appimage");

        let args = RunArgs {
            target: appimage_path.clone(),
            target_label: None,
            args: Vec::new(),
            watch: false,
            background: false,
            nacelle: None,
            registry: None,
            enforcement: "best_effort".to_string(),
            sandbox_mode: false,
            dangerously_skip_permissions: false,
            compatibility_fallback: None,
            provider_toolchain_requested: crate::ProviderToolchain::Auto,
            explicit_commit: None,
            assume_yes: true,
            verbose: false,
            agent_mode: crate::RunAgentMode::Off,
            agent_local_root: Some(tmp.path().to_path_buf()),
            keep_failed_artifacts: false,
            auto_fix_mode: None,
            allow_unverified: false,
            read_grants: Vec::new(),
            write_grants: Vec::new(),
            read_write_grants: Vec::new(),
            caller_cwd: tmp.path().to_path_buf(),
            effective_cwd: None,
            export_request: None,
            state_bindings: Vec::new(),
            inject_bindings: Vec::new(),
            build_policy: crate::application::build_materialization::BuildPolicy::IfStale,
            cache_strategy: crate::application::dependency_materializer::CacheStrategy::None,
            reporter: Arc::new(CliReporter::new(true)),
            preview_mode: false,
        };

        let resolved = crate::install::support::ResolvedRunTarget {
            path: appimage_path.clone(),
            agent_local_root: Some(tmp.path().to_path_buf()),
            desktop_open_path: None,
            export_request: None,
            provider_workspace: None,
            transient_workspace_root: None,
        };

        let normalized = normalize_run_target_after_install(&args, &resolved, None)
            .await
            .expect("normalize target");
        let authoritative = normalized
            .authoritative_input
            .as_ref()
            .expect("authoritative input");
        let routed = capsule_core::router::route_lock(
            &authoritative.lock_path,
            &authoritative.lock,
            &authoritative.workspace_root,
            capsule_core::router::ExecutionProfile::Dev,
            None,
        )
        .expect("route lock");

        assert_eq!(normalized.target, tmp.path().join("dist"));
        assert_eq!(routed.plan.execution_driver().as_deref(), Some("native"));
        assert_eq!(routed.plan.selected_target_label(), "desktop");
        assert_eq!(
            routed.plan.execution_entrypoint().as_deref(),
            Some("MyApp.AppImage")
        );
    }

    #[test]
    fn run_phase_details_match_hourglass_consumer_flow() {
        assert_eq!(
            run_phase_detail(super::HourglassPhase::Prepare),
            "manifest and launch context resolution"
        );
        assert_eq!(
            run_phase_detail(super::HourglassPhase::Build),
            "build and lifecycle hooks"
        );
        assert_eq!(
            run_phase_detail(super::HourglassPhase::Verify),
            "execution plan verification"
        );
        assert_eq!(
            run_phase_detail(super::HourglassPhase::DryRun),
            "runtime preflight"
        );
        assert_eq!(
            run_phase_detail(super::HourglassPhase::Execute),
            "capsule execution"
        );
    }

    #[test]
    fn process_runtime_label_preserves_runtime_under_host_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::from_str(
                r#"
                schema_version = "0.3"
                name = "app"
                version = "0.1.0"
                type = "app"

                runtime = "source/node"
                run = "node server.js""#,
            )
            .expect("manifest"),
            tmp.path().join("capsule.toml"),
            tmp.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor");

        let label = process_runtime_label(&plan, false, CompatibilityHostMode::Enabled);
        assert_eq!(label, "source/node [host-fallback]");
    }

    #[test]
    fn resolve_state_source_overrides_registers_persistent_state_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(&tmp.path().join("state-store")).expect("open store");

        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

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
"#,
        )
        .unwrap();

        let bind_dir = tmp.path().join("bind").join("data");
        let overrides = resolve_state_source_overrides_with_store(
            &manifest,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
        .expect("state override");

        assert_eq!(
            overrides.get("data").map(|value| value.as_str()),
            Some(bind_dir.canonicalize().unwrap().to_string_lossy().as_ref())
        );
        assert!(tmp
            .path()
            .join("state-store")
            .join("registry.sqlite3")
            .exists());
    }

    #[test]
    fn resolve_state_source_overrides_accepts_state_id_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(&tmp.path().join("state-store")).expect("open store");

        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

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
"#,
        )
        .unwrap();

        let bind_dir = tmp.path().join("bind").join("data");
        let first = resolve_state_source_overrides_with_store(
            &manifest,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
        .expect("initial state registration");
        let record = store
            .list_persistent_states(Some("demo-app"), Some("data"))
            .expect("list states")
            .into_iter()
            .next()
            .expect("registered state");

        let second = resolve_state_source_overrides_with_store(
            &manifest,
            &[format!("data={}", record.state_id)],
            Some(&store),
        )
        .expect("state id bind");

        assert_eq!(first, second);
    }

    #[test]
    fn resolve_state_source_overrides_rejects_incompatible_registry_entry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(&tmp.path().join("state-store")).expect("open store");

        let manifest_a = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

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
"#,
        )
        .unwrap();
        let manifest_b = CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "secondary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        )
        .unwrap();

        let bind_dir = tmp.path().join("bind").join("data");
        resolve_state_source_overrides_with_store(
            &manifest_a,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
        .expect("first bind");

        let err = resolve_state_source_overrides_with_store(
            &manifest_b,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
        .expect_err("incompatible bind must fail");
        assert!(err
            .to_string()
            .contains("producer/purpose/schema_id must match exactly"));
    }

    fn manifest_with_required_env(keys: Vec<&str>) -> ManifestData {
        manifest_with_schema_and_target(
            "0.2",
            PathBuf::from("/tmp"),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("native".to_string())),
                ("entrypoint", toml::Value::String("main.py".to_string())),
                (
                    "required_env",
                    toml::Value::Array(
                        keys.into_iter()
                            .map(|k| toml::Value::String(k.to_string()))
                            .collect(),
                    ),
                ),
            ],
        )
    }

    fn manifest_with_schema_and_target(
        schema_version: &str,
        manifest_dir: PathBuf,
        entries: Vec<(&str, toml::Value)>,
    ) -> ManifestData {
        let mut manifest = toml::map::Map::new();
        manifest.insert(
            "schema_version".to_string(),
            toml::Value::String(schema_version.to_string()),
        );
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );
        manifest.insert("type".to_string(), toml::Value::String("app".to_string()));

        let mut target = toml::map::Map::new();
        for (key, value) in entries {
            target.insert(key.to_string(), value);
        }

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::Value::Table(manifest),
            manifest_dir.join("capsule.toml"),
            manifest_dir,
            ExecutionProfile::Dev,
            Some("default"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor")
    }
}
