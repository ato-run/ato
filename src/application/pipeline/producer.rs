use anyhow::Result;

use crate::application::pipeline::executor::{HourglassPhaseRunner, HourglassPipeline};
use crate::application::pipeline::hourglass::{
    HourglassFlow, HourglassPhase, HourglassPhaseSelection,
};

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

pub(crate) struct ProducerPipeline {
    inner: HourglassPipeline,
}

impl ProducerPipeline {
    pub(crate) fn new(selection: HourglassPhaseSelection) -> Self {
        Self {
            inner: HourglassPipeline::new(selection),
        }
    }

    pub(crate) async fn run<R>(&self, runner: &mut R) -> Result<()>
    where
        R: HourglassPhaseRunner,
    {
        self.inner.run(runner).await
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
    use super::{
        build_publish_pipeline_plan, select_publish_dry_run_phases, select_publish_phases,
        should_warn_legacy_full_publish, validate_publish_phase_options, PublishPhaseOptions,
        PublishPipelineRequest,
    };
    use crate::application::pipeline::hourglass::HourglassPhase;

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
}
