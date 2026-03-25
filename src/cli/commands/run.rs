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
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};
use tracing::debug;

use crate::application::pipeline::cleanup::PipelineAttemptContext;
use crate::application::pipeline::consumer::ConsumerRunPipeline;
use crate::application::pipeline::executor::HourglassPhaseRunner;
use crate::application::pipeline::hourglass;
use crate::application::pipeline::hourglass::{HourglassPhase, HourglassPhaseState};
use crate::application::pipeline::phases::run as run_phase;
use crate::application::ports::OutputPort;
use crate::application::source_inference;
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
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
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

const BACKGROUND_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const BACKGROUND_READY_WAIT_TIMEOUT_ENV: &str = "ATO_BACKGROUND_READY_WAIT_TIMEOUT_SECS";

type RunPipelineState = run_phase::RunPipelineState;

pub struct RunArgs {
    pub target: PathBuf,
    pub target_label: Option<String>,
    pub watch: bool,
    pub background: bool,
    pub nacelle: Option<PathBuf>,
    pub registry: Option<String>,
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub compatibility_fallback: Option<String>,
    pub assume_yes: bool,
    pub agent_mode: crate::RunAgentMode,
    pub agent_local_root: Option<PathBuf>,
    pub keep_failed_artifacts: bool,
    pub auto_fix_mode: Option<crate::GitHubAutoFixMode>,
    pub allow_unverified: bool,
    pub state_bindings: Vec<String>,
    pub inject_bindings: Vec<String>,
    pub reporter: Arc<CliReporter>,
    pub preview_mode: bool,
}

pub async fn execute(args: RunArgs) -> Result<()> {
    if args.watch {
        execute_watch_mode_with_install(args).await
    } else {
        execute_normal_mode(args).await
    }
}

async fn execute_watch_mode_with_install(args: RunArgs) -> Result<()> {
    let install = run_install_phase(&args).await?;
    if matches!(
        install.manifest_outcome,
        crate::install::support::LocalRunManifestPreparationOutcome::CreatedManualManifest
    ) {
        return Ok(());
    }

    let normalized =
        normalize_run_target_after_install(&args, &install.resolved_target.path, None).await?;
    execute_watch_mode(RunArgs {
        target: normalized.target,
        agent_local_root: install.resolved_target.agent_local_root,
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

    let cas_provider = capsule_core::capsule_v3::CasProvider::from_env();
    let payload_outcome = capsule_core::capsule_v3::unpack_payload_from_capsule_root_with_provider(
        &extract_dir,
        &extract_dir,
        &cas_provider,
    )
    .with_context(|| "Failed to extract payload from capsule root (v2/v3)")?;
    match payload_outcome {
        capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV3
        | capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV2 => {}
        capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV2DueToCasDisabled(reason) => {
            emit_run_cas_disabled_warning_once(&reason);
        }
        capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV2DueToV3Error(err) => {
            emit_run_v3_fallback_warning_once(&err);
        }
    }
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

fn emit_run_cas_disabled_warning_once(reason: &capsule_core::capsule_v3::CasDisableReason) {
    static STDERR_WARN_ONCE: Once = Once::new();
    STDERR_WARN_ONCE.call_once(|| {
        eprintln!(
            "⚠️  Performance warning: CAS is disabled (reason: {}). Falling back to v2 legacy mode.",
            reason
        );
    });
}

fn emit_run_v3_fallback_warning_once(error_message: &str) {
    static STDERR_WARN_ONCE: Once = Once::new();
    STDERR_WARN_ONCE.call_once(|| {
        eprintln!(
            "⚠️  Performance warning: v3 payload reconstruction failed ({}). Falling back to v2 legacy mode.",
            error_message
        );
    });
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

fn build_consumer_run_request(args: &RunArgs) -> run_phase::ConsumerRunRequest {
    run_phase::ConsumerRunRequest {
        target: args.target.clone(),
        target_label: args.target_label.clone(),
        authoritative_input: None,
        background: args.background,
        nacelle: args.nacelle.clone(),
        enforcement: args.enforcement.clone(),
        sandbox_mode: args.sandbox_mode,
        dangerously_skip_permissions: args.dangerously_skip_permissions,
        compatibility_fallback: args.compatibility_fallback.clone(),
        assume_yes: args.assume_yes,
        agent_mode: args.agent_mode,
        agent_local_root: args.agent_local_root.clone(),
        registry: args.registry.clone(),
        keep_failed_artifacts: args.keep_failed_artifacts,
        auto_fix_mode: args.auto_fix_mode,
        allow_unverified: args.allow_unverified,
        state_bindings: args.state_bindings.clone(),
        inject_bindings: args.inject_bindings.clone(),
        reporter: args.reporter.clone(),
        preview_mode: args.preview_mode,
    }
}

fn build_consumer_run_request_with_target(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
) -> run_phase::ConsumerRunRequest {
    let mut request = build_consumer_run_request(args);
    request.target = target.to_path_buf();
    request.agent_local_root = agent_local_root;
    request.authoritative_input = authoritative_input;
    request
}

#[derive(Debug, Clone)]
struct NormalizedRunTarget {
    target: PathBuf,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
}

fn authoritative_kind_from_materialization(
    input_kind: source_inference::SourceInferenceInputKind,
) -> run_phase::RunAuthoritativeInputKind {
    match input_kind {
        source_inference::SourceInferenceInputKind::CanonicalLock => {
            run_phase::RunAuthoritativeInputKind::CanonicalLock
        }
        source_inference::SourceInferenceInputKind::DraftLock => {
            run_phase::RunAuthoritativeInputKind::CompatibilityCompiledDraft
        }
        source_inference::SourceInferenceInputKind::SourceEvidence => {
            run_phase::RunAuthoritativeInputKind::SourceOnly
        }
    }
}

fn authoritative_input_from_materialization(
    materialized: source_inference::RunMaterialization,
    compatibility_legacy_lock: Option<run_phase::CompatibilityLegacyLockContext>,
) -> run_phase::RunAuthoritativeInput {
    run_phase::RunAuthoritativeInput {
        kind: authoritative_kind_from_materialization(materialized.input_kind),
        lock: materialized.lock,
        lock_path: materialized.lock_path,
        sidecar_path: materialized.sidecar_path,
        bridge_manifest_path: materialized.manifest_path,
        bridge_manifest_sha256: materialized.bridge_manifest_sha256,
        compatibility_legacy_lock,
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
        reporter: &Arc<CliReporter>,
    ) -> Result<PathBuf> {
        preflight_native_sandbox(nacelle_override, plan, prepared, reporter)
    }

    async fn complete_background_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        plan: &capsule_core::router::ManifestData,
        runtime: String,
        scoped_id: Option<String>,
        ready_without_events: bool,
        compatibility_host_mode: CompatibilityHostMode,
        reporter: &Arc<CliReporter>,
    ) -> Result<()> {
        complete_background_source_process(
            process,
            plan,
            runtime,
            scoped_id,
            ready_without_events,
            compatibility_host_mode,
            reporter,
        )
        .await
    }

    async fn complete_foreground_source_process(
        &self,
        process: crate::executors::source::CapsuleProcess,
        reporter: Arc<CliReporter>,
        sandbox_initialized: bool,
        ipc_socket_mapped: bool,
        use_progressive_ui: bool,
    ) -> Result<i32> {
        complete_foreground_source_process(
            process,
            reporter,
            sandbox_initialized,
            ipc_socket_mapped,
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
    agent_local_root: Option<PathBuf>,
    should_stop_after_install: bool,
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
}

#[async_trait(?Send)]
impl HourglassPhaseRunner for ConsumerRunPhaseRunner<'_> {
    fn should_continue(&self) -> bool {
        !self.should_stop_after_install
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
                let normalized = normalize_run_target_after_install(
                    self.args,
                    &install.resolved_target.path,
                    Some(attempt),
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Install, err);
                })?;
                self.target = Some(normalized.target);
                self.authoritative_input = normalized.authoritative_input;
                self.agent_local_root = install.resolved_target.agent_local_root;
                self.should_stop_after_install = matches!(
                    install.manifest_outcome,
                    crate::install::support::LocalRunManifestPreparationOutcome::CreatedManualManifest
                );
                Ok(())
            }
            HourglassPhase::Prepare => {
                let state = run_prepare_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    Some(attempt),
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Prepare, err);
                })?;
                self.state = Some(state);
                Ok(())
            }
            HourglassPhase::Build => {
                let input = self.take_state(HourglassPhase::Build)?;
                let state = run_build_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    input,
                )
                .await
                .inspect_err(|err| {
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
                    input,
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Verify, err);
                })?;
                self.state = Some(state);
                Ok(())
            }
            HourglassPhase::DryRun => {
                let input = self.take_state(HourglassPhase::DryRun)?;
                let state = run_dry_run_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    input,
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::DryRun, err);
                })?;
                self.state = Some(state);
                Ok(())
            }
            HourglassPhase::Execute => {
                let input = self.take_state(HourglassPhase::Execute)?;
                run_execute_phase(
                    self.args,
                    self.resolved_target(),
                    self.agent_local_root.clone(),
                    self.authoritative_input.clone(),
                    input,
                    Some(attempt),
                )
                .await
                .inspect_err(|err| {
                    emit_run_phase_failure(self.args, HourglassPhase::Execute, err);
                })
            }
            HourglassPhase::Publish => anyhow::bail!(
                "unsupported run pipeline phase {} in run command",
                phase.as_str()
            ),
        }
    }
}

async fn execute_normal_mode(args: RunArgs) -> Result<()> {
    let pipeline = ConsumerRunPipeline::standard();
    let mut runner = ConsumerRunPhaseRunner {
        args: &args,
        state: None,
        target: None,
        authoritative_input: None,
        agent_local_root: args.agent_local_root.clone(),
        should_stop_after_install: false,
    };

    pipeline.run(&mut runner).await
}

fn run_phase_detail(boundary: HourglassPhase) -> &'static str {
    match boundary {
        HourglassPhase::Install => "target resolution and install",
        HourglassPhase::Prepare => "manifest and launch context resolution",
        HourglassPhase::Build => "build and lifecycle hooks",
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
    hourglass::print_phase_line(args.reporter.is_json(), boundary, state, detail);
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
    resolved_target: &Path,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<NormalizedRunTarget> {
    if resolved_target
        .extension()
        .map(|value| value.eq_ignore_ascii_case("capsule"))
        .unwrap_or(false)
    {
        let target = prepare_capsule_target(args, &resolved_target.to_path_buf(), attempt).await?;
        return Ok(NormalizedRunTarget {
            target,
            authoritative_input: None,
        });
    }

    if resolved_target.is_dir()
        || resolved_target.file_name().and_then(|value| value.to_str()) == Some("capsule.toml")
    {
        return match resolve_authoritative_input(resolved_target, ResolveInputOptions::default())? {
            ResolvedInput::CanonicalLock { canonical, .. } => {
                let mut cleanup_scope = attempt.map(|attempt| attempt.cleanup_scope());
                let materialized = source_inference::materialize_run_from_canonical_lock(
                    &canonical,
                    cleanup_scope.as_mut(),
                    args.reporter.clone(),
                    args.assume_yes,
                )?;
                let target = materialized.manifest_path.clone();
                Ok(NormalizedRunTarget {
                    target,
                    authoritative_input: Some(authoritative_input_from_materialization(
                        materialized,
                        None,
                    )),
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
                let target = materialized.manifest_path.clone();
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
                        compatibility_legacy_lock,
                    )),
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
                let target = materialized.manifest_path.clone();
                Ok(NormalizedRunTarget {
                    target,
                    authoritative_input: Some(authoritative_input_from_materialization(
                        materialized,
                        None,
                    )),
                })
            }
        };
    }

    Ok(NormalizedRunTarget {
        target: resolved_target.to_path_buf(),
        authoritative_input: None,
    })
}

async fn run_install_phase(args: &RunArgs) -> Result<run_phase::RunInstallPhaseResult> {
    let request = build_consumer_run_request(args);
    let progress = RunProgress { args };
    run_phase::run_install_phase(&request, &progress).await
}

async fn run_prepare_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<RunPipelineState> {
    let request =
        build_consumer_run_request_with_target(args, target, agent_local_root, authoritative_input);
    let progress = RunProgress { args };
    run_phase::run_prepare_phase(&request, &progress, attempt).await
}

async fn run_build_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    state: RunPipelineState,
) -> Result<RunPipelineState> {
    let request =
        build_consumer_run_request_with_target(args, target, agent_local_root, authoritative_input);
    let progress = RunProgress { args };
    run_phase::run_build_phase(&request, &progress, state).await
}

async fn run_verify_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    state: RunPipelineState,
) -> Result<RunPipelineState> {
    let request =
        build_consumer_run_request_with_target(args, target, agent_local_root, authoritative_input);
    let progress = RunProgress { args };
    run_phase::run_verify_phase(&request, &progress, state).await
}

async fn run_dry_run_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    state: RunPipelineState,
) -> Result<RunPipelineState> {
    let request =
        build_consumer_run_request_with_target(args, target, agent_local_root, authoritative_input);
    let progress = RunProgress { args };
    run_phase::run_dry_run_phase(&request, &progress, state).await
}

async fn run_execute_phase(
    args: &RunArgs,
    target: &Path,
    agent_local_root: Option<PathBuf>,
    authoritative_input: Option<run_phase::RunAuthoritativeInput>,
    state: RunPipelineState,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<()> {
    let request =
        build_consumer_run_request_with_target(args, target, agent_local_root, authoritative_input);
    let progress = RunProgress { args };
    run_phase::run_execute_phase(&request, &progress, state, attempt, &RunExecuteHooks).await
}

#[cfg(test)]
async fn reroute_auto_provisioned_execution(
    decision: capsule_core::router::RuntimeDecision,
    launch_ctx: crate::executors::launch_context::RuntimeLaunchContext,
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
    let preview_mode =
        args.preview_mode || preview::load_preview_session_for_manifest(&manifest_path)?.is_some();
    let manifest = if preview_mode {
        capsule_core::manifest::load_manifest_with_validation_mode(
            &manifest_path,
            capsule_core::types::ValidationMode::Preview,
        )?
        .model
    } else {
        CapsuleManifest::load_from_file(&manifest_path)?
    };
    let state_source_overrides = resolve_state_source_overrides(&manifest, &args.state_bindings)?;
    let decision = capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
        &manifest_path,
        router::ExecutionProfile::Dev,
        args.target_label.as_deref(),
        state_source_overrides,
        if preview_mode {
            capsule_core::types::ValidationMode::Preview
        } else {
            capsule_core::types::ValidationMode::Strict
        },
    )?;
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
        initial_foreground_native_messages, plan_v03_provision_command,
        preflight_required_environment_variables, process_runtime_label,
        reroute_auto_provisioned_execution, resolve_compatibility_host_mode,
        resolve_python_dependency_lock_path, resolve_state_source_overrides_with_store,
        run_phase_detail, CompatibilityHostMode, ForegroundEventMessage,
    };
    use crate::executors::launch_context::{InjectedMount, RuntimeLaunchContext};
    use crate::registry::store::RegistryStore;
    use crate::reporters::CliReporter;
    use capsule_core::execution_plan::guard::ExecutorKind;
    use capsule_core::lifecycle::LifecycleEvent;
    use capsule_core::router::{self, ExecutionProfile, ManifestData};
    use capsule_core::types::CapsuleManifest;
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
        assert_eq!(command.as_deref(), Some("pnpm install --frozen-lockfile"));
    }

    #[test]
    fn v03_node_provision_rejects_ambiguous_lockfiles() {
        let tmp = tempfile::tempdir().expect("tempdir");
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

        let err = plan_v03_provision_command(&plan).expect_err("must reject ambiguity");
        assert!(err.to_string().contains("multiple node lockfiles detected"));
    }

    #[test]
    fn v03_node_provision_uses_target_working_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("apps").join("web");
        std::fs::create_dir_all(&app_dir).expect("create app dir");
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
        assert_eq!(command.as_deref(), Some("pnpm install --frozen-lockfile"));
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
        );

        assert_eq!(
            message,
            vec![ForegroundEventMessage::Warn(
                "❌ Service 'main' exited before readiness (exit code: 42)".to_string()
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
        let message = background_ready_message("capsule-42", CompatibilityHostMode::Enabled);
        assert_eq!(
            message,
            "✔ Capsule is ready (Host Fallback, ID: capsule-42)"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reroute_auto_provisioned_execution_preserves_injected_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let original_manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(
            &original_manifest_path,
            r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
runtime_version = "20.11.0"
run_command = "node server.js"
"#,
        )
        .expect("write original manifest");
        let shadow_root = tmp
            .path()
            .join(".tmp")
            .join("ato-auto-provision")
            .join("run-1");
        std::fs::create_dir_all(&shadow_root).expect("shadow root");
        let shadow_manifest_path = shadow_root.join("capsule.toml");
        std::fs::write(
            &shadow_manifest_path,
            r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
runtime_version = "20.11.0"
working_dir = "workspace"
run_command = "node server.js"
"#,
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
        let plan = ManifestData {
            manifest: toml::from_str(
                r#"
                [targets.app]
                runtime = "source"
                driver = "node"
                run_command = "node server.js"
                "#,
            )
            .expect("manifest"),
            manifest_path: tmp.path().join("capsule.toml"),
            manifest_dir: tmp.path().to_path_buf(),
            profile: ExecutionProfile::Dev,
            selected_target: "app".to_string(),
            state_source_overrides: std::collections::HashMap::new(),
        };

        let label = process_runtime_label(&plan, false, CompatibilityHostMode::Enabled);
        assert_eq!(label, "source/node [host-fallback]");
    }

    #[test]
    fn resolve_state_source_overrides_registers_persistent_state_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(&tmp.path().join("state-store")).expect("open store");

        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
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

        let mut target = toml::map::Map::new();
        for (key, value) in entries {
            target.insert(key.to_string(), value);
        }

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        ManifestData {
            manifest: toml::Value::Table(manifest),
            manifest_path: manifest_dir.join("capsule.toml"),
            manifest_dir,
            profile: ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: std::collections::HashMap::new(),
        }
    }
}
