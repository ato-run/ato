use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use capsule_core::execution_identity::{ExecutionReceipt, ReproducibilityClass, TrackingStatus};

use crate::application::execution_receipts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayMode {
    Strict,
    BestEffort,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayPlan {
    pub(crate) receipt: ExecutionReceipt,
    pub(crate) mode: ReplayMode,
    pub(crate) run_path: PathBuf,
    pub(crate) target: Option<String>,
    pub(crate) entry: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) args: Vec<String>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn plan_replay(execution_id: &str, mode: ReplayMode) -> Result<ReplayPlan> {
    let receipt = execution_receipts::read_receipt(execution_id)?;
    validate_same_host_source(&receipt)?;
    if mode == ReplayMode::Strict {
        validate_strict_receipt(&receipt)?;
    }
    let run_path = source_run_path(&receipt)?;
    let cwd = replay_cwd(&receipt)?;
    validate_same_host_cwd(&run_path, cwd.as_ref())?;
    let warnings = replay_warnings(&receipt);
    let args = receipt.launch.argv.clone();
    let entry = if receipt.launch.entry_point.trim().is_empty() {
        None
    } else {
        Some(receipt.launch.entry_point.clone())
    };
    Ok(ReplayPlan {
        receipt,
        mode,
        run_path,
        target: None,
        entry,
        cwd,
        args,
        warnings,
    })
}

fn validate_strict_receipt(receipt: &ExecutionReceipt) -> Result<()> {
    if receipt.reproducibility.class != ReproducibilityClass::Pure {
        bail!(
            "strict replay requires a pure execution receipt; {} is {:?} with causes: {}",
            receipt.execution_id,
            receipt.reproducibility.class,
            receipt
                .reproducibility
                .causes
                .iter()
                .map(|cause| format!("{cause:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

fn validate_same_host_source(receipt: &ExecutionReceipt) -> Result<()> {
    if receipt.source.source_ref.value.as_deref().is_none() {
        bail!(
            "execution receipt {} has no local source reference",
            receipt.execution_id
        );
    }
    if !receipt
        .source
        .source_ref
        .value
        .as_deref()
        .is_some_and(|source_ref| source_ref.starts_with("local:"))
    {
        bail!(
            "same-host replay only supports local source receipts; got {:?}",
            receipt.source.source_ref.value
        );
    }
    Ok(())
}

fn source_run_path(receipt: &ExecutionReceipt) -> Result<PathBuf> {
    let source_ref = receipt
        .source
        .source_ref
        .value
        .as_deref()
        .context("execution receipt source_ref is missing")?;
    let raw = source_ref
        .strip_prefix("local:")
        .context("same-host replay source_ref must start with local:")?;
    let path = PathBuf::from(raw);
    if path
        .file_name()
        .is_some_and(|name| name == "capsule.toml" || name == "ato.toml")
    {
        Ok(path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")))
    } else {
        Ok(path)
    }
}

fn replay_cwd(receipt: &ExecutionReceipt) -> Result<Option<PathBuf>> {
    if receipt.launch.working_directory.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(&receipt.launch.working_directory)))
}

fn validate_same_host_cwd(run_path: &PathBuf, cwd: Option<&PathBuf>) -> Result<()> {
    let Some(cwd) = cwd else {
        return Ok(());
    };
    if cwd.exists() {
        return Ok(());
    }
    if cwd.is_absolute() {
        bail!(
            "receipt working directory does not exist on this host: {}",
            cwd.display()
        );
    }
    let candidate = run_path.join(cwd);
    if candidate.exists() {
        return Ok(());
    }
    bail!(
        "receipt working directory does not exist on this host: {}",
        candidate.display()
    );
}

fn replay_warnings(receipt: &ExecutionReceipt) -> Vec<String> {
    let mut warnings = Vec::new();
    if receipt.source.source_tree_hash.status != TrackingStatus::Known {
        warnings.push("source tree hash was not known in the original receipt".to_string());
    }
    if receipt.dependencies.derivation_hash.status != TrackingStatus::Known {
        warnings
            .push("dependency derivation hash was not known in the original receipt".to_string());
    }
    if receipt.dependencies.output_hash.status != TrackingStatus::Known {
        warnings.push("dependency output hash was not known in the original receipt".to_string());
    }
    if receipt.runtime.binary_hash.status != TrackingStatus::Known {
        warnings.push("runtime binary hash was not known in the original receipt".to_string());
    }
    if receipt.environment.closure_hash.status != TrackingStatus::Known {
        warnings.push("environment closure hash was not known in the original receipt".to_string());
    }
    if receipt.filesystem.view_hash.status != TrackingStatus::Known {
        warnings.push("filesystem view hash was not known in the original receipt".to_string());
    }
    warnings
}

#[cfg(test)]
mod tests {
    use capsule_core::execution_identity::{
        DependencyIdentity, EnvironmentIdentity, EnvironmentMode, ExecutionIdentityInput,
        FilesystemIdentity, LaunchIdentity, PlatformIdentity, PolicyIdentity, ReproducibilityCause,
        ReproducibilityIdentity, RuntimeIdentity, SourceIdentity, Tracked,
    };

    use super::*;

    #[test]
    fn source_run_path_uses_manifest_parent() {
        let receipt = receipt_with_source("local:/workspace/app/capsule.toml");

        let path = source_run_path(&receipt).expect("source path");

        assert_eq!(path, PathBuf::from("/workspace/app"));
    }

    #[test]
    fn plan_replay_extracts_launch_envelope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let receipt = receipt_with_source(&format!(
            "local:{}",
            temp.path().join("capsule.toml").display()
        ));

        let cwd = replay_cwd(&receipt).expect("cwd");

        assert_eq!(cwd, Some(PathBuf::from("/workspace/app")));
        assert_eq!(receipt.launch.entry_point, "node");
        assert_eq!(receipt.launch.argv, vec!["server.js".to_string()]);
    }

    #[test]
    fn replay_warns_when_components_are_not_known() {
        let mut receipt = receipt_with_source("local:/workspace/app/capsule.toml");
        receipt.dependencies.output_hash = Tracked::unknown("missing");
        receipt.filesystem.view_hash = Tracked::untracked("partial");

        let warnings = replay_warnings(&receipt);

        assert!(warnings
            .iter()
            .any(|warning| warning.contains("dependency output hash")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("filesystem view hash")));
    }

    #[test]
    fn same_host_replay_rejects_remote_sources() {
        let receipt = receipt_with_source("github.com/acme/app@abc");

        let err = validate_same_host_source(&receipt).expect_err("remote should fail");

        assert!(err.to_string().contains("same-host replay only supports"));
    }

    #[test]
    fn strict_replay_rejects_best_effort_receipts() {
        let mut receipt = receipt_with_source("local:/workspace/app/capsule.toml");
        receipt.reproducibility = ReproducibilityIdentity {
            class: ReproducibilityClass::BestEffort,
            causes: vec![ReproducibilityCause::UnknownDependencyOutput],
        };

        let err = validate_strict_receipt(&receipt).expect_err("best effort should fail");

        assert!(err.to_string().contains("strict replay requires"));
    }

    fn receipt_with_source(source_ref: &str) -> ExecutionReceipt {
        ExecutionReceipt::from_input(
            ExecutionIdentityInput::new(
                SourceIdentity {
                    source_ref: Tracked::known(source_ref.to_string()),
                    source_tree_hash: Tracked::known("blake3:source".to_string()),
                },
                DependencyIdentity {
                    derivation_hash: Tracked::known("blake3:deps-in".to_string()),
                    output_hash: Tracked::known("blake3:deps-out".to_string()),
                },
                RuntimeIdentity {
                    declared: Some("node@20".to_string()),
                    resolved: Some("/usr/bin/node".to_string()),
                    binary_hash: Tracked::known("blake3:runtime".to_string()),
                    dynamic_linkage: Tracked::known("darwin".to_string()),
                    platform: PlatformIdentity {
                        os: "macos".to_string(),
                        arch: "arm64".to_string(),
                        libc: "darwin".to_string(),
                    },
                },
                EnvironmentIdentity {
                    closure_hash: Tracked::known("blake3:env".to_string()),
                    mode: EnvironmentMode::Closed,
                    tracked_keys: Vec::new(),
                    redacted_keys: Vec::new(),
                    unknown_keys: Vec::new(),
                },
                FilesystemIdentity {
                    view_hash: Tracked::known("blake3:fs".to_string()),
                    projection_strategy: "direct".to_string(),
                    writable_dirs: Vec::new(),
                    persistent_state: Vec::new(),
                    known_readonly_layers: Vec::new(),
                },
                PolicyIdentity {
                    network_policy_hash: Tracked::known("blake3:network".to_string()),
                    capability_policy_hash: Tracked::known("blake3:capability".to_string()),
                },
                LaunchIdentity {
                    entry_point: "node".to_string(),
                    argv: vec!["server.js".to_string()],
                    working_directory: "/workspace/app".to_string(),
                },
                ReproducibilityIdentity {
                    class: ReproducibilityClass::Pure,
                    causes: Vec::new(),
                },
            ),
            "2026-05-03T00:00:00Z".to_string(),
        )
        .expect("receipt")
    }
}
