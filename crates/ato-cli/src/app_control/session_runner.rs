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
use capsule_core::launch_spec::LaunchSpec;
use capsule_core::router::ManifestData;

use crate::application::build_materialization as bm;
use crate::application::pipeline::cleanup::PipelineAttemptContext;
use crate::application::pipeline::executor::{HourglassPhaseRunner, PhaseAnnotation};
use crate::application::pipeline::hourglass::HourglassPhase;
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::executors::target_runner::preflight_required_environment_variables;
use crate::reporters::CliReporter;

use super::guest_contract::parse_guest_contract;
use super::resolve::{build_resolution, HandleResolution};
use super::session::{
    redirect_stdout_to_stderr, resolve_session_launch_plan, restore_stdout, start_guest_session,
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
            session_info: None,
        }
    }

    async fn run_install(&mut self) -> Result<()> {
        let resolution = build_resolution(self.handle, self.target_label, None)?;
        let (manifest_path, plan, launch, mut notes) =
            resolve_session_launch_plan(self.handle, self.target_label)?;
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

        let manifest_value: toml::Value = toml::from_str(raw_manifest)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        let guest = parse_guest_contract(
            &manifest_value,
            manifest_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );

        let info = if let Some(guest) = guest {
            start_guest_session(
                self.handle,
                resolution,
                manifest_path,
                plan,
                guest,
                self.notes.clone(),
            )?
        } else {
            start_runtime_session(
                self.handle,
                resolution,
                manifest_path,
                plan,
                raw_manifest,
                launch,
                self.notes.clone(),
            )?
        };
        self.session_info = Some(info);
        Ok(())
    }
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
            _ => Some(PhaseAnnotation::with_result_kind("executed")),
        }
    }
}
