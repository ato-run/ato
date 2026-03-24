use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tracing::debug;

pub(crate) use crate::application::pipeline::hourglass::HourglassPhase as RunPhaseBoundary;
use crate::install::support::{enforce_sandbox_mode_flags, execute_run_command};
pub(crate) use crate::install::support::{LocalRunManifestPreparationOutcome, ResolvedRunTarget};
use crate::reporters;
use crate::{install, CompatibilityFallbackBackend, EnforcementMode, RunAgentMode};

pub(crate) struct RunLikeCommandArgs {
    pub(crate) path: PathBuf,
    pub(crate) target: Option<String>,
    pub(crate) watch: bool,
    pub(crate) background: bool,
    pub(crate) nacelle: Option<PathBuf>,
    pub(crate) registry: Option<String>,
    pub(crate) state: Vec<String>,
    pub(crate) inject: Vec<String>,
    pub(crate) enforcement: EnforcementMode,
    pub(crate) sandbox_mode: bool,
    pub(crate) unsafe_mode_legacy: bool,
    pub(crate) unsafe_bypass_sandbox_legacy: bool,
    pub(crate) dangerously_skip_permissions: bool,
    pub(crate) compatibility_fallback: Option<CompatibilityFallbackBackend>,
    pub(crate) yes: bool,
    pub(crate) agent_mode: RunAgentMode,
    pub(crate) keep_failed_artifacts: bool,
    pub(crate) allow_unverified: bool,
    pub(crate) deprecation_warning: Option<&'static str>,
    pub(crate) reporter: Arc<reporters::CliReporter>,
}

pub(crate) fn execute_run_like_command(args: RunLikeCommandArgs) -> Result<()> {
    if let Some(warning) = args.deprecation_warning {
        eprintln!("{warning}");
    }

    let rt = tokio::runtime::Runtime::new()?;

    let install_phase = rt.block_on(execute_run_install_phase(
        args.path,
        args.yes,
        args.keep_failed_artifacts,
        args.allow_unverified,
        args.registry.as_deref(),
        args.reporter.clone(),
    ))?;
    if install_phase.should_stop_after_install {
        return Ok(());
    }

    let sandbox_requested =
        args.sandbox_mode || args.unsafe_mode_legacy || args.unsafe_bypass_sandbox_legacy;
    let effective_enforcement = enforce_sandbox_mode_flags(
        args.enforcement,
        sandbox_requested,
        args.dangerously_skip_permissions,
        args.compatibility_fallback,
        args.reporter.clone(),
    )?;
    execute_run_command(
        install_phase.resolved_target.path,
        args.target,
        args.watch,
        args.background,
        args.nacelle,
        effective_enforcement,
        sandbox_requested,
        args.dangerously_skip_permissions,
        args.compatibility_fallback
            .map(CompatibilityFallbackBackend::as_str)
            .map(str::to_string),
        args.yes,
        args.agent_mode,
        install_phase.resolved_target.agent_local_root,
        args.state,
        args.inject,
        args.reporter,
    )
}

struct RunInstallPhaseResult {
    resolved_target: ResolvedRunTarget,
    should_stop_after_install: bool,
}

async fn execute_run_install_phase(
    path: PathBuf,
    yes: bool,
    keep_failed_artifacts: bool,
    allow_unverified: bool,
    registry: Option<&str>,
    reporter: Arc<reporters::CliReporter>,
) -> Result<RunInstallPhaseResult> {
    debug!(
        phase = RunPhaseBoundary::Install.as_str(),
        "Running run pipeline phase"
    );

    let resolved_target = install::support::resolve_run_target_or_install(
        path,
        yes,
        keep_failed_artifacts,
        allow_unverified,
        registry,
        reporter.clone(),
    )
    .await?;
    let manifest_outcome =
        install::support::ensure_local_manifest_ready_for_run(&resolved_target, yes, reporter)?;

    Ok(RunInstallPhaseResult {
        resolved_target,
        should_stop_after_install: matches!(
            manifest_outcome,
            LocalRunManifestPreparationOutcome::CreatedManualManifest
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{LocalRunManifestPreparationOutcome, ResolvedRunTarget, RunPhaseBoundary};
    use crate::install::support::LocalRunManifestStatus;
    use std::sync::Arc;

    #[test]
    fn run_phase_boundaries_follow_hourglass_order() {
        assert!(RunPhaseBoundary::Install < RunPhaseBoundary::Prepare);
        assert!(RunPhaseBoundary::Prepare < RunPhaseBoundary::Build);
        assert!(RunPhaseBoundary::Build < RunPhaseBoundary::Verify);
        assert!(RunPhaseBoundary::Verify < RunPhaseBoundary::DryRun);
        assert!(RunPhaseBoundary::DryRun < RunPhaseBoundary::Execute);
        assert_eq!(RunPhaseBoundary::DryRun.as_str(), "dry_run");
    }

    #[test]
    fn local_directory_is_agent_eligible() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            crate::install::support::agent_local_root_for_path(tmp.path()),
            Some(tmp.path().to_path_buf())
        );
    }

    #[test]
    fn missing_manifest_is_detected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let status =
            crate::install::support::inspect_local_run_manifest(&tmp.path().join("capsule.toml"))
                .expect("inspect");
        assert!(matches!(status, LocalRunManifestStatus::Missing));
    }

    #[test]
    fn invalid_manifest_is_backed_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(&manifest_path, "not = [valid").expect("manifest");

        let status =
            crate::install::support::inspect_local_run_manifest(&manifest_path).expect("inspect");
        assert!(matches!(status, LocalRunManifestStatus::Invalid { .. }));

        let backup_path =
            crate::install::support::backup_invalid_manifest(&manifest_path).expect("backup");
        assert!(!manifest_path.exists());
        assert!(backup_path.exists());
        assert_eq!(
            backup_path.parent().expect("parent"),
            tmp.path().join(".tmp/ato/run-invalid-manifests")
        );
        assert!(backup_path
            .file_name()
            .and_then(|value| value.to_str())
            .expect("file name")
            .starts_with("capsule.toml.invalid."));
    }

    #[test]
    fn missing_manifest_is_generated_with_yes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resolved = ResolvedRunTarget {
            path: tmp.path().to_path_buf(),
            agent_local_root: Some(tmp.path().to_path_buf()),
        };
        let reporter = Arc::new(crate::reporters::CliReporter::new(true));

        let outcome =
            crate::install::support::ensure_local_manifest_ready_for_run(&resolved, true, reporter)
                .expect("run");
        assert_eq!(outcome, LocalRunManifestPreparationOutcome::Ready);

        let manifest = std::fs::read_to_string(tmp.path().join("capsule.toml")).expect("manifest");
        assert!(manifest.contains("schema_version = \"0.2\""));
        assert!(manifest.contains("name = "));
    }

    #[test]
    fn invalid_manifest_is_regenerated_with_yes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(&manifest_path, "not = [valid").expect("manifest");
        let resolved = ResolvedRunTarget {
            path: tmp.path().to_path_buf(),
            agent_local_root: Some(tmp.path().to_path_buf()),
        };
        let reporter = Arc::new(crate::reporters::CliReporter::new(true));

        let outcome =
            crate::install::support::ensure_local_manifest_ready_for_run(&resolved, true, reporter)
                .expect("run");
        assert_eq!(outcome, LocalRunManifestPreparationOutcome::Ready);
        assert!(manifest_path.exists());

        let backups: Vec<_> = std::fs::read_dir(tmp.path().join(".tmp/ato/run-invalid-manifests"))
            .expect("read dir")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(backups.len(), 1);
    }
}
