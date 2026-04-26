use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::application::pipeline::cleanup::{PipelineAttemptContext, PipelineAttemptError};
use crate::application::pipeline::executor::HourglassPhaseRunner;
use crate::application::pipeline::hourglass::{
    HourglassFlow, HourglassPhase, HourglassPhaseSelection,
};

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PublishPhaseOptions {
    pub(crate) fix: bool,
    pub(crate) legacy_full_publish: bool,
    pub(crate) allow_existing: bool,
    pub(crate) finalize_local: bool,
    pub(crate) allow_external_finalize: bool,
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct PublishPipelineRequest {
    pub(crate) top_level_dry_run: bool,
    pub(crate) prepare: bool,
    pub(crate) build: bool,
    pub(crate) deploy: bool,
    pub(crate) is_official: bool,
    pub(crate) has_artifact: bool,
    pub(crate) finalize_local: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PublishPipelinePlan {
    pub(crate) selection: HourglassPhaseSelection,
    pub(crate) warn_legacy_full_publish: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishTargetMode {
    PersonalDockDirect,
    OfficialCi,
    CustomDirect,
}

impl PublishTargetMode {
    pub(crate) fn is_official(self) -> bool {
        matches!(self, Self::OfficialCi)
    }

    pub(crate) fn is_personal_dock(self) -> bool {
        matches!(self, Self::PersonalDockDirect)
    }

    pub(crate) fn route_label(self) -> &'static str {
        match self {
            Self::PersonalDockDirect => "personal_dock_direct",
            Self::OfficialCi => "official",
            Self::CustomDirect => "private",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedPublishTarget {
    pub(crate) registry_url: String,
    pub(crate) mode: PublishTargetMode,
    pub(crate) publisher_handle: Option<String>,
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
    artifact_path: Option<PathBuf>,
    verified_artifact: Option<crate::publish_artifact::VerifiedArtifactInfo>,
    resolved_scoped_id: Option<String>,
    resolved_version: Option<String>,
    install_result: Option<PublishInstallResult>,
    dry_run_result: Option<PublishDryRunStageResult>,
}

impl PublishPipelineState {
    pub(crate) fn with_resolved_release(mut self, scoped_id: String, version: String) -> Self {
        self.resolved_scoped_id = Some(scoped_id);
        self.resolved_version = Some(version);
        self
    }

    pub(crate) fn record_built_artifact(&mut self, artifact_path: PathBuf) {
        self.artifact_path = Some(artifact_path);
    }

    pub(crate) fn artifact_path(&self) -> Option<&PathBuf> {
        self.artifact_path.as_ref()
    }

    pub(crate) fn artifact_path_or(&self, fallback: Option<PathBuf>) -> Option<PathBuf> {
        fallback.or_else(|| self.artifact_path.clone())
    }

    pub(crate) fn record_verified_artifact(
        &mut self,
        artifact_path: PathBuf,
        verification: crate::publish_artifact::VerifiedArtifactInfo,
    ) {
        self.artifact_path = Some(artifact_path);
        self.verified_artifact = Some(verification);
    }

    pub(crate) fn verified_artifact(
        &self,
    ) -> Option<&crate::publish_artifact::VerifiedArtifactInfo> {
        self.verified_artifact.as_ref()
    }

    pub(crate) fn record_install_result(&mut self, result: PublishInstallResult) {
        self.install_result = Some(result);
    }

    pub(crate) fn install_result(&self) -> Option<&PublishInstallResult> {
        self.install_result.as_ref()
    }

    pub(crate) fn record_dry_run_result(&mut self, result: PublishDryRunStageResult) {
        self.dry_run_result = Some(result);
    }

    pub(crate) fn dry_run_result(&self) -> Option<&PublishDryRunStageResult> {
        self.dry_run_result.as_ref()
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
        let mut attempt = PipelineAttemptContext::default();
        let phases = self.selection.flow.phases();
        let stop_index = phase_index(self.selection.flow, stop_point)
            .unwrap_or_else(|| panic!("missing stop phase {}", stop_point.as_str()));

        for phase in phases {
            attempt.enter_phase(*phase);
            let phase_index = phase_index(self.selection.flow, *phase)
                .unwrap_or_else(|| panic!("missing phase {}", phase.as_str()));
            let result = if phase_index > stop_index {
                runner.skip_phase(*phase, &mut attempt).await
            } else if self.selection.runs(*phase) {
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
pub(crate) fn build_publish_pipeline_plan(
    request: PublishPipelineRequest,
    options: PublishPhaseOptions,
) -> Result<PublishPipelinePlan> {
    let selection = if request.top_level_dry_run {
        select_publish_dry_run_phases(request.has_artifact, request.finalize_local)
    } else {
        select_publish_phases(
            request.prepare,
            request.build,
            request.deploy,
            request.is_official,
            options.legacy_full_publish,
            request.has_artifact,
            request.finalize_local,
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

pub(crate) fn select_publish_dry_run_phases(
    has_artifact: bool,
    finalize_local: bool,
) -> HourglassPhaseSelection {
    HourglassPhaseSelection {
        flow: if finalize_local && !has_artifact {
            HourglassFlow::ProducerPublishFinalize
        } else {
            HourglassFlow::ProducerPublish
        },
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
    finalize_local: bool,
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
        flow: if finalize_local && !has_artifact && !is_official {
            HourglassFlow::ProducerPublishFinalize
        } else {
            HourglassFlow::ProducerPublish
        },
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

    if options.finalize_local && !options.allow_external_finalize {
        anyhow::bail!("--finalize-local requires --allow-external-finalize");
    }

    if options.finalize_local && is_official {
        anyhow::bail!("--finalize-local is not available with --ci/official publish");
    }

    if options.finalize_local && !selection.runs_build() {
        anyhow::bail!("--finalize-local is only available for source publish, not --artifact");
    }

    if phase_index(selection.flow, selection.start).is_none()
        || phase_index(selection.flow, selection.stop).is_none()
    {
        anyhow::bail!("The selected publish phase range is invalid for the active publish flow.");
    }

    if phase_index(selection.flow, selection.start) > phase_index(selection.flow, selection.stop) {
        anyhow::bail!(
            "The selected publish phase range is invalid. `--artifact` cannot be combined with stop points that end before Verify."
        );
    }

    Ok(())
}

pub(crate) fn resolve_publish_target_from_sources(
    cli_registry: Option<&str>,
    manifest_registry: Option<&str>,
    publisher_handle: Option<&str>,
) -> Result<ResolvedPublishTarget> {
    if let Some(url) = cli_registry {
        return resolve_explicit_publish_target(url);
    }

    if let Some(url) = manifest_registry {
        return resolve_explicit_publish_target(url);
    }

    if let Some(handle) = publisher_handle {
        return Ok(ResolvedPublishTarget {
            registry_url: crate::auth::default_store_registry_url(),
            mode: PublishTargetMode::PersonalDockDirect,
            publisher_handle: Some(handle.to_string()),
        });
    }

    anyhow::bail!(
        "No default publish target found. Run `ato login` to publish to your Personal Dock, or pass `--registry https://api.ato.run` / `--ci` for the official Store."
    );
}

fn resolve_explicit_publish_target(raw: &str) -> Result<ResolvedPublishTarget> {
    let normalized = crate::registry::http::normalize_registry_url(raw, "registry")?;
    if is_legacy_dock_publish_registry(&normalized) {
        anyhow::bail!(
            "Registry URL `{}` is no longer supported. Personal Dock publish now uses `https://api.ato.run`; `/d/<handle>` is a UI page, not a registry.",
            normalized
        );
    }

    Ok(ResolvedPublishTarget {
        registry_url: normalized.clone(),
        mode: if is_official_publish_registry(&normalized) {
            PublishTargetMode::OfficialCi
        } else {
            PublishTargetMode::CustomDirect
        },
        publisher_handle: None,
    })
}

fn is_official_publish_registry(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("api.ato.run") || host.eq_ignore_ascii_case("staging.api.ato.run")
}

pub(crate) fn is_legacy_dock_publish_registry(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(mut segments) = parsed.path_segments() else {
        return false;
    };
    while let Some(segment) = segments.next() {
        if segment == "d" {
            return segments
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some();
        }
    }
    false
}

pub(crate) fn should_warn_legacy_full_publish(
    options: PublishPhaseOptions,
    selection: HourglassPhaseSelection,
    is_official: bool,
) -> bool {
    options.legacy_full_publish && is_official && !selection.explicit_filter
}

fn phase_index(flow: HourglassFlow, phase: HourglassPhase) -> Option<usize> {
    flow.phases()
        .iter()
        .position(|candidate| *candidate == phase)
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
    use crate::application::pipeline::cleanup::PipelineAttemptContext;
    use crate::application::pipeline::executor::HourglassPhaseRunner;
    use crate::application::pipeline::hourglass::{HourglassFlow, HourglassPhase};

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

    #[test]
    fn private_deploy_runs_install_and_dry_run() {
        let selected = select_publish_phases(false, false, true, false, false, false, false);
        assert!(selected.runs_prepare());
        assert!(selected.runs_build());
        assert!(selected.runs_verify());
        assert!(selected.runs_install());
        assert!(selected.runs_dry_run());
        assert!(selected.runs_publish());
    }

    #[test]
    fn official_default_is_publish_only() {
        let selected = select_publish_phases(false, false, false, true, false, false, false);
        assert_eq!(selected.start, HourglassPhase::Publish);
        assert_eq!(selected.stop, HourglassPhase::Publish);
    }

    #[test]
    fn dry_run_with_artifact_starts_at_verify() {
        let selected = select_publish_dry_run_phases(true, false);
        assert_eq!(selected.start, HourglassPhase::Verify);
        assert_eq!(selected.stop, HourglassPhase::DryRun);
    }

    #[test]
    fn validation_rejects_allow_existing_without_publish() {
        let selected = select_publish_phases(false, true, false, false, false, false, false);
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
        let selected = select_publish_phases(false, false, false, true, true, false, false);
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
                finalize_local: false,
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
            false, false, true, false, false, false, false,
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
            false, false, true, false, false, false, false,
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
                (HourglassPhase::Install, "skip"),
                (HourglassPhase::DryRun, "skip"),
                (HourglassPhase::Publish, "skip"),
            ]
        );
    }

    #[test]
    fn finalize_local_uses_finalize_flow() {
        let selected = select_publish_phases(false, false, true, false, false, false, true);
        assert_eq!(selected.flow, HourglassFlow::ProducerPublishFinalize);
        assert!(selected.runs_finalize());
    }

    #[test]
    fn validation_rejects_finalize_without_external_permission() {
        let selected = select_publish_phases(false, false, true, false, false, false, true);
        let err = validate_publish_phase_options(
            PublishPhaseOptions {
                finalize_local: true,
                ..PublishPhaseOptions::default()
            },
            selected,
            false,
        )
        .expect_err("validation should fail");
        assert!(err.to_string().contains("--allow-external-finalize"));
    }

    #[test]
    fn validation_rejects_finalize_for_artifact_publish() {
        let selected = select_publish_phases(false, false, true, false, false, true, true);
        let err = validate_publish_phase_options(
            PublishPhaseOptions {
                finalize_local: true,
                allow_external_finalize: true,
                ..PublishPhaseOptions::default()
            },
            selected,
            false,
        )
        .expect_err("validation should fail");
        assert!(err.to_string().contains("--artifact"));
    }

    #[tokio::test]
    async fn producer_pipeline_runs_finalize_publish_phases_in_order() {
        let pipeline = ProducerPipeline::new(select_publish_phases(
            false, false, true, false, false, false, true,
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
                (HourglassPhase::Install, "run"),
                (HourglassPhase::Finalize, "run"),
                (HourglassPhase::Verify, "run"),
                (HourglassPhase::DryRun, "run"),
                (HourglassPhase::Publish, "run"),
            ]
        );
    }
}
