use anyhow::Result;
use async_trait::async_trait;

use crate::application::pipeline::cleanup::{PipelineAttemptContext, PipelineAttemptError};
use crate::application::pipeline::hourglass::{HourglassPhase, HourglassPhaseSelection};

pub(crate) struct HourglassPipeline {
    selection: HourglassPhaseSelection,
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

        for phase in self.selection.flow.phases() {
            if !runner.should_continue() {
                break;
            }
            attempt.enter_phase(*phase);
            let result = if self.selection.runs(*phase) {
                runner.run_phase(*phase, &mut attempt).await
            } else {
                runner.skip_phase(*phase, &mut attempt).await
            };

            if let Err(err) = result {
                let cleanup_report = attempt.unwind_cleanup();
                return Err(PipelineAttemptError::new(*phase, err, cleanup_report).into());
            }
        }

        attempt.mark_committed();
        Ok(())
    }
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
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use capsule_core::execution_plan::error::{CleanupActionRecord, CleanupActionStatus};

    use super::{HourglassPhaseRunner, HourglassPipeline};
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
        events: Rc<RefCell<Vec<String>>>,
    }

    #[async_trait(?Send)]
    impl HourglassPhaseRunner for FailingRunner {
        async fn run_phase(
            &mut self,
            phase: HourglassPhase,
            attempt: &mut PipelineAttemptContext,
        ) -> Result<()> {
            if phase == HourglassPhase::Prepare {
                let events = Rc::clone(&self.events);
                let mut scope = attempt.cleanup_scope();
                scope.register(move || CleanupActionRecord {
                    action: "remove_temp_dir".to_string(),
                    status: CleanupActionStatus::Succeeded,
                    detail: Some({
                        events.borrow_mut().push("cleanup".to_string());
                        ".tmp/work".to_string()
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
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut runner = FailingRunner {
            events: Rc::clone(&events),
        };

        let err = pipeline.run(&mut runner).await.unwrap_err();
        let attempt_err = err.downcast_ref::<PipelineAttemptError>().unwrap();

        assert_eq!(attempt_err.phase(), HourglassPhase::Prepare);
        assert_eq!(events.borrow().as_slice(), ["cleanup".to_string()]);
        assert_eq!(attempt_err.cleanup_report().actions.len(), 1);
    }
}
