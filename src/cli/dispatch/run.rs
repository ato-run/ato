use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

#[cfg(test)]
pub(crate) use crate::application::pipeline::hourglass::HourglassPhase as RunPhaseBoundary;
use crate::install::support::{enforce_sandbox_mode_flags, execute_run_command};
#[cfg(test)]
pub(crate) use crate::install::support::{LocalRunManifestPreparationOutcome, ResolvedRunTarget};
use crate::reporters;
use crate::{CompatibilityFallbackBackend, EnforcementMode, GitHubAutoFixMode, RunAgentMode};

pub(crate) struct RunLikeCommandArgs {
    pub(crate) path: PathBuf,
    pub(crate) target: Option<String>,
    pub(crate) args: Vec<String>,
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
    pub(crate) auto_fix_mode: Option<GitHubAutoFixMode>,
    pub(crate) allow_unverified: bool,
    pub(crate) read: Vec<String>,
    pub(crate) write: Vec<String>,
    pub(crate) read_write: Vec<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) deprecation_warning: Option<&'static str>,
    pub(crate) reporter: Arc<reporters::CliReporter>,
}

pub(crate) fn execute_run_like_command(args: RunLikeCommandArgs) -> Result<()> {
    if let Some(warning) = args.deprecation_warning {
        eprintln!("{warning}");
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
        args.path,
        args.target,
        args.args,
        args.watch,
        args.background,
        args.nacelle,
        args.registry,
        effective_enforcement,
        sandbox_requested,
        args.dangerously_skip_permissions,
        args.compatibility_fallback
            .map(CompatibilityFallbackBackend::as_str)
            .map(str::to_string),
        args.yes,
        args.agent_mode,
        None,
        args.keep_failed_artifacts,
        args.auto_fix_mode,
        args.allow_unverified,
        args.read,
        args.write,
        args.read_write,
        args.cwd,
        args.state,
        args.inject,
        args.reporter,
    )
}

#[cfg(test)]
mod tests {
    use capsule_core::ato_lock::{self, AtoLock};
    use serde_json::json;

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
            tmp.path().join(".ato/tmp/run-invalid-manifests")
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
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        let resolved = ResolvedRunTarget {
            path: tmp.path().to_path_buf(),
            agent_local_root: Some(tmp.path().to_path_buf()),
            desktop_open_path: None,
            export_request: None,
            provider_workspace: None,
        };
        let reporter = Arc::new(crate::reporters::CliReporter::new(true));

        let outcome =
            crate::install::support::ensure_local_manifest_ready_for_run(&resolved, true, reporter)
                .expect("run");
        assert_eq!(outcome, LocalRunManifestPreparationOutcome::Ready);
        assert!(!tmp.path().join("capsule.toml").exists());
        assert!(!tmp.path().join("ato.lock.json").exists());
    }

    #[test]
    fn canonical_lock_input_skips_legacy_manifest_generation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "node", "cmd": ["index.js"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([{"name": "main", "process": {"entrypoint": "node", "cmd": ["index.js"]}}]),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "node", "version": "20.11.0"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {"label": "default", "runtime": "source", "driver": "node", "entrypoint": "node", "cmd": ["index.js"], "compatible": true}
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete", "observed_lockfiles": []}),
        );
        ato_lock::write_pretty_to_path(&lock, &tmp.path().join("ato.lock.json")).expect("lock");
        let resolved = ResolvedRunTarget {
            path: tmp.path().join("ato.lock.json"),
            agent_local_root: Some(tmp.path().to_path_buf()),
            desktop_open_path: None,
            export_request: None,
            provider_workspace: None,
        };
        let reporter = Arc::new(crate::reporters::CliReporter::new(true));

        let outcome =
            crate::install::support::ensure_local_manifest_ready_for_run(&resolved, true, reporter)
                .expect("run");
        assert_eq!(outcome, LocalRunManifestPreparationOutcome::Ready);
        assert!(!tmp.path().join("capsule.toml").exists());
    }
}
