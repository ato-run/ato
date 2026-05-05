use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use capsule_core::execution_identity::{
    ExecutionReceipt, ExecutionReceiptDocument, ReproducibilityClass, TrackingStatus,
};

use crate::application::execution_receipts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayMode {
    Strict,
    BestEffort,
}

/// Replay plan synthesized from either a v1 or v2 execution receipt.
/// Holds only the fields the executor needs to relaunch on the same
/// host. The original v1 receipt (when source was v1) is preserved as
/// `receipt` so existing code paths and tests that consume the v1 shape
/// continue to work; v2-only replays carry a synthesized v1-like view.
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
    let document = execution_receipts::read_receipt_document(execution_id)?;
    match document {
        ExecutionReceiptDocument::V1(receipt) => plan_replay_v1(receipt, mode),
        ExecutionReceiptDocument::V2(receipt) => plan_replay_v2(*Box::new(receipt), mode),
    }
}

fn plan_replay_v1(receipt: ExecutionReceipt, mode: ReplayMode) -> Result<ReplayPlan> {
    validate_same_host_source(&receipt)?;
    if mode == ReplayMode::Strict {
        validate_strict_receipt(&receipt)?;
    }
    let run_path = source_run_path(&receipt)?;
    let cwd = replay_cwd(&receipt)?;
    validate_same_host_cwd(&run_path, cwd.as_deref())?;
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

fn plan_replay_v2(
    receipt: capsule_core::execution_identity::ExecutionReceiptV2,
    mode: ReplayMode,
) -> Result<ReplayPlan> {
    let local = receipt.local.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "v2 same-host replay requires a `local` locator section; receipt {} lacks one",
            receipt.execution_id
        )
    })?;

    if mode == ReplayMode::Strict {
        // Strict replay requires Pure. host-bound is the practical
        // ceiling on macOS / Linux for executions that link against
        // any host library, and the plan's strict-replay reachability
        // bullet anticipates Pure-only fixtures (statically linked
        // runtimes). Future work may relax this to include HostBound
        // on the same host where the runtime binary hash + dynamic
        // linkage hash both match.
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
    }

    let run_path = local
        .workspace_root
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| {
            local
                .manifest_path
                .as_deref()
                .map(PathBuf::from)
                .and_then(|p| p.parent().map(PathBuf::from))
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "v2 receipt {} local locator does not carry a workspace_root or manifest_path",
                receipt.execution_id
            )
        })?;

    let cwd = local.working_directory_path.as_deref().map(PathBuf::from);
    validate_same_host_cwd(&run_path, cwd.as_deref())?;

    let warnings = v2_replay_warnings(&receipt);
    let entry = local
        .entry_point_raw
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let args = local.argv_raw.clone();

    // Synthesize a v1-compatible ExecutionReceipt view for downstream
    // consumers that still hold a v1 shape. We only fill in the fields
    // the executor and inspect path actually read; v1 callers that
    // compare execution_id / class / causes get the v2 values, so the
    // identity contract is preserved across the schema bridge.
    let synthetic_v1 = synthesize_v1_view(&receipt, local);

    Ok(ReplayPlan {
        receipt: synthetic_v1,
        mode,
        run_path,
        target: None,
        entry,
        cwd,
        args,
        warnings,
    })
}

fn v2_replay_warnings(
    receipt: &capsule_core::execution_identity::ExecutionReceiptV2,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if receipt.source.source_tree_hash.status != TrackingStatus::Known {
        warnings.push("source tree hash was not known in the original receipt".to_string());
    }
    if receipt.dependencies.derivation_hash.status != TrackingStatus::Known
        && receipt.dependencies.derivation_hash.status != TrackingStatus::NotApplicable
    {
        warnings
            .push("dependency derivation hash was not known in the original receipt".to_string());
    }
    if receipt.dependencies.output_hash.status != TrackingStatus::Known
        && receipt.dependencies.output_hash.status != TrackingStatus::NotApplicable
    {
        warnings.push("dependency output hash was not known in the original receipt".to_string());
    }
    if receipt.runtime.binary_hash.status != TrackingStatus::Known {
        warnings.push("runtime binary hash was not known in the original receipt".to_string());
    }
    if receipt.runtime.dynamic_linkage.status != TrackingStatus::Known {
        warnings.push("runtime dynamic linkage was not known in the original receipt".to_string());
    }
    warnings
}

fn synthesize_v1_view(
    receipt: &capsule_core::execution_identity::ExecutionReceiptV2,
    local: &capsule_core::execution_identity::LocalExecutionLocator,
) -> ExecutionReceipt {
    use capsule_core::execution_identity::{
        DependencyIdentity, EnvironmentIdentity, EnvironmentMode, ExecutionIdentityMetadata,
        FilesystemIdentity, LaunchIdentity, PolicyIdentity, RuntimeIdentity, SourceIdentity,
        Tracked,
    };
    ExecutionReceipt {
        schema_version: 1,
        execution_id: receipt.execution_id.clone(),
        computed_at: receipt.computed_at.clone(),
        identity: ExecutionIdentityMetadata {
            canonicalization: receipt.identity.canonicalization.clone(),
            hash_algorithm: receipt.identity.hash_algorithm.clone(),
            input_hash: receipt.identity.input_hash.clone(),
        },
        source: SourceIdentity {
            source_ref: Tracked::known(format!(
                "local:{}",
                local.manifest_path.as_deref().unwrap_or("?")
            )),
            source_tree_hash: receipt.source.source_tree_hash.clone(),
        },
        dependencies: DependencyIdentity {
            derivation_hash: receipt.dependencies.derivation_hash.clone(),
            output_hash: receipt.dependencies.output_hash.clone(),
        },
        runtime: RuntimeIdentity {
            declared: receipt.runtime.declared.clone(),
            resolved: local.runtime_resolved_path.clone(),
            binary_hash: receipt.runtime.binary_hash.clone(),
            dynamic_linkage: receipt.runtime.dynamic_linkage.clone(),
            platform: receipt.runtime.platform.clone(),
        },
        environment: EnvironmentIdentity {
            closure_hash: Tracked::untracked(
                "v2 environment closure does not project to v1 closure_hash",
            ),
            mode: match receipt.environment.mode {
                EnvironmentMode::Closed => EnvironmentMode::Closed,
                EnvironmentMode::Partial => EnvironmentMode::Partial,
                EnvironmentMode::Untracked => EnvironmentMode::Untracked,
            },
            tracked_keys: receipt
                .environment
                .entries
                .iter()
                .map(|entry| entry.key.clone())
                .collect(),
            redacted_keys: Vec::new(),
            unknown_keys: receipt.environment.ambient_untracked_keys.clone(),
        },
        filesystem: FilesystemIdentity {
            view_hash: receipt.filesystem.view_hash.clone(),
            projection_strategy: "v2-canonical".to_string(),
            writable_dirs: receipt
                .filesystem
                .writable_dirs
                .iter()
                .map(|d| d.role.clone())
                .collect(),
            persistent_state: receipt
                .filesystem
                .persistent_state
                .iter()
                .map(|s| format!("{}={}", s.name, s.identity.value.as_deref().unwrap_or("")))
                .collect(),
            known_readonly_layers: receipt
                .filesystem
                .readonly_layers
                .iter()
                .map(|l| l.role.clone())
                .collect(),
        },
        policy: PolicyIdentity {
            network_policy_hash: receipt.policy.network_policy_hash.clone(),
            capability_policy_hash: receipt.policy.capability_policy_hash.clone(),
            sandbox_policy_hash: receipt.policy.sandbox_policy_hash.clone(),
        },
        launch: LaunchIdentity {
            entry_point: local.entry_point_raw.clone().unwrap_or_default(),
            argv: local.argv_raw.clone(),
            working_directory: local.working_directory_path.clone().unwrap_or_default(),
        },
        reproducibility: receipt.reproducibility.clone(),
    }
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

fn validate_same_host_cwd(run_path: &Path, cwd: Option<&Path>) -> Result<()> {
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
                    sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
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
