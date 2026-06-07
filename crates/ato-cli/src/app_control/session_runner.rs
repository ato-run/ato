//! `SessionStartPhaseRunner` — drives `ato app session start` through the
//! same Hourglass pipeline that `ato run` uses, so the build phase observes
//! the same materialization layer (RFC: BUILD_MATERIALIZATION).
//!
//! Phase responsibilities for v0:
//!
//! | Phase    | Behavior                                                             |
//! |----------|----------------------------------------------------------------------|
//! | Install  | Session-side handle resolution + env preflight                        |
//! | Prepare  | No-op (resolution already done in Install)                           |
//! | Build    | Same materialization helpers as `run_build_phase`                     |
//! | Verify   | No-op for v0 (consent / sandbox checks deferred)                     |
//! | DryRun   | No-op for v0                                                          |
//! | Execute  | Spawn guest / runtime session, register ProcessManager, wait ready   |
//!
//! Verify and DryRun are intentionally no-op for v0 to keep the change
//! focused on closing the build-skip gap. They will be filled in once the
//! desktop has a UX for consent prompts and sandbox preflight.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use ato_session_core::{
    MaterializedLaunchRecord, MaterializedLaunchValidationOutcome,
    validate_materialized_launch_record,
};
use capsule_core::launch_spec::LaunchSpec;
use capsule_core::router::ManifestData;

use crate::application::build_materialization as bm;
use crate::application::execution_receipt_builder;
use crate::application::execution_receipts;
use crate::application::launch_materialization as lm;
use crate::application::pipeline::cleanup::PipelineAttemptContext;
use crate::application::pipeline::executor::{
    HourglassPhaseRunner, PhaseAnnotation, PhaseStageTimer,
};
use crate::application::pipeline::hourglass::HourglassPhase;
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::executors::target_runner;
use crate::reporters::CliReporter;

use super::guest_contract::parse_guest_contract;
use super::resolve::HandleResolution;
use super::session::{
    SessionInfo, redirect_stdout_to_stderr, resolve_local_plan_for_session_start,
    resolve_session_launch_plan, restore_stdout, start_guest_session,
    start_orchestration_session_in_process, start_orchestration_session_supervisor,
    start_runtime_session,
};

/// Env var fence for the legacy opaque orchestration supervisor (#73 PR-C).
///
/// When `ATO_LEGACY_SUPERVISOR=1` the wrapper falls back to spawning a
/// nested `ato run` subprocess (the pre-v0.5.0 behavior). The normal path
/// uses `start_orchestration_session_in_process` which materializes the
/// orchestration graph in-process via `executors::orchestrator`'s detach
/// API. The fence exists only as an emergency escape if the in-process path
/// regresses against a real-world capsule; it is not part of the supported
/// surface and is removed in v0.5.x once the regression matrix is in place.
pub(crate) const LEGACY_SUPERVISOR_ENV: &str = "ATO_LEGACY_SUPERVISOR";

pub(crate) fn legacy_supervisor_enabled() -> bool {
    legacy_supervisor_enabled_for_value(std::env::var(LEGACY_SUPERVISOR_ENV).ok().as_deref())
}

/// Pure-logic helper extracted so tests can verify the gate without
/// mutating process-global env (which races other env-touching tests in
/// the crate). Only `Some("1")` flips the gate; `None`, `Some("true")`,
/// `Some("yes")`, etc. all keep the in-process path.
pub(crate) fn legacy_supervisor_enabled_for_value(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

fn try_remote_build_output_projection(
    plan: &ManifestData,
    workspace_root: &Path,
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

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum SessionStartSource {
    Handle,
    MaterializedRecord(MaterializedLaunchRecord),
    /// Start from an explicit capsule.toml file (e.g. fetched from the community API).
    /// The manifest_path field on the runner already holds the file path;
    /// run_install resolves the plan directly from it instead of going through
    /// network resolution.
    TomlPath,
}

pub(super) struct SessionStartPhaseRunner {
    handle: String,
    target_label: Option<String>,
    json: bool,
    start_source: SessionStartSource,
    expected_run_config_hash: Option<String>,
    attach_state: Vec<String>,

    // Set by Install phase
    resolution: Option<HandleResolution>,
    manifest_path: Option<PathBuf>,
    plan: Option<ManifestData>,
    launch: Option<LaunchSpec>,
    raw_manifest: Option<String>,
    manifest_value: Option<toml::Value>,
    notes: Vec<String>,
    launch_ctx: RuntimeLaunchContext,

    // Set by Install phase (warm-launch fast path).
    /// `true` when the Install phase hit a live session reuse and populated
    /// `session_info` directly. Build and Execute phases are no-ops.
    install_reused: bool,
    /// LaunchSpec computed from identity before source projection runs.
    /// Held here so Execute can use the same spec without recomputing it.
    pre_projection_spec: Option<lm::LaunchSpec>,
    /// Advisory per-slot file lock acquired during Install and held until
    /// the runner drops (i.e. across Build and Execute).
    _launch_lock: Option<lm::LaunchLock>,

    // Set by Build phase
    build_observation: Option<bm::BuildObservation>,
    build_decision_kind: Option<bm::BuildResultKind>,
    // `orchestration_supervisor_mode` was removed in #73 PR-C. The Build
    // phase now runs unconditionally (the in-process orchestration path
    // uses the same Build helpers as single-target session start), and the
    // Execute phase decides between in-process and legacy fallback by
    // checking `plan.is_orchestration_mode()` and the
    // `ATO_LEGACY_SUPERVISOR=1` env at the spawn point.

    // Set by Execute phase (App Session Materialization).
    /// `true` when Execute returned an envelope by reusing an existing
    /// ready session (no spawn). Drives `result_kind=materialized-session`
    /// in `phase_annotation`.
    execute_reused: bool,
    /// Reason the existing record was rejected, if Execute fell through to
    /// spawn after observing a stale candidate. Surfaced as the
    /// `prior_kind` extra on PHASE-TIMING.
    execute_prior_kind: Option<lm::PriorKind>,

    // Set by Execute phase. Read by `start_session` after `pipeline.run`.
    pub(super) session_info: Option<SessionInfo>,

    /// PR-3b boundary plumbing: handle to the
    /// `ReceiptEmissionContext::graph_id_sink` for this launch. Set
    /// by the outer wrapper in
    /// `app_control::session::start_session_for_capsule` before
    /// `pipeline.run` so `emit_execution_receipt` can publish
    /// declared/resolved ids to the boundary the moment the
    /// LaunchGraphBundle is built.
    ///
    /// Note: PR-3b deliberately does NOT carry the full
    /// `LaunchGraphBundle` on the runner. The bundle is owned by
    /// `emit_execution_receipt` and lives only in that method's
    /// local scope; the sink is the only handle that survives the
    /// boundary. Session-record enrichment reads declared/resolved
    /// ids from `ExecutionReceiptSessionMetadata` (built from the
    /// receipt document), which is the same id space the bundle
    /// stamped on the document — so no separate carrier is needed.
    pub(super) receipt_graph_id_sink:
        Option<crate::application::receipt_boundary::ReceiptGraphIdSink>,
}

impl SessionStartPhaseRunner {
    pub(super) fn new(
        handle: &str,
        target_label: Option<&str>,
        attach_state: Vec<String>,
        expected_run_config_hash: Option<String>,
        json: bool,
    ) -> Self {
        Self {
            handle: handle.to_string(),
            target_label: target_label.map(str::to_string),
            json,
            start_source: SessionStartSource::Handle,
            expected_run_config_hash,
            attach_state,
            resolution: None,
            manifest_path: None,
            plan: None,
            launch: None,
            raw_manifest: None,
            manifest_value: None,
            notes: Vec::new(),
            launch_ctx: RuntimeLaunchContext::empty(),
            install_reused: false,
            pre_projection_spec: None,
            _launch_lock: None,
            build_observation: None,
            build_decision_kind: None,
            execute_reused: false,
            execute_prior_kind: None,
            session_info: None,
            receipt_graph_id_sink: None,
        }
    }

    pub(super) fn from_materialized_record(
        record: MaterializedLaunchRecord,
        expected_run_config_hash: Option<String>,
        attach_state: Vec<String>,
        json: bool,
    ) -> Self {
        Self {
            handle: record.handle.clone(),
            target_label: Some(record.target_label.clone()),
            json,
            start_source: SessionStartSource::MaterializedRecord(record),
            expected_run_config_hash,
            attach_state,
            resolution: None,
            manifest_path: None,
            plan: None,
            launch: None,
            raw_manifest: None,
            manifest_value: None,
            notes: Vec::new(),
            launch_ctx: RuntimeLaunchContext::empty(),
            install_reused: false,
            pre_projection_spec: None,
            _launch_lock: None,
            build_observation: None,
            build_decision_kind: None,
            execute_reused: false,
            execute_prior_kind: None,
            session_info: None,
            receipt_graph_id_sink: None,
        }
    }

    /// Construct a runner that starts a session from an explicit capsule.toml
    /// file (e.g. one fetched from the community API). Unlike `new`, the
    /// install phase skips network resolution and resolves the plan directly
    /// from `manifest_path`.
    pub(super) fn from_toml_path(
        handle: &str,
        manifest_path: PathBuf,
        target_label: Option<&str>,
        attach_state: Vec<String>,
        expected_run_config_hash: Option<String>,
        json: bool,
    ) -> Self {
        let mut runner = Self::new(
            handle,
            target_label,
            attach_state,
            expected_run_config_hash,
            json,
        );
        runner.manifest_path = Some(manifest_path);
        runner.start_source = SessionStartSource::TomlPath;
        runner
    }

    /// `true` when the consumer manifest declares a top-level `[services]`
    /// graph and no explicit target was selected. Computed from `self.plan`
    /// after `run_install`. Replaces the old `orchestration_supervisor_mode`
    /// field — Build no longer skips on this and Execute uses it only to
    /// dispatch between `start_orchestration_session_in_process` (default)
    /// and `start_orchestration_session_supervisor`
    /// (legacy, `ATO_LEGACY_SUPERVISOR=1`).
    fn is_orchestration_session(&self) -> bool {
        self.target_label.is_none()
            && self
                .plan
                .as_ref()
                .map(|plan| plan.is_orchestration_mode())
                .unwrap_or(false)
    }

    fn install_from_materialized_record(
        &mut self,
        record: &MaterializedLaunchRecord,
    ) -> Result<()> {
        match validate_materialized_launch_record(record)? {
            MaterializedLaunchValidationOutcome::Valid => {}
            MaterializedLaunchValidationOutcome::Stale { reason } => {
                anyhow::bail!(
                    "materialized launch record {} is stale: {}",
                    record
                        .last_session_id
                        .as_deref()
                        .unwrap_or(&record.launch_key),
                    reason.as_str()
                );
            }
        }

        let manifest_path = PathBuf::from(&record.manifest_path);
        if record.handle != self.handle {
            anyhow::bail!(
                "materialized launch record belongs to '{}' not '{}'",
                record.handle,
                self.handle
            );
        }
        let expected_target = self.target_label.as_deref().unwrap_or(&record.target_label);
        if record.target_label != expected_target {
            anyhow::bail!(
                "materialized launch record target '{}' does not match '{}'",
                record.target_label,
                expected_target
            );
        }
        let expected_run_config_hash = self
            .expected_run_config_hash
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("materialized relaunch requires --run-config-hash"))?;
        if record.run_config_hash != expected_run_config_hash {
            anyhow::bail!("materialized launch record is stale: run config changed");
        }
        if record.platform != ato_session_core::current_platform_tag() {
            anyhow::bail!(
                "materialized launch record is stale: platform changed from {} to {}",
                record.platform,
                ato_session_core::current_platform_tag()
            );
        }
        let manifest_path_str = manifest_path.to_string_lossy().to_string();
        let mut resolution = super::resolve::build_resolution(
            &manifest_path_str,
            Some(record.target_label.as_str()),
            None,
        )?;
        resolution.input = record.handle.clone();
        resolution.normalized_handle = record.normalized_handle.clone();
        resolution.canonical_handle = record.canonical_handle.clone();
        resolution.source = record.source.clone();
        resolution.trust_state = record.trust_state.clone();
        resolution.restricted = record.restricted;
        resolution.snapshot = record.snapshot.clone();

        let sample_recipe_slug = record
            .source
            .as_deref()
            .filter(|source| *source == "sample_recipe")
            .and_then(|_| manifest_path.parent())
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str());
        let (plan, _guest, mut notes) = resolve_local_plan_for_session_start(
            &manifest_path,
            Some(record.target_label.as_str()),
            sample_recipe_slug,
            &self.attach_state,
        )?;
        let expected_app_root = PathBuf::from(&record.app_root)
            .canonicalize()
            .with_context(|| format!("failed to resolve app root {}", record.app_root))?;
        let actual_app_root = plan.workspace_root.canonicalize().with_context(|| {
            format!(
                "failed to resolve workspace root {}",
                plan.workspace_root.display()
            )
        })?;
        if actual_app_root != expected_app_root {
            anyhow::bail!(
                "materialized launch record {} is stale: workspace root changed from {} to {}",
                record
                    .last_session_id
                    .as_deref()
                    .unwrap_or(&record.launch_key),
                expected_app_root.display(),
                actual_app_root.display()
            );
        }

        let launch = capsule_core::launch_spec::derive_launch_spec(&plan).with_context(|| {
            format!(
                "failed to derive launch spec for materialized manifest {}",
                manifest_path.display()
            )
        })?;
        let raw_manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest_value: toml::Value = toml::from_str(&raw_manifest)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        let launch_spec = lm::canonicalize_launch_spec(
            &record.handle,
            &record.target_label,
            &plan,
            &launch,
            &manifest_path,
            None,
        )?;
        let launch_digest = lm::compute_launch_digest(&launch_spec);
        if launch_digest != record.launch_digest {
            anyhow::bail!(
                "materialized launch record {} is stale: launch digest changed",
                record
                    .last_session_id
                    .as_deref()
                    .unwrap_or(&record.launch_key)
            );
        }

        let launch_key = lm::compute_launch_key(&launch_spec);
        if record.launch_key != launch_key {
            anyhow::bail!("materialized launch record is stale: launch key changed");
        }
        self._launch_lock = lm::acquire_launch_lock(&launch_key).ok();
        self.pre_projection_spec = Some(launch_spec);
        notes.push(format!(
            "Relaunched from materialized launch record {}; skipped resolve/install.",
            record
                .last_session_id
                .as_deref()
                .unwrap_or(&record.launch_key)
        ));
        self.resolution = Some(resolution);
        self.manifest_path = Some(manifest_path);
        self.plan = Some(plan);
        self.launch = Some(launch);
        self.raw_manifest = Some(raw_manifest);
        self.manifest_value = Some(manifest_value);
        self.notes = notes;
        Ok(())
    }

    /// Install phase for community-TOML-path launches. The manifest file has
    /// already been fetched and written to `self.manifest_path` by the caller;
    /// we resolve the plan directly from it without network resolution.
    fn install_from_toml_path(&mut self) -> Result<()> {
        let manifest_path = self
            .manifest_path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("TomlPath start source requires manifest_path"))?;

        let manifest_path_str = manifest_path.to_string_lossy().to_string();
        let resolution = super::resolve::build_resolution(
            &manifest_path_str,
            self.target_label.as_deref(),
            None,
        )?;
        let (plan, _guest, notes) = resolve_local_plan_for_session_start(
            &manifest_path,
            self.target_label.as_deref(),
            None,
            &self.attach_state,
        )?;

        // Run preflight so missing secrets surface via the usual E103 path.
        let is_orchestration = self.target_label.is_none() && plan.is_orchestration_mode();
        if is_orchestration {
            let orchestration = plan
                .resolve_services()
                .context("failed to resolve [services] orchestration plan")?;
            let manifest_preflight: toml::Value = toml::from_str(
                &std::fs::read_to_string(&manifest_path)
                    .with_context(|| format!("failed to read {}", manifest_path.display()))?,
            )
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
            crate::application::pipeline::phases::run::preflight_orchestration_session_environment(
                &plan,
                &manifest_preflight,
                &orchestration,
                &self.launch_ctx,
                &crate::application::dependency_credentials::ProcessHostEnv,
                "launching the session",
            )?;
        } else {
            target_runner::preflight_required_environment_variables(&plan, &self.launch_ctx)?;
        }

        let launch = capsule_core::launch_spec::derive_launch_spec(&plan).with_context(|| {
            format!(
                "failed to derive launch spec for community manifest {}",
                manifest_path.display()
            )
        })?;
        let raw_manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest_value: toml::Value = toml::from_str(&raw_manifest)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

        self.resolution = Some(resolution);
        self.plan = Some(plan);
        self.launch = Some(launch);
        self.raw_manifest = Some(raw_manifest);
        self.manifest_value = Some(manifest_value);
        self.notes = notes;
        Ok(())
    }

    async fn run_install(&mut self) -> Result<()> {
        if let SessionStartSource::MaterializedRecord(record) = self.start_source.clone() {
            return self.install_from_materialized_record(&record);
        }

        if matches!(self.start_source, SessionStartSource::TomlPath) {
            return self.install_from_toml_path();
        }

        if matches!(self.start_source, SessionStartSource::Handle)
            && self.attach_state.is_empty()
            && let Some(hit) = crate::application::warm_launch::try_registry_live_reuse_fast_path(
                &self.handle,
                self.target_label.as_deref(),
            )?
        {
            self.install_reused = true;
            self.pre_projection_spec = Some(hit.pre_projection_spec);
            self._launch_lock = hit.launch_lock;
            self.session_info = Some(super::session::session_info_from_stored(*hit.record));
            return Ok(());
        }

        let resolution = super::resolve::build_resolution_for_session_start(
            &self.handle,
            self.target_label.as_deref(),
            None,
            true,
        )?;
        let (manifest_path, mut plan, mut launch, mut notes) = resolve_session_launch_plan(
            &self.handle,
            self.target_label.as_deref(),
            None,
            &self.attach_state,
        )?;
        let is_orchestration = self.target_label.is_none() && plan.is_orchestration_mode();

        // Env preflight runs BEFORE the live-session reuse check so we never
        // reuse a session when required environment variables are currently
        // absent.
        if is_orchestration {
            let orchestration = plan
                .resolve_services()
                .context("failed to resolve [services] orchestration plan")?;
            let manifest_preflight: toml::Value = toml::from_str(
                &std::fs::read_to_string(&manifest_path)
                    .with_context(|| format!("failed to read {}", manifest_path.display()))?,
            )
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
            crate::application::pipeline::phases::run::preflight_orchestration_session_environment(
                &plan,
                &manifest_preflight,
                &orchestration,
                &self.launch_ctx,
                &crate::application::dependency_credentials::ProcessHostEnv,
                "launching the session",
            )?;
        } else {
            target_runner::preflight_required_environment_variables(&plan, &self.launch_ctx)?;
        }

        // --- Warm-launch fast path (non-orchestration registry capsules only) ---
        //
        // For registry-installed capsules we can determine a stable
        // identity-addressed projection key before doing any projection work.
        // We compute the launch_spec now so the lock is held across all three
        // phases, matching the session contract; if the 5-condition reuse check
        // passes we short-circuit and skip projection, build, and execute.
        let is_registry_capsule = if !is_orchestration {
            capsule_core::common::paths::runtime_cache_dir()
                .map(|cache| plan.workspace_root.starts_with(&cache))
                .unwrap_or(false)
        } else {
            false
        };

        let (manifest_path, logical_cwd) = if is_registry_capsule {
            use crate::application::warm_launch::{self, ProjectionKey};

            let install_workspace = plan.workspace_root.clone();
            let workdir_relative = launch
                .working_dir
                .strip_prefix(&install_workspace)
                .map(|r| r.to_path_buf())
                .unwrap_or_default();
            let manifest_rel = manifest_path
                .strip_prefix(&install_workspace)
                .map(|r| r.to_path_buf())
                .unwrap_or_else(|_| std::path::PathBuf::from("capsule.toml"));

            let manifest_digest = warm_launch::compute_file_digest(&manifest_path)
                .unwrap_or_else(|_| "unknown".to_string());
            let lock_digest = warm_launch::compute_lock_digest(&install_workspace);
            let toolchain = bm::toolchain_fingerprint_for_plan(&plan);
            let platform = warm_launch::current_platform();
            let target_label = self
                .target_label
                .as_deref()
                .unwrap_or_else(|| plan.selected_target_label())
                .to_string();

            let proj_key = ProjectionKey::compute(
                &install_workspace.to_string_lossy(),
                &manifest_digest,
                lock_digest.as_deref(),
                &target_label,
                &platform,
                &toolchain,
            );

            let logical_cwd = warm_launch::make_logical_cwd(&proj_key, &workdir_relative);

            // Build the pre-projection LaunchSpec + acquire the advisory lock
            // so concurrent callers serialise on the same slot.
            let pre_spec = lm::canonicalize_launch_spec(
                &self.handle,
                &target_label,
                &plan,
                &launch,
                &manifest_path,
                Some(logical_cwd.clone()),
            )?;
            let launch_digest = lm::compute_launch_digest(&pre_spec);
            let launch_key = lm::compute_launch_key(&pre_spec);
            let lock = lm::acquire_launch_lock(&launch_key).ok();

            // Live-session reuse check BEFORE projection and build.
            let decision = lm::prepare_reuse_decision(&pre_spec, &launch_digest);
            if let Ok(lm::ReuseDecision::Reuse { record }) = decision {
                self.install_reused = true;
                self.pre_projection_spec = Some(pre_spec);
                self._launch_lock = lock;
                self.session_info = Some(super::session::session_info_from_stored(*record));
                // Populate the remaining fields (needed by phase_annotation).
                notes.extend(resolution.notes.clone());
                self.resolution = Some(resolution);
                self.manifest_path = Some(manifest_path);
                self.plan = Some(plan);
                self.launch = Some(launch);
                self.raw_manifest = None;
                self.manifest_value = None;
                self.notes = notes;
                return Ok(());
            }

            // Miss: resolve (or reuse) the identity-addressed projection root.
            let proj_resolution =
                warm_launch::resolve_identity_projection(&proj_key, &install_workspace)
                    .unwrap_or_else(|err| {
                        // Projection failure: fall back to a fresh random projection
                        // via the old path below.  This preserves availability at the
                        // cost of skipping warm-launch optimisations.
                        eprintln!(
                            "ATO-WARN warm-launch projection failed: {err}; using legacy path"
                        );
                        // Return a dummy that makes the caller drop to the legacy path.
                        // We signal this by returning Err from an outer block, but since
                        // we are in a non-error arm here we return a "created" resolution
                        // pointing at a temp dir that the legacy code will overwrite.
                        warm_launch::ProjectionResolution::fallback(&install_workspace)
                    });

            // Update plan and launch to point at the content-addressed root.
            let source_root = proj_resolution.source_root.clone();
            let new_manifest_path = source_root.join(&manifest_rel);
            plan.workspace_root = source_root.clone();
            plan.manifest_dir = new_manifest_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| source_root.clone());
            plan.manifest_path = new_manifest_path.clone();
            launch.working_dir = source_root.join(&workdir_relative);

            self.pre_projection_spec = Some(pre_spec);
            self._launch_lock = lock;
            (new_manifest_path, Some(logical_cwd))
        } else {
            // Local capsule or orchestration: keep original path, no logical cwd override.
            let manifest_path = if is_orchestration {
                manifest_path
            } else {
                match maybe_project_to_session(&manifest_path, &mut plan, &mut launch) {
                    Ok(maybe_path) => maybe_path.unwrap_or(manifest_path),
                    Err(err) => {
                        eprintln!(
                            "ATO-WARN source projection failed; falling back to install dir: {err}"
                        );
                        manifest_path
                    }
                }
            };
            (manifest_path, None)
        };

        notes.extend(resolution.notes.clone());
        let raw_manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest_value: toml::Value = toml::from_str(&raw_manifest)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

        // Store logical_cwd for Execute phase to use when building the full spec.
        if let Some(lcwd) = logical_cwd {
            // Stash it in pre_projection_spec if not already set above.
            if self.pre_projection_spec.is_none() {
                let target_label = self
                    .target_label
                    .as_deref()
                    .unwrap_or_else(|| plan.selected_target_label())
                    .to_string();
                let pre_spec = lm::canonicalize_launch_spec(
                    &self.handle,
                    &target_label,
                    &plan,
                    &launch,
                    &manifest_path,
                    Some(lcwd),
                )?;
                let launch_key = lm::compute_launch_key(&pre_spec);
                self._launch_lock = lm::acquire_launch_lock(&launch_key).ok();
                self.pre_projection_spec = Some(pre_spec);
            }
        }

        self.resolution = Some(resolution);
        self.manifest_path = Some(manifest_path);
        self.plan = Some(plan);
        self.launch = Some(launch);
        self.raw_manifest = Some(raw_manifest);
        self.manifest_value = Some(manifest_value);
        self.notes = notes;
        Ok(())
    }

    async fn run_build(&mut self) -> Result<()> {
        // Fast-path: if Install already returned a live session via reuse,
        // there is no need to run the build phase at all.
        if self.install_reused {
            return Ok(());
        }
        if matches!(
            &self.start_source,
            SessionStartSource::MaterializedRecord(_)
        ) {
            self.build_observation = None;
            self.build_decision_kind = Some(bm::BuildResultKind::Materialized);
            return Ok(());
        }

        // PR-C: orchestration mode no longer skips Build. The in-process
        // orchestration path runs in this process, so the wrapper owns the
        // build instead of delegating it to a nested `ato run`. The Build
        // helpers below are mode-agnostic; orchestration capsules whose
        // workload requires no build observe `BuildResultKind::NotApplicable`
        // through `prepare_decision`, the same way single-target capsules
        // without a build script do.

        let plan = self
            .plan
            .as_ref()
            .expect("install phase must populate plan before build");
        let workspace_root = plan.workspace_root.clone();

        let prepared = bm::prepare_decision(
            plan,
            &self.launch_ctx,
            bm::BuildPolicy::IfStale,
            &workspace_root,
        );
        self.build_observation = prepared.observation.clone();
        self.build_decision_kind = Some(prepared.decision.result_kind);
        let build_output_lock = if matches!(
            &prepared.decision.action,
            bm::DecisionAction::Project(_) | bm::DecisionAction::Execute | bm::DecisionAction::Fail
        ) {
            prepared
                .observation
                .as_ref()
                .map(
                    crate::application::phase_materializer::acquire_build_output_lock_for_observation,
                )
                .transpose()?
        } else {
            None
        };

        match prepared.decision.action {
            bm::DecisionAction::Skip => return Ok(()),
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
                        return Ok(());
                    }
                    Err(error) => {
                        eprintln!(
                            "ATO-WARN failed to project build output layer; trying remote \
                             materialization before local build: {}",
                            error
                        );
                        match try_remote_build_output_projection(
                            plan,
                            &workspace_root,
                            observation,
                            self.json,
                        ) {
                            Ok(true) => {
                                drop(build_output_lock);
                                self.build_decision_kind = Some(bm::BuildResultKind::Materialized);
                                return Ok(());
                            }
                            Ok(false) => {}
                            Err(remote_error) => {
                                eprintln!(
                                    "ATO-WARN remote build output materialization unavailable; \
                                     build will execute: {remote_error:#}"
                                );
                            }
                        }
                    }
                }
            }
            bm::DecisionAction::Fail => return Err(bm::no_build_error(&prepared.decision)),
            bm::DecisionAction::Execute => {
                if let Some(observation) = prepared.observation.as_ref() {
                    match try_remote_build_output_projection(
                        plan,
                        &workspace_root,
                        observation,
                        self.json,
                    ) {
                        Ok(true) => {
                            drop(build_output_lock);
                            self.build_decision_kind = Some(bm::BuildResultKind::Materialized);
                            return Ok(());
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

        // In `--json` mode the caller (Desktop orchestrator) parses the
        // session envelope from stdout, so anything the lifecycle prints —
        // both the `reporter.notify` headers and the inherited subprocess
        // stdout (`pnpm install` progress, the `next build` route table,
        // etc.) — must NOT land on stdout. Use `CliReporter::new_run` so
        // reporter output goes to stderr, and dup fd 1→fd 2 around the
        // lifecycle call so the subprocess's inherited stdout follows.
        let lifecycle_reporter = Arc::new(CliReporter::new_run(false));
        let stdout_guard = if self.json {
            Some(redirect_stdout_to_stderr().context("failed to redirect stdout for lifecycle")?)
        } else {
            None
        };
        let lifecycle_result = crate::commands::run::run_v03_lifecycle_steps(
            plan,
            &lifecycle_reporter,
            &self.launch_ctx,
        )
        .await;
        if let Some(saved) = stdout_guard {
            // Restore stdout before propagating any error so the caller's
            // stdout is intact (the envelope JSON, if any, is emitted by
            // start_session post-pipeline).
            let _ = restore_stdout(saved);
        }
        lifecycle_result?;

        if let Some(observation) = self.build_observation.as_ref() {
            // Capture while the lock is still held so that workspace-output
            // reading (capture) and state-record writing are inside the same
            // lock region as the build executor.
            // session_runner always uses BuildPolicy::IfStale; NoBuild is
            // not reachable here, so no hard-fail branch is needed.
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
            bm::persist_after_execute(plan, &workspace_root, observation, self.json, output_layer);
            drop(build_output_lock);
        }
        self.build_decision_kind = Some(bm::BuildResultKind::Executed);
        Ok(())
    }

    async fn run_execute(&mut self) -> Result<()> {
        // Fast-path: install phase already returned a live session.
        if self.install_reused {
            return Ok(());
        }

        let resolution = self
            .resolution
            .as_ref()
            .expect("install populates resolution");
        let manifest_path = self
            .manifest_path
            .as_ref()
            .expect("install populates manifest_path");
        let plan = self.plan.as_ref().expect("install populates plan");
        let launch = self.launch.as_ref().expect("install populates launch");
        let raw_manifest = self
            .raw_manifest
            .as_ref()
            .expect("install populates raw_manifest");
        let manifest_value = self
            .manifest_value
            .as_ref()
            .expect("install populates manifest_value");

        // App Session Materialization (RFC v0.2 §5.1):
        //
        //   acquire lock(launch_key)        ──┐
        //   lookup + 5-condition validate    │  held across the entire body
        //   ↳ Reuse: return existing envelope│  so a concurrent caller observes
        //   ↳ Spawn: start fresh, persist v2 │  the freshly-written record on
        //                                     │  unlock instead of duplicating.
        //   release lock                    ──┘
        //
        // If Install already computed the spec (registry capsule warm path),
        // reuse it so the digest is stable. Otherwise build it fresh here
        // (local capsule, orchestration, or any path that didn't go through
        // the warm projection branch).
        let (launch_spec, _guard) = if let Some(stored) = self.pre_projection_spec.take() {
            // Lock already acquired and stored; move it so it lives until
            // this function returns.
            let lock = self._launch_lock.take();
            (stored, lock)
        } else {
            let spec = lm::canonicalize_launch_spec(
                &self.handle,
                self.target_label
                    .as_deref()
                    .unwrap_or_else(|| plan.selected_target_label()),
                plan,
                launch,
                manifest_path,
                None,
            )?;
            let launch_key = lm::compute_launch_key(&spec);
            let lock = lm::acquire_launch_lock(&launch_key).ok();
            (spec, lock)
        };
        let launch_key = lm::compute_launch_key(&launch_spec);
        let launch_digest = lm::compute_launch_digest(&launch_spec);

        // 1. Lookup + validate.
        let materialized_start = matches!(
            &self.start_source,
            SessionStartSource::MaterializedRecord(_)
        );
        let decision = if materialized_start {
            None
        } else {
            let lookup_timer = PhaseStageTimer::start(HourglassPhase::Execute, "session_lookup");
            let decision = lm::prepare_reuse_decision(&launch_spec, &launch_digest);
            lookup_timer.finish_ok();
            Some(decision)
        };

        // 1b. Emit the execution receipt BEFORE we spawn the workload. The
        // launch envelope identity (source/deps/runtime/env/fs/policy/launch)
        // is fully determined at this point, and capsules that write into
        // their own working dir at startup would otherwise pollute
        // source_tree_hash before the observer reads it. Reuse path also
        // re-emits here so its computed_at refreshes against the same clean
        // workspace state.
        let prelaunch_receipt = match self.emit_execution_receipt() {
            Ok((metadata, bundle)) => {
                // PR-3b: publish declared/resolved ids to the boundary
                // sink IMMEDIATELY after the bundle is built — so if
                // the rest of run_execute (spawn, healthcheck,
                // session-record write) fails, the partial-receipt
                // boundary wrapper picks up the ids the would-be
                // success receipt would have carried.
                if let (Some(sink), Some(bundle_ref)) =
                    (self.receipt_graph_id_sink.as_ref(), bundle.as_ref())
                {
                    sink.set(crate::application::receipt_boundary::GraphIds {
                        declared_execution_id: Some(
                            bundle_ref
                                .derived
                                .execution_ids
                                .declared_execution_id
                                .clone(),
                        ),
                        resolved_execution_id: Some(
                            bundle_ref
                                .derived
                                .execution_ids
                                .resolved_execution_id
                                .clone(),
                        ),
                    });
                }
                Some(metadata)
            }
            Err(err) => {
                eprintln!(
                    "ATO-WARN session start failed to emit execution receipt: {}",
                    err
                );
                None
            }
        };

        let (mut info, fresh_spawn) = match decision {
            Some(Ok(lm::ReuseDecision::Reuse { record })) => {
                let validate_timer =
                    PhaseStageTimer::start(HourglassPhase::Execute, "session_validate");
                // The 5-condition check ran inside prepare_reuse_decision;
                // the timer here just bookmarks the validate boundary so
                // PHASE-TIMING shows the same shape regardless of hit/miss.
                validate_timer.finish_ok();

                self.execute_reused = true;
                (super::session::session_info_from_stored(*record), false)
            }
            Some(Ok(lm::ReuseDecision::Spawn { prior_kind })) => {
                self.execute_prior_kind = prior_kind;
                (
                    self.spawn_fresh_session(
                        resolution,
                        manifest_path,
                        plan,
                        manifest_value,
                        raw_manifest,
                        launch,
                    )?,
                    true,
                )
            }
            Some(Err(err)) => {
                // Lookup failure (e.g. session_root unreadable) — fall
                // through to spawn. The reuse miss is itself diagnostic
                // signal; surface it as `prior_kind=stale-session` is
                // misleading, so we leave prior_kind unset and let the
                // user inspect logs.
                eprintln!("ATO-WARN session reuse lookup failed: {}", err);
                (
                    self.spawn_fresh_session(
                        resolution,
                        manifest_path,
                        plan,
                        manifest_value,
                        raw_manifest,
                        launch,
                    )?,
                    true,
                )
            }
            None => (
                self.spawn_fresh_session(
                    resolution,
                    manifest_path,
                    plan,
                    manifest_value,
                    raw_manifest,
                    launch,
                )?,
                true,
            ),
        };

        // 3. Enrich the freshly-written record with schema=2 fields. Best-
        // effort: failures here only weaken future reuse, not the current
        // launch. Skipped for the reuse path because the existing record
        // already carries its enrichment from the original spawn.
        if fresh_spawn {
            let pid = info.pid() as u32;
            let process_start_time = lm::process_start_time_unix_ms(pid);
            if let Err(err) = lm::persist_after_spawn(
                pid,
                &launch_key,
                &launch_digest,
                process_start_time,
                prelaunch_receipt.as_ref(),
            ) {
                eprintln!(
                    "ATO-WARN failed to enrich session record with reuse metadata: {}",
                    err
                );
            }
        }

        // 4. Surface the prelaunch receipt identity onto the SessionInfo so
        //    the JSON envelope returned to the desktop carries it. The
        //    receipt itself was already emitted in step 1b above (before
        //    spawn) so observers see a clean workspace.
        if let Some(metadata) = prelaunch_receipt.as_ref() {
            info.attach_execution_receipt_metadata(metadata);
            if let Err(err) =
                execution_receipts::mark_v2_receipt_readiness_passed(&metadata.execution_id)
            {
                eprintln!(
                    "ATO-WARN failed to mark session execution receipt readiness-passed: {}",
                    err
                );
            }
            // Runtime observation v1 (#490): once the workload is ready, stamp
            // the observed launch envelope (captured by start_runtime_session)
            // onto the receipt and surface the derived observed_execution_id on
            // the session. Only a fresh spawn that produced real evidence is
            // stamped — reuse/warm-start carries no evidence (`take` → None),
            // and a non-V2 receipt or insufficient evidence is a no-op.
            if let Some(evidence) = info.take_observed_runtime() {
                match execution_receipts::mark_v2_receipt_observed(&metadata.execution_id, evidence)
                {
                    Ok(observed_id @ Some(_)) => info.set_observed_execution_id(observed_id),
                    Ok(None) => {}
                    Err(err) => eprintln!(
                        "ATO-WARN failed to stamp runtime observation onto execution receipt: {}",
                        err
                    ),
                }
            }
        }

        if fresh_spawn && let Some(run_config_hash) = self.expected_run_config_hash.as_deref() {
            let materialized_record = info.to_materialized_launch_record(
                resolution,
                &plan.workspace_root,
                &launch_key,
                &launch_digest,
                run_config_hash,
            );
            if let Err(err) = crate::app_control::session::launch_cache_root().and_then(|root| {
                crate::app_control::session::write_materialized_launch_record_atomic(
                    &root,
                    &materialized_record,
                )
            }) {
                eprintln!(
                    "ATO-WARN failed to persist materialized launch record for {}: {}",
                    materialized_record.launch_key, err
                );
            }
        }

        self.session_info = Some(info);
        Ok(())
    }

    /// Returns the receipt metadata and (for V2 schema launches) the
    /// `LaunchGraphBundle` that produced the receipt's
    /// declared/resolved execution ids. PR-3b: callers stash the bundle
    /// onto the runner so downstream steps share the same instance.
    fn emit_execution_receipt(
        &self,
    ) -> Result<(
        super::session::ExecutionReceiptSessionMetadata,
        Option<capsule_core::engine::execution_graph::LaunchGraphBundle>,
    )> {
        use capsule_core::engine::execution_plan::derive::compile_execution_plan;
        use capsule_core::execution_identity::ExecutionReceiptDocument;
        use capsule_core::router::ExecutionProfile;

        let manifest_path = self
            .manifest_path
            .as_ref()
            .context("emit_execution_receipt: manifest_path missing")?;
        let plan = self
            .plan
            .as_ref()
            .context("emit_execution_receipt: plan missing")?;

        let compiled = compile_execution_plan(
            manifest_path,
            ExecutionProfile::Dev,
            self.target_label.as_deref(),
        )
        .map_err(|err| anyhow::anyhow!("failed to compile execution plan: {err}"))?;

        let receipt_output =
            execution_receipt_builder::build_prelaunch_receipt_document_with_graph(
                plan,
                &compiled.execution_plan,
                &self.launch_ctx,
                self.build_observation.as_ref(),
            )?;
        // PR-3b: emit_execution_receipt returns the bundle alongside
        // the metadata so the caller can stash it onto self without
        // requiring a `&mut self` borrow here (which would conflict
        // with the immutable borrows of `self.{resolution, plan, ...}`
        // held by run_execute across the call site).
        let document = receipt_output.document;
        let _path = execution_receipts::write_receipt_document_atomic(&document)?;
        let metadata = match document {
            ExecutionReceiptDocument::V1(receipt) => {
                super::session::ExecutionReceiptSessionMetadata {
                    execution_id: receipt.execution_id,
                    schema_version: receipt.schema_version,
                    declared_execution_id: None,
                    resolved_execution_id: None,
                    observed_execution_id: None,
                    graph_completeness: None,
                    reproducibility_class: Some(format!("{:?}", receipt.reproducibility.class)),
                }
            }
            ExecutionReceiptDocument::V2(receipt) => {
                super::session::ExecutionReceiptSessionMetadata {
                    execution_id: receipt.execution_id,
                    schema_version: receipt.schema_version,
                    declared_execution_id: receipt.declared_execution_id,
                    resolved_execution_id: receipt.resolved_execution_id,
                    observed_execution_id: receipt.observed_execution_id,
                    graph_completeness: receipt
                        .graph_completeness
                        .as_ref()
                        .map(|completeness| completeness.as_str().to_string()),
                    reproducibility_class: Some(format!("{:?}", receipt.reproducibility.class)),
                }
            }
        };
        Ok((metadata, receipt_output.launch_graph))
    }

    // Note: `maybe_project_to_session` is a free function below the impl
    // because it does not borrow `self` and must run before `self.plan` /
    // `self.launch` are stored.

    /// Spawn a fresh guest or runtime session. Extracted from `run_execute`
    /// so the spawn path can be invoked from both the reuse-decision-spawn
    /// branch and the reuse-lookup-failure fallback without duplicating the
    /// guest-vs-runtime dispatch.
    fn spawn_fresh_session(
        &self,
        resolution: &HandleResolution,
        manifest_path: &std::path::Path,
        plan: &ManifestData,
        manifest_value: &toml::Value,
        raw_manifest: &str,
        launch: &LaunchSpec,
    ) -> Result<SessionInfo> {
        // Orchestration session dispatch (#73 PR-C):
        //   normal path  → start_orchestration_session_in_process
        //   ATO_LEGACY_SUPERVISOR=1 → start_orchestration_session_supervisor
        if self.is_orchestration_session() {
            if legacy_supervisor_enabled() {
                eprintln!(
                    "ATO-WARN ATO_LEGACY_SUPERVISOR=1 — using legacy nested `ato run` supervisor for orchestration session start. This emergency fallback is removed in v0.5.x.",
                );
                return start_orchestration_session_supervisor(
                    &self.handle,
                    resolution,
                    manifest_path,
                    plan,
                    self.notes.clone(),
                );
            }
            return start_orchestration_session_in_process(
                &self.handle,
                resolution,
                manifest_path,
                plan,
                raw_manifest,
                self.notes.clone(),
            );
        }

        let guest = parse_guest_contract(
            manifest_value,
            manifest_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );

        if let Some(guest) = guest {
            start_guest_session(
                &self.handle,
                resolution,
                manifest_path,
                plan,
                guest,
                self.notes.clone(),
            )
        } else {
            start_runtime_session(
                &self.handle,
                resolution,
                manifest_path,
                plan,
                raw_manifest,
                launch,
                self.notes.clone(),
            )
        }
    }
}

/// Phase Y option 2 helper: when the resolved manifest lives under the
/// shared `~/.ato/runtimes/` install tree, project the install source into
/// a per-session workspace via hardlinks (see
/// `application::source_projection`) and re-anchor `plan` and `launch` at
/// the projected path. Returns the new manifest path on success.
///
/// Returns `Ok(None)` for non-registry capsules (local user projects),
/// where we keep the original workspace pointing at the user's editable
/// source. Returns `Err` only on filesystem failures during projection;
/// the caller treats those as best-effort and falls back to the install
/// dir.
fn maybe_project_to_session(
    manifest_path: &std::path::Path,
    plan: &mut ManifestData,
    launch: &mut LaunchSpec,
) -> Result<Option<PathBuf>> {
    use capsule_core::common::paths::{ato_runs_dir, runtime_cache_dir};

    let runtime_cache = runtime_cache_dir().ok();
    let install_workspace = plan.workspace_root.clone();
    let is_under_runtimes = runtime_cache
        .as_ref()
        .is_some_and(|cache| install_workspace.starts_with(cache));
    if !is_under_runtimes {
        return Ok(None);
    }

    // Determine the launch's relative working dir before we mutate `launch`.
    let workdir_relative = launch
        .working_dir
        .strip_prefix(&install_workspace)
        .map(|rel| rel.to_path_buf())
        .unwrap_or_else(|_| std::path::PathBuf::new());

    // Allocate a fresh session-scoped projection target. The path embeds a
    // monotonic + random suffix per `ato_run_layout`, so concurrent session
    // starts of the same capsule do not race on the same projection dir.
    let session_dir = ato_runs_dir().join(format!(
        "session-y-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        rand::random::<u64>()
    ));
    let projection_target = session_dir.join("workspace").join("source");

    crate::application::source_projection::project_install_source(
        &install_workspace,
        &projection_target,
    )
    .with_context(|| {
        format!(
            "failed to project install source {} -> {}",
            install_workspace.display(),
            projection_target.display()
        )
    })?;

    let new_manifest_path = projection_target.join(
        manifest_path
            .strip_prefix(&install_workspace)
            .map(|rel| rel.to_path_buf())
            .unwrap_or_else(|_| std::path::PathBuf::from("capsule.toml")),
    );

    plan.workspace_root = projection_target.clone();
    plan.manifest_dir = new_manifest_path
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| projection_target.clone());
    plan.manifest_path = new_manifest_path.clone();
    launch.working_dir = projection_target.join(&workdir_relative);

    Ok(Some(new_manifest_path))
}

#[async_trait(?Send)]
impl HourglassPhaseRunner for SessionStartPhaseRunner {
    async fn run_phase(
        &mut self,
        phase: HourglassPhase,
        _attempt: &mut PipelineAttemptContext,
    ) -> Result<()> {
        match phase {
            HourglassPhase::Install => self.run_install().await,
            HourglassPhase::Prepare | HourglassPhase::Verify | HourglassPhase::DryRun => Ok(()),
            HourglassPhase::Build => self.run_build().await,
            HourglassPhase::Execute => self.run_execute().await,
            HourglassPhase::Finalize | HourglassPhase::Publish => {
                anyhow::bail!("unsupported phase for session start: {}", phase.as_str())
            }
        }
    }

    fn phase_annotation(&self, phase: HourglassPhase) -> Option<PhaseAnnotation> {
        match phase {
            HourglassPhase::Build => {
                let mut annotation = PhaseAnnotation::with_result_kind(
                    self.build_decision_kind
                        .map(|kind| kind.as_str())
                        .unwrap_or("executed"),
                );
                if let Some(observation) = &self.build_observation {
                    annotation.add_extra("source", observation.source.timing_label());
                    if let Some(label) = observation.source.heuristic_label() {
                        annotation.add_extra("heuristic", label);
                    }
                    annotation.add_extra("target", observation.target.clone());
                    annotation.add_extra("digest", observation.input_digest.clone());
                }
                Some(annotation)
            }
            // No-op phases for v0: mark as not-applicable so PHASE-TIMING
            // distinguishes them from real executions and matches RFC §6.1.
            HourglassPhase::Prepare | HourglassPhase::Verify | HourglassPhase::DryRun => {
                Some(PhaseAnnotation::with_result_kind("not-applicable"))
            }
            HourglassPhase::Execute => {
                let mut annotation = PhaseAnnotation::with_result_kind(if self.execute_reused {
                    "materialized-session"
                } else {
                    "executed"
                });
                if let Some(prior) = self.execute_prior_kind {
                    // prior_kind is meaningful only on miss → spawn paths;
                    // omit it on reuse hits since there is no rejected
                    // candidate to attribute.
                    if !self.execute_reused {
                        annotation.add_extra("prior_kind", prior.as_str());
                    }
                }
                Some(annotation)
            }
            _ => Some(PhaseAnnotation::with_result_kind("executed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::receipt_boundary::{GraphIds, ReceiptGraphIdSink};

    /// PR-3b (PR #180 review fix): the wrapper in
    /// `app_control::session::start_session_for_capsule` writes its
    /// boundary sink onto the runner before the pipeline runs, so
    /// `emit_execution_receipt` can publish declared/resolved ids
    /// the moment the LaunchGraphBundle is built. The smoke test
    /// here pins the wire-up: after assignment, the runner's
    /// `receipt_graph_id_sink` shares the same Arc cell as the
    /// input sink — so a publish on the input side is observable
    /// from the runner side.
    #[test]
    fn assigning_receipt_graph_id_sink_shares_arc_with_input_sink() {
        let mut runner =
            SessionStartPhaseRunner::new("publisher/slug", None, Vec::new(), None, false);
        assert!(
            runner.receipt_graph_id_sink.is_none(),
            "fixture sanity: a freshly built runner has no sink"
        );

        let sink = ReceiptGraphIdSink::new();
        runner.receipt_graph_id_sink = Some(sink.clone());

        // Publish from the boundary side; the runner side must observe
        // the same ids because both handles are Arc-clones of one cell.
        sink.set(GraphIds {
            declared_execution_id: Some("blake3:session-declared".to_string()),
            resolved_execution_id: Some("blake3:session-resolved".to_string()),
        });

        let snapshot = runner.receipt_graph_id_sink.as_ref().unwrap().snapshot();
        assert_eq!(
            snapshot.declared_execution_id.as_deref(),
            Some("blake3:session-declared"),
            "PR-3b: runner-side sink must share the Arc with the boundary's input sink"
        );
        assert_eq!(
            snapshot.resolved_execution_id.as_deref(),
            Some("blake3:session-resolved")
        );
    }
}
