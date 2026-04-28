use anyhow::Result;
use async_trait::async_trait;

use crate::application::pipeline::cleanup::{PipelineAttemptContext, PipelineAttemptError};
use crate::application::pipeline::hourglass::{HourglassPhase, HourglassPhaseSelection};

pub(crate) struct HourglassPipeline {
    selection: HourglassPhaseSelection,
}

/// Diagnostic annotation a runner may attach to a phase result so the executor
/// can include it in `PHASE-TIMING` output.
///
/// `result_kind` is the canonical category (e.g. `executed`, `materialized`,
/// `not-applicable`) that maps to `HourglassPhaseResult::result_kind`. `extras`
/// is a list of additional `key=value` pairs (e.g. `source=heuristic`,
/// `heuristic=nextjs:v1`) that are emitted verbatim. Keys must be ASCII
/// identifiers; values are emitted with `{:?}` (Rust debug) so multi-line or
/// quote-bearing values stay grep-friendly on a single line.
#[derive(Debug, Default, Clone)]
pub(crate) struct PhaseAnnotation {
    pub(crate) result_kind: Option<String>,
    pub(crate) extras: Vec<(String, String)>,
}

impl PhaseAnnotation {
    pub(crate) fn with_result_kind(kind: impl Into<String>) -> Self {
        Self {
            result_kind: Some(kind.into()),
            extras: Vec::new(),
        }
    }

    #[allow(dead_code)] // wired by PR-B'/PR-C for source/heuristic annotations
    pub(crate) fn add_extra(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.extras.push((key.into(), value.into()));
    }
}

impl HourglassPipeline {
    pub(crate) fn new(selection: HourglassPhaseSelection) -> Self {
        Self { selection }
    }

    pub(crate) async fn run<R>(&self, runner: &mut R) -> Result<()>
    where
        R: HourglassPhaseRunner,
    {
        let mut attempt = PipelineAttemptContext::default();
        attempt.activate_sigint_cleanup();

        let timing_enabled = phase_timing_enabled();
        let pipeline_started = std::time::Instant::now();

        for phase in self.selection.flow.phases() {
            if !runner.should_continue() {
                break;
            }
            attempt.enter_phase(*phase);
            let phase_started = std::time::Instant::now();
            let selected = self.selection.runs(*phase);
            let result = if selected {
                runner.run_phase(*phase, &mut attempt).await
            } else {
                runner.skip_phase(*phase, &mut attempt).await
            };
            let elapsed_ms = phase_started.elapsed().as_millis() as u64;

            if timing_enabled {
                let annotation = runner.phase_annotation(*phase);
                let state = if result.is_err() {
                    "fail"
                } else if selected {
                    "ok"
                } else {
                    "skip"
                };
                let error = result.as_ref().err().map(|err| err.to_string());
                emit_phase_timing(*phase, state, elapsed_ms, error.as_deref(), annotation);
            }

            if let Err(err) = result {
                let cleanup_report = attempt.unwind_cleanup();
                return Err(PipelineAttemptError::new(*phase, err, cleanup_report).into());
            }
        }

        if timing_enabled {
            let total_ms = pipeline_started.elapsed().as_millis() as u64;
            eprintln!("PHASE-TIMING total elapsed_ms={}", total_ms);
        }

        attempt.mark_committed();
        Ok(())
    }
}

fn phase_timing_enabled() -> bool {
    match std::env::var("ATO_PHASE_TIMING") {
        Ok(value) => {
            let trimmed = value.trim();
            !trimmed.is_empty() && !matches!(trimmed, "0" | "false" | "off" | "no")
        }
        Err(_) => false,
    }
}

/// Emit a fine-grained sub-stage timing line during a phase.
///
/// Format: `PHASE-TIMING phase=<phase> stage=<stage> state=<ok|fail> elapsed_ms=<n> [error=...]`.
/// The `stage=` field is optional — its absence on a `PHASE-TIMING` line
/// means the line is the phase-level summary (existing contract preserved).
/// No-op when `ATO_PHASE_TIMING` is not enabled, so production stderr stays
/// clean.
///
/// Stages live below phases. They are intended for callers (phase impls)
/// that want to attribute their elapsed_ms to internal sub-stages — e.g.
/// `prepare_session_execution`, `spawn_runtime_process`, `wait_http_ready`
/// inside `Execute`.
pub(crate) fn emit_phase_stage_timing(
    phase: HourglassPhase,
    stage: &str,
    state: &str,
    elapsed_ms: u64,
    error: Option<&str>,
) {
    if !phase_timing_enabled() {
        return;
    }
    let mut line = format!(
        "PHASE-TIMING phase={} stage={} state={} elapsed_ms={}",
        phase.as_str(),
        stage,
        state,
        elapsed_ms
    );
    if let Some(message) = error {
        let truncated: String = message.chars().take(200).collect();
        let one_line = truncated.replace('\n', " ");
        line.push_str(&format!(" error={:?}", one_line));
    }
    eprintln!("{}", line);
}

/// Convenience timer paired with [`emit_phase_stage_timing`]. Records its
/// start instant and emits an `ok` (or `fail`) line on demand. Use it when
/// you have a fixed sub-stage boundary; for ad-hoc cases call
/// `emit_phase_stage_timing` directly with your own `Instant::now()`.
pub(crate) struct PhaseStageTimer {
    phase: HourglassPhase,
    stage: &'static str,
    started: std::time::Instant,
}

impl PhaseStageTimer {
    pub(crate) fn start(phase: HourglassPhase, stage: &'static str) -> Self {
        Self {
            phase,
            stage,
            started: std::time::Instant::now(),
        }
    }

    pub(crate) fn finish_ok(self) {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        emit_phase_stage_timing(self.phase, self.stage, "ok", elapsed_ms, None);
    }

    #[allow(dead_code)] // Reserved for fail-emitting call sites; not yet used in v0.
    pub(crate) fn finish_fail(self, error: &str) {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        emit_phase_stage_timing(self.phase, self.stage, "fail", elapsed_ms, Some(error));
    }
}

fn emit_phase_timing(
    phase: HourglassPhase,
    state: &str,
    elapsed_ms: u64,
    error: Option<&str>,
    annotation: Option<PhaseAnnotation>,
) {
    let mut line = format!(
        "PHASE-TIMING phase={} state={} elapsed_ms={}",
        phase.as_str(),
        state,
        elapsed_ms
    );
    if let Some(annotation) = annotation {
        if let Some(kind) = annotation.result_kind {
            line.push_str(&format!(" result_kind={}", kind));
        }
        for (key, value) in annotation.extras {
            line.push_str(&format!(" {}={:?}", key, value));
        }
    }
    if let Some(message) = error {
        let truncated: String = message.chars().take(200).collect();
        let one_line = truncated.replace('\n', " ");
        line.push_str(&format!(" error={:?}", one_line));
    }
    eprintln!("{}", line);
}

#[async_trait(?Send)]
pub(crate) trait HourglassPhaseRunner {
    async fn run_phase(
        &mut self,
        phase: HourglassPhase,
        attempt: &mut PipelineAttemptContext,
    ) -> Result<()>;

    fn should_continue(&self) -> bool {
        true
    }

    async fn skip_phase(
        &mut self,
        _phase: HourglassPhase,
        _attempt: &mut PipelineAttemptContext,
    ) -> Result<()> {
        Ok(())
    }

    /// Called by the executor after a phase finishes (success, skip, or fail)
    /// to attach diagnostic metadata to the `PHASE-TIMING` line. Default
    /// implementation returns no annotation, preserving prior behavior for
    /// runners that don't opt in.
    fn phase_annotation(&self, _phase: HourglassPhase) -> Option<PhaseAnnotation> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use capsule_core::execution_plan::error::{CleanupActionRecord, CleanupActionStatus};

    use super::{HourglassPhaseRunner, HourglassPipeline, PhaseAnnotation};
    use crate::application::pipeline::cleanup::{PipelineAttemptContext, PipelineAttemptError};
    use crate::application::pipeline::hourglass::{
        HourglassFlow, HourglassPhase, HourglassPhaseSelection,
    };

    #[derive(Default)]
    struct Recorder {
        entries: Vec<(HourglassPhase, &'static str)>,
    }

    #[async_trait(?Send)]
    impl HourglassPhaseRunner for Recorder {
        async fn run_phase(
            &mut self,
            phase: HourglassPhase,
            _attempt: &mut PipelineAttemptContext,
        ) -> Result<()> {
            self.entries.push((phase, "run"));
            Ok(())
        }

        async fn skip_phase(
            &mut self,
            phase: HourglassPhase,
            _attempt: &mut PipelineAttemptContext,
        ) -> Result<()> {
            self.entries.push((phase, "skip"));
            Ok(())
        }
    }

    struct FailingRunner {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait(?Send)]
    impl HourglassPhaseRunner for FailingRunner {
        async fn run_phase(
            &mut self,
            phase: HourglassPhase,
            attempt: &mut PipelineAttemptContext,
        ) -> Result<()> {
            if phase == HourglassPhase::Prepare {
                let events = Arc::clone(&self.events);
                let mut scope = attempt.cleanup_scope();
                scope.register(move || CleanupActionRecord {
                    action: "remove_temp_dir".to_string(),
                    status: CleanupActionStatus::Succeeded,
                    detail: Some({
                        events.lock().unwrap().push("cleanup".to_string());
                        ".ato/tmp/work".to_string()
                    }),
                });
                return Err(anyhow!("prepare failed"));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn pipeline_runs_consumer_phases_in_hourglass_order() {
        let pipeline = HourglassPipeline::new(HourglassPhaseSelection {
            flow: HourglassFlow::ConsumerRun,
            start: HourglassPhase::Prepare,
            stop: HourglassPhase::Execute,
            explicit_filter: false,
        });
        let mut recorder = Recorder::default();

        pipeline.run(&mut recorder).await.expect("run pipeline");

        assert_eq!(
            recorder.entries,
            vec![
                (HourglassPhase::Install, "skip"),
                (HourglassPhase::Prepare, "run"),
                (HourglassPhase::Build, "run"),
                (HourglassPhase::Verify, "run"),
                (HourglassPhase::DryRun, "run"),
                (HourglassPhase::Execute, "run"),
            ]
        );
    }

    #[tokio::test]
    async fn pipeline_runs_publish_stop_range_and_skips_outside_phases() {
        let pipeline = HourglassPipeline::new(HourglassPhaseSelection {
            flow: HourglassFlow::ProducerPublish,
            start: HourglassPhase::Verify,
            stop: HourglassPhase::DryRun,
            explicit_filter: true,
        });
        let mut recorder = Recorder::default();

        pipeline.run(&mut recorder).await.expect("run pipeline");

        assert_eq!(
            recorder.entries,
            vec![
                (HourglassPhase::Prepare, "skip"),
                (HourglassPhase::Build, "skip"),
                (HourglassPhase::Verify, "run"),
                (HourglassPhase::Install, "run"),
                (HourglassPhase::DryRun, "run"),
                (HourglassPhase::Publish, "skip"),
            ]
        );
    }

    #[tokio::test]
    async fn pipeline_unwinds_registered_cleanup_on_failure() {
        let pipeline = HourglassPipeline::new(HourglassPhaseSelection {
            flow: HourglassFlow::ConsumerRun,
            start: HourglassPhase::Prepare,
            stop: HourglassPhase::Execute,
            explicit_filter: false,
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut runner = FailingRunner {
            events: Arc::clone(&events),
        };

        let err = pipeline.run(&mut runner).await.unwrap_err();
        let attempt_err = err.downcast_ref::<PipelineAttemptError>().unwrap();

        assert_eq!(attempt_err.phase(), HourglassPhase::Prepare);
        assert_eq!(events.lock().unwrap().as_slice(), ["cleanup".to_string()]);
        assert_eq!(attempt_err.cleanup_report().actions.len(), 1);
    }

    #[test]
    fn phase_annotation_builder_collects_extras() {
        let mut annotation = PhaseAnnotation::with_result_kind("materialized");
        annotation.add_extra("source", "heuristic");
        annotation.add_extra("heuristic", "nextjs:v1");
        assert_eq!(annotation.result_kind.as_deref(), Some("materialized"));
        assert_eq!(annotation.extras.len(), 2);
        assert_eq!(annotation.extras[0].0, "source");
        assert_eq!(annotation.extras[1].1, "nextjs:v1");
    }
}
