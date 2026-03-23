use anyhow::Result;
use async_trait::async_trait;

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
        for phase in self.selection.flow.phases() {
            if self.selection.runs(*phase) {
                runner.run_phase(*phase).await?;
            } else {
                runner.skip_phase(*phase).await?;
            }
        }

        Ok(())
    }
}

#[async_trait(?Send)]
pub(crate) trait HourglassPhaseRunner {
    async fn run_phase(&mut self, phase: HourglassPhase) -> Result<()>;

    async fn skip_phase(&mut self, _phase: HourglassPhase) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use async_trait::async_trait;

    use super::{HourglassPhaseRunner, HourglassPipeline};
    use crate::application::pipeline::hourglass::{
        HourglassFlow, HourglassPhase, HourglassPhaseSelection,
    };

    #[derive(Default)]
    struct Recorder {
        entries: Vec<(HourglassPhase, &'static str)>,
    }

    #[async_trait(?Send)]
    impl HourglassPhaseRunner for Recorder {
        async fn run_phase(&mut self, phase: HourglassPhase) -> Result<()> {
            self.entries.push((phase, "run"));
            Ok(())
        }

        async fn skip_phase(&mut self, phase: HourglassPhase) -> Result<()> {
            self.entries.push((phase, "skip"));
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
}
