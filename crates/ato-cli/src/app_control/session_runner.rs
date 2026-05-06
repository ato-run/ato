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

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use capsule_core::launch_spec::{derive_launch_spec, LaunchSpec};
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
use crate::executors::target_runner::preflight_required_environment_variables;
use crate::reporters::CliReporter;

use super::guest_contract::parse_guest_contract;
use super::resolve::{build_resolution, resolve_local_plan, HandleResolution};
use super::session::{
    orchestration_leaf_target_label, redirect_stdout_to_stderr, resolve_session_launch_plan,
    restore_stdout, start_guest_session, start_orchestration_session_supervisor,
    start_runtime_session, SessionInfo,
};

pub(super) struct SessionStartPhaseRunner<'a> {
    handle: &'a str,
    target_label: Option<&'a str>,
    json: bool,

    // Set by Install phase
    resolution: Option<HandleResolution>,
    manifest_path: Option<PathBuf>,
    plan: Option<ManifestData>,
    launch: Option<LaunchSpec>,
    raw_manifest: Option<String>,
    notes: Vec<String>,
    launch_ctx: RuntimeLaunchContext,

    // Set by Build phase
    build_observation: Option<bm::BuildObservation>,
    build_decision_kind: Option<bm::BuildResultKind>,

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
}

impl<'a> SessionStartPhaseRunner<'a> {
    pub(super) fn new(handle: &'a str, target_label: Option<&'a str>, json: bool) -> Self {
        Self {
            handle,
            target_label,
            json,
            resolution: None,
            manifest_path: None,
            plan: None,
            launch: None,
            raw_manifest: None,
            notes: Vec::new(),
            launch_ctx: RuntimeLaunchContext::empty(),
            build_observation: None,
            build_decision_kind: None,
            execute_reused: false,
            execute_prior_kind: None,
            session_info: None,
        }
    }

    async fn run_install(&mut self) -> Result<()> {
        let resolution = build_resolution(self.handle, self.target_label, None)?;
        let (manifest_path, mut plan, mut launch, mut notes) =
            resolve_session_launch_plan(self.handle, self.target_label)?;

        if self.target_label.is_none() {
            if let Some(leaf_target) = orchestration_leaf_target_label(&plan)? {
                if leaf_target != plan.selected_target_label() {
                    let (rerouted_plan, _guest, rerouted_notes) =
                        resolve_local_plan(&manifest_path, Some(&leaf_target))?;
                    let rerouted_launch =
                        derive_launch_spec(&rerouted_plan).with_context(|| {
                            format!(
                            "failed to derive launch spec for orchestration leaf target '{}' in {}",
                            leaf_target,
                            manifest_path.display()
                        )
                        })?;
                    notes.extend(rerouted_notes);
                    notes.push(format!(
                        "Orchestration mode: selected leaf target '{}' for Desktop session.",
                        leaf_target
                    ));
                    plan = rerouted_plan;
                    launch = rerouted_launch;
                }
            }
        }

        // Phase Y option 2: re-anchor the registry-installed capsule onto a
        // session-local hardlink projection so the launch cannot mutate the
        // shared install dir at `~/.ato/runtimes/<scoped>/<version>/`.
        // Local capsules (manifest path outside the runtimes tree) keep
        // their original workspace_root so user edits stay live.
        let manifest_path = match maybe_project_to_session(&manifest_path, &mut plan, &mut launch) {
            Ok(maybe_path) => maybe_path.unwrap_or(manifest_path),
            Err(err) => {
                eprintln!("ATO-WARN source projection failed; falling back to install dir: {err}");
                manifest_path
            }
        };

        notes.extend(resolution.notes.clone());
        let raw_manifest = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        // RuntimeLaunchContext::empty() matches the pre-pipeline behavior of
        // `start_session`: no IPC bindings, no extra injected env. The check
        // falls back to OS env / manifest env entries — which is what the
        // spawned child will actually receive.
        preflight_required_environment_variables(&plan, &self.launch_ctx)?;

        self.resolution = Some(resolution);
        self.manifest_path = Some(manifest_path);
        self.plan = Some(plan);
        self.launch = Some(launch);
        self.raw_manifest = Some(raw_manifest);
        self.notes = notes;
        Ok(())
    }

    async fn run_build(&mut self) -> Result<()> {
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

        match prepared.decision.action {
            bm::DecisionAction::Skip => return Ok(()),
            bm::DecisionAction::Fail => return Err(bm::no_build_error(&prepared.decision)),
            bm::DecisionAction::Execute => {}
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
            bm::persist_after_execute(plan, &workspace_root, observation, self.json);
        }
        self.build_decision_kind = Some(bm::BuildResultKind::Executed);
        Ok(())
    }

    async fn run_execute(&mut self) -> Result<()> {
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

        // App Session Materialization (RFC v0.2 §5.1):
        //
        //   acquire lock(launch_key)        ──┐
        //   lookup + 5-condition validate    │  held across the entire body
        //   ↳ Reuse: return existing envelope│  so a concurrent caller observes
        //   ↳ Spawn: start fresh, persist v2 │  the freshly-written record on
        //                                     │  unlock instead of duplicating.
        //   release lock                    ──┘
        //
        // Lock failures are non-fatal for v0: if we cannot acquire the
        // file lock (permission, exotic FS, etc.) we proceed without it.
        // The reuse path still functions — it just falls back to "no race
        // protection," which is no worse than the pre-RFC behavior.
        let launch_spec = lm::canonicalize_launch_spec(
            self.handle,
            self.target_label
                .unwrap_or_else(|| plan.selected_target_label()),
            plan,
            launch,
            manifest_path,
        )?;
        let launch_digest = lm::compute_launch_digest(&launch_spec);
        let launch_key = lm::compute_launch_key(&launch_spec);
        let _lock = lm::acquire_launch_lock(&launch_key).ok();

        // 1. Lookup + validate.
        let lookup_timer = PhaseStageTimer::start(HourglassPhase::Execute, "session_lookup");
        let decision = lm::prepare_reuse_decision(&launch_spec, &launch_digest);
        lookup_timer.finish_ok();

        // 1b. Emit the execution receipt BEFORE we spawn the workload. The
        // launch envelope identity (source/deps/runtime/env/fs/policy/launch)
        // is fully determined at this point, and capsules that write into
        // their own working dir at startup would otherwise pollute
        // source_tree_hash before the observer reads it. Reuse path also
        // re-emits here so its computed_at refreshes against the same clean
        // workspace state.
        let prelaunch_receipt = match self.emit_execution_receipt() {
            Ok(pair) => Some(pair),
            Err(err) => {
                eprintln!(
                    "ATO-WARN session start failed to emit execution receipt: {}",
                    err
                );
                None
            }
        };

        let (mut info, fresh_spawn) = match decision {
            Ok(lm::ReuseDecision::Reuse { record }) => {
                let validate_timer =
                    PhaseStageTimer::start(HourglassPhase::Execute, "session_validate");
                // The 5-condition check ran inside prepare_reuse_decision;
                // the timer here just bookmarks the validate boundary so
                // PHASE-TIMING shows the same shape regardless of hit/miss.
                validate_timer.finish_ok();

                self.execute_reused = true;
                (super::session::session_info_from_stored(*record), false)
            }
            Ok(lm::ReuseDecision::Spawn { prior_kind }) => {
                self.execute_prior_kind = prior_kind;
                (
                    self.spawn_fresh_session(
                        resolution,
                        manifest_path,
                        plan,
                        raw_manifest,
                        launch,
                    )?,
                    true,
                )
            }
            Err(err) => {
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
                        raw_manifest,
                        launch,
                    )?,
                    true,
                )
            }
        };

        // 3. Enrich the freshly-written record with schema=2 fields. Best-
        // effort: failures here only weaken future reuse, not the current
        // launch. Skipped for the reuse path because the existing record
        // already carries its enrichment from the original spawn.
        if fresh_spawn {
            let pid = info.pid() as u32;
            let process_start_time = lm::process_start_time_unix_ms(pid);
            if let Err(err) = lm::persist_after_spawn(pid, &launch_digest, process_start_time) {
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
        if let Some((execution_id, schema_version)) = prelaunch_receipt {
            info.attach_execution_receipt(execution_id, schema_version);
        }

        self.session_info = Some(info);
        Ok(())
    }

    fn emit_execution_receipt(&self) -> Result<(String, u32)> {
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

        let compiled =
            compile_execution_plan(manifest_path, ExecutionProfile::Dev, self.target_label)
                .map_err(|err| anyhow::anyhow!("failed to compile execution plan: {err}"))?;

        let document = execution_receipt_builder::build_prelaunch_receipt_document(
            plan,
            &compiled.execution_plan,
            &self.launch_ctx,
            self.build_observation.as_ref(),
        )?;
        let _path = execution_receipts::write_receipt_document_atomic(&document)?;
        let (execution_id, schema_version) = match document {
            ExecutionReceiptDocument::V1(receipt) => (receipt.execution_id, receipt.schema_version),
            ExecutionReceiptDocument::V2(receipt) => (receipt.execution_id, receipt.schema_version),
        };
        Ok((execution_id, schema_version))
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
        raw_manifest: &str,
        launch: &LaunchSpec,
    ) -> Result<SessionInfo> {
        if self.target_label.is_none() && plan.is_orchestration_mode() {
            return start_orchestration_session_supervisor(
                self.handle,
                resolution,
                manifest_path,
                plan,
                raw_manifest,
                self.notes.clone(),
            );
        }

        let manifest_value: toml::Value = toml::from_str(raw_manifest)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        let guest = parse_guest_contract(
            &manifest_value,
            manifest_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );

        if let Some(guest) = guest {
            start_guest_session(
                self.handle,
                resolution,
                manifest_path,
                plan,
                guest,
                self.notes.clone(),
            )
        } else {
            start_runtime_session(
                self.handle,
                resolution,
                manifest_path,
                plan,
                raw_manifest,
                launch,
                self.target_label.is_some(),
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
impl HourglassPhaseRunner for SessionStartPhaseRunner<'_> {
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
