use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::application::pipeline::executor::HourglassPhaseRunner;
use crate::application::pipeline::hourglass::{
    HourglassFlow, HourglassPhase, HourglassPhaseSelection,
};

const PRODUCER_PHASE_SEQUENCE: &[HourglassPhase] = &[
    HourglassPhase::Prepare,
    HourglassPhase::Build,
    HourglassPhase::Verify,
    HourglassPhase::Install,
    HourglassPhase::DryRun,
    HourglassPhase::Publish,
];

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PublishPhaseOptions {
    pub(crate) fix: bool,
    pub(crate) legacy_full_publish: bool,
    pub(crate) allow_existing: bool,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct PublishPipelineRequest {
    pub(crate) top_level_dry_run: bool,
    pub(crate) prepare: bool,
    pub(crate) build: bool,
    pub(crate) deploy: bool,
    pub(crate) is_official: bool,
    pub(crate) has_artifact: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PublishPipelinePlan {
    pub(crate) selection: HourglassPhaseSelection,
    pub(crate) warn_legacy_full_publish: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublishInstallResult {
    pub(crate) scoped_id: String,
    pub(crate) version: String,
    pub(crate) path: PathBuf,
    pub(crate) content_hash: String,
    pub(crate) install_kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublishDryRunStageResult {
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diagnosis: Option<crate::publish_official::OfficialPublishDiagnosis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) upload_endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reachable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) auth_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) permission_check: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct PublishPipelineState {
    pub(crate) artifact_path: Option<PathBuf>,
    pub(crate) verified_artifact: Option<crate::publish_artifact::VerifiedArtifactInfo>,
    pub(crate) resolved_scoped_id: Option<String>,
    pub(crate) resolved_version: Option<String>,
    pub(crate) install_result: Option<PublishInstallResult>,
    pub(crate) dry_run_result: Option<PublishDryRunStageResult>,
}

impl PublishPipelineState {
    pub(crate) fn with_resolved_release(mut self, scoped_id: String, version: String) -> Self {
        self.resolved_scoped_id = Some(scoped_id);
        self.resolved_version = Some(version);
        self
    }
}

pub(crate) struct ProducerPipeline {
    selection: HourglassPhaseSelection,
}

impl ProducerPipeline {
    pub(crate) fn new(selection: HourglassPhaseSelection) -> Self {
        Self { selection }
    }

    pub(crate) async fn run_until<R>(
        &self,
        stop_point: HourglassPhase,
        runner: &mut R,
    ) -> Result<()>
    where
        R: HourglassPhaseRunner,
    {
        for phase in PRODUCER_PHASE_SEQUENCE {
            if *phase > stop_point {
                runner.skip_phase(*phase).await?;
                continue;
            }

            if self.selection.runs(*phase) {
                runner.run_phase(*phase).await?;
            } else {
                runner.skip_phase(*phase).await?;
            }
        }

        Ok(())
    }
}
pub(crate) fn build_publish_pipeline_plan(
    request: PublishPipelineRequest,
    options: PublishPhaseOptions,
) -> Result<PublishPipelinePlan> {
    let selection = if request.top_level_dry_run {
        select_publish_dry_run_phases(request.has_artifact)
    } else {
        select_publish_phases(
            request.prepare,
            request.build,
            request.deploy,
            request.is_official,
            options.legacy_full_publish,
            request.has_artifact,
        )
    };
    validate_publish_phase_options(options, selection, request.is_official)?;

    Ok(PublishPipelinePlan {
        selection,
        warn_legacy_full_publish: should_warn_legacy_full_publish(
            options,
            selection,
            request.is_official,
        ),
    })
}

pub(crate) fn select_publish_dry_run_phases(has_artifact: bool) -> HourglassPhaseSelection {
    HourglassPhaseSelection {
        flow: HourglassFlow::ProducerPublish,
        start: if has_artifact {
            HourglassPhase::Verify
        } else {
            HourglassPhase::Prepare
        },
        stop: HourglassPhase::DryRun,
        explicit_filter: false,
    }
}

pub(crate) fn select_publish_phases(
    prepare: bool,
    build: bool,
    deploy: bool,
    is_official: bool,
    legacy_full_publish: bool,
    has_artifact: bool,
) -> HourglassPhaseSelection {
    let explicit_filter = prepare || build || deploy;
    let stop = if explicit_filter {
        if deploy {
            HourglassPhase::Publish
        } else if build {
            HourglassPhase::Verify
        } else {
            HourglassPhase::Prepare
        }
    } else {
        HourglassPhase::Publish
    };

    let official_deploy_only = is_official && !legacy_full_publish && deploy && !prepare && !build;
    let official_default_publish_only = is_official && !legacy_full_publish && !explicit_filter;

    let start = if official_deploy_only {
        HourglassPhase::Publish
    } else if has_artifact {
        HourglassPhase::Verify
    } else if official_default_publish_only {
        HourglassPhase::Publish
    } else {
        HourglassPhase::Prepare
    };

    HourglassPhaseSelection {
        flow: HourglassFlow::ProducerPublish,
        start,
        stop,
        explicit_filter,
    }
}

pub(crate) fn validate_publish_phase_options(
    options: PublishPhaseOptions,
    selection: HourglassPhaseSelection,
    is_official: bool,
) -> Result<()> {
    if options.fix && !(is_official && selection.runs_publish()) {
        anyhow::bail!("--fix is only available when deploying to official registry");
    }

    if options.legacy_full_publish && !is_official {
        anyhow::bail!("--legacy-full-publish is only available for official registry publish");
    }

    if options.legacy_full_publish && selection.explicit_filter {
        anyhow::bail!("--legacy-full-publish cannot be combined with --prepare/--build/--deploy");
    }

    if options.allow_existing && (is_official || !selection.runs_publish()) {
        anyhow::bail!("--allow-existing is only available for private registry deploy phase");
    }

    if selection.start > selection.stop {
        anyhow::bail!(
            "The selected publish phase range is invalid. `--artifact` cannot be combined with stop points that end before Verify."
        );
    }

    Ok(())
}

pub(crate) fn should_warn_legacy_full_publish(
    options: PublishPhaseOptions,
    selection: HourglassPhaseSelection,
    is_official: bool,
) -> bool {
    options.legacy_full_publish && is_official && !selection.explicit_filter
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use async_trait::async_trait;

    use super::{
        build_publish_pipeline_plan, select_publish_dry_run_phases, select_publish_phases,
        should_warn_legacy_full_publish, validate_publish_phase_options, ProducerPipeline,
        PublishPhaseOptions, PublishPipelineRequest,
    };
    use crate::application::pipeline::executor::HourglassPhaseRunner;
    use crate::application::pipeline::hourglass::HourglassPhase;

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

    #[test]
    fn private_deploy_runs_install_and_dry_run() {
        let selected = select_publish_phases(false, false, true, false, false, false);
        assert!(selected.runs_prepare());
        assert!(selected.runs_build());
        assert!(selected.runs_verify());
        assert!(selected.runs_install());
        assert!(selected.runs_dry_run());
        assert!(selected.runs_publish());
    }

    #[test]
    fn official_default_is_publish_only() {
        let selected = select_publish_phases(false, false, false, true, false, false);
        assert_eq!(selected.start, HourglassPhase::Publish);
        assert_eq!(selected.stop, HourglassPhase::Publish);
    }

    #[test]
    fn dry_run_with_artifact_starts_at_verify() {
        let selected = select_publish_dry_run_phases(true);
        assert_eq!(selected.start, HourglassPhase::Verify);
        assert_eq!(selected.stop, HourglassPhase::DryRun);
    }

    #[test]
    fn validation_rejects_allow_existing_without_publish() {
        let selected = select_publish_phases(false, true, false, false, false, false);
        let err = validate_publish_phase_options(
            PublishPhaseOptions {
                allow_existing: true,
                ..PublishPhaseOptions::default()
            },
            selected,
            false,
        )
        .expect_err("validation should fail");
        assert!(err.to_string().contains("--allow-existing"));
    }

    #[test]
    fn legacy_warning_only_applies_to_official_default() {
        let selected = select_publish_phases(false, false, false, true, true, false);
        assert!(should_warn_legacy_full_publish(
            PublishPhaseOptions {
                legacy_full_publish: true,
                ..PublishPhaseOptions::default()
            },
            selected,
            true,
        ));
    }

    #[test]
    fn build_plan_combines_selection_validation_and_warning() {
        let plan = build_publish_pipeline_plan(
            PublishPipelineRequest {
                top_level_dry_run: false,
                prepare: false,
                build: false,
                deploy: false,
                is_official: true,
                has_artifact: false,
            },
            PublishPhaseOptions {
                legacy_full_publish: true,
                ..PublishPhaseOptions::default()
            },
        )
        .expect("build plan");

        assert_eq!(plan.selection.start, HourglassPhase::Prepare);
        assert_eq!(plan.selection.stop, HourglassPhase::Publish);
        assert!(plan.warn_legacy_full_publish);
    }

    #[tokio::test]
    async fn producer_pipeline_runs_publish_phases_in_publish_order() {
        let pipeline = ProducerPipeline::new(select_publish_phases(
            false, false, true, false, false, false,
        ));
        let mut recorder = Recorder::default();

        pipeline
            .run_until(HourglassPhase::Publish, &mut recorder)
            .await
            .expect("run pipeline");

        assert_eq!(
            recorder.entries,
            vec![
                (HourglassPhase::Prepare, "run"),
                (HourglassPhase::Build, "run"),
                (HourglassPhase::Verify, "run"),
                (HourglassPhase::Install, "run"),
                (HourglassPhase::DryRun, "run"),
                (HourglassPhase::Publish, "run"),
            ]
        );
    }

    #[tokio::test]
    async fn producer_pipeline_run_until_skips_after_stop_point() {
        let pipeline = ProducerPipeline::new(select_publish_phases(
            false, false, true, false, false, false,
        ));
        let mut recorder = Recorder::default();

        pipeline
            .run_until(HourglassPhase::Verify, &mut recorder)
            .await
            .expect("run pipeline until verify");

        assert_eq!(
            recorder.entries,
            vec![
                (HourglassPhase::Prepare, "run"),
                (HourglassPhase::Build, "run"),
                (HourglassPhase::Verify, "run"),
                (HourglassPhase::Install, "run"),
                (HourglassPhase::DryRun, "skip"),
                (HourglassPhase::Publish, "skip"),
            ]
        );
    }
}
