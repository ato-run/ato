use anyhow::Result;

use crate::application::pipeline::executor::{HourglassPhaseRunner, HourglassPipeline};
use crate::application::pipeline::hourglass::{
    HourglassFlow, HourglassPhase, HourglassPhaseSelection,
};

pub(crate) struct ConsumerRunPipeline {
    inner: HourglassPipeline,
}

impl ConsumerRunPipeline {
    pub(crate) fn standard() -> Self {
        Self {
            inner: HourglassPipeline::new(HourglassPhaseSelection {
                flow: HourglassFlow::ConsumerRun,
                start: HourglassPhase::Install,
                stop: HourglassPhase::Execute,
                explicit_filter: false,
            }),
        }
    }

    pub(crate) async fn run<R>(&self, runner: &mut R) -> Result<()>
    where
        R: HourglassPhaseRunner,
    {
        self.inner.run(runner).await
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use async_trait::async_trait;

    use super::ConsumerRunPipeline;
    use crate::application::pipeline::executor::HourglassPhaseRunner;
    use crate::application::pipeline::hourglass::HourglassPhase;

    #[derive(Default)]
    struct Recorder {
        entries: Vec<HourglassPhase>,
    }

    #[async_trait(?Send)]
    impl HourglassPhaseRunner for Recorder {
        async fn run_phase(&mut self, phase: HourglassPhase) -> Result<()> {
            self.entries.push(phase);
            Ok(())
        }
    }

    #[tokio::test]
    async fn standard_consumer_pipeline_runs_install_through_execute() {
        let pipeline = ConsumerRunPipeline::standard();
        let mut recorder = Recorder::default();

        pipeline.run(&mut recorder).await.expect("run pipeline");

        assert_eq!(
            recorder.entries,
            vec![
                HourglassPhase::Install,
                HourglassPhase::Prepare,
                HourglassPhase::Build,
                HourglassPhase::Verify,
                HourglassPhase::DryRun,
                HourglassPhase::Execute,
            ]
        );
    }
}
