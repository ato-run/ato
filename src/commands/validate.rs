#![allow(dead_code)]

use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ValidateResult {
    pub manifest_path: PathBuf,
    pub runtime: String,
    pub target_label: String,
    pub lockfile_checked: bool,
    pub warnings: Vec<String>,
}

pub fn execute(path: PathBuf, json_output: bool) -> Result<ValidateResult> {
    let manifest_path = resolve_manifest_path(&path)?;

    let decision = capsule_core::router::route_manifest(
        &manifest_path,
        capsule_core::router::ExecutionProfile::Release,
        None,
    )?;
    capsule_core::diagnostics::manifest::validate_manifest_for_build(
        &manifest_path,
        decision.plan.selected_target_label(),
    )?;

    let lockfile_path = manifest_path
        .parent()
        .map(|parent| parent.join("capsule.lock"))
        .unwrap_or_else(|| PathBuf::from("capsule.lock"));
    let lockfile_checked = if lockfile_path.exists() {
        capsule_core::lockfile::verify_lockfile_manifest(&manifest_path, &lockfile_path).map_err(
            |err| {
                if err.to_string().contains("manifest hash mismatch") {
                    AtoExecutionError::lockfile_tampered(err.to_string(), Some("capsule.lock"))
                } else {
                    AtoExecutionError::policy_violation(err.to_string())
                }
            },
        )?;
        true
    } else {
        false
    };

    let ipc_diagnostics = crate::ipc::validate::validate_manifest(
        &decision.plan.manifest,
        &decision.plan.manifest_dir,
    )
    .map_err(|err| AtoExecutionError::policy_violation(format!("IPC validation failed: {err}")))?;
    if crate::ipc::validate::has_errors(&ipc_diagnostics) {
        return Err(
            AtoExecutionError::policy_violation(crate::ipc::validate::format_diagnostics(
                &ipc_diagnostics,
            ))
            .into(),
        );
    }

    let result = ValidateResult {
        manifest_path,
        runtime: format!("{:?}", decision.kind).to_lowercase(),
        target_label: decision.plan.selected_target_label().to_string(),
        lockfile_checked,
        warnings: ipc_diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.to_string())
            .collect(),
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("✔ Manifest validation passed");
        println!("  Manifest: {}", result.manifest_path.display());
        println!("  Runtime: {}", result.runtime);
        println!("  Target: {}", result.target_label);
        if result.lockfile_checked {
            println!("  capsule.lock: verified");
        }
        if result.warnings.is_empty() {
            println!("  IPC: no warnings");
        } else {
            println!("  IPC warnings:");
            for warning in &result.warnings {
                println!("    {}", warning.replace('\n', "\n    "));
            }
        }
    }

    Ok(result)
}

fn resolve_manifest_path(path: &Path) -> Result<PathBuf> {
    let manifest_path = if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path.to_path_buf()
    };

    if !manifest_path.exists() {
        anyhow::bail!("capsule.toml not found at {}", manifest_path.display());
    }

    manifest_path.canonicalize().with_context(|| {
        format!(
            "Failed to resolve manifest path: {}",
            manifest_path.display()
        )
    })
}
