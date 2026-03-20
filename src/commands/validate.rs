use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::lockfile::{
    manifest_external_capsule_dependencies, parse_lockfile_text, resolve_existing_lockfile_path,
    verify_lockfile_external_dependencies, CAPSULE_LOCK_FILE_NAME,
};
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
    let raw_manifest_text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let raw_manifest: toml::Value = toml::from_str(&raw_manifest_text)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    let decision = capsule_core::router::route_manifest(
        &manifest_path,
        capsule_core::router::ExecutionProfile::Release,
        None,
    )?;
    let targets_to_validate = decision.plan.selected_target_package_order()?;
    for target_label in &targets_to_validate {
        capsule_core::diagnostics::manifest::validate_manifest_for_build(
            &manifest_path,
            target_label,
        )?;
    }

    let lockfile_path = manifest_path
        .parent()
        .and_then(resolve_existing_lockfile_path);
    let lockfile_checked = if let Some(lockfile_path) = lockfile_path.as_ref() {
        capsule_core::lockfile::verify_lockfile_manifest(&manifest_path, lockfile_path).map_err(
            |err| {
                if err.to_string().contains("manifest hash mismatch") {
                    AtoExecutionError::lockfile_tampered(
                        err.to_string(),
                        Some(CAPSULE_LOCK_FILE_NAME),
                    )
                } else {
                    AtoExecutionError::policy_violation(err.to_string())
                }
            },
        )?;
        let raw = std::fs::read_to_string(lockfile_path)?;
        let lockfile = parse_lockfile_text(&raw, lockfile_path)?;
        verify_lockfile_external_dependencies(&decision.plan.manifest, &lockfile)?;
        true
    } else {
        let external_dependencies =
            manifest_external_capsule_dependencies(&decision.plan.manifest)?;
        if !external_dependencies.is_empty() {
            return Err(AtoExecutionError::lock_incomplete(
                "external capsule dependencies require capsule.lock.json",
                Some(CAPSULE_LOCK_FILE_NAME),
            )
            .into());
        }
        false
    };

    let ipc_diagnostics = crate::ipc::validate::validate_manifest(
        &raw_manifest,
        manifest_path.parent().unwrap_or_else(|| Path::new(".")),
    )
    .map_err(|err| {
        AtoExecutionError::execution_contract_invalid(
            format!("IPC validation failed: {err}"),
            None,
            None,
        )
    })?;
    if crate::ipc::validate::has_errors(&ipc_diagnostics) {
        return Err(AtoExecutionError::execution_contract_invalid(
            crate::ipc::validate::format_diagnostics(&ipc_diagnostics),
            None,
            None,
        )
        .into());
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
            println!("  {}: verified", CAPSULE_LOCK_FILE_NAME);
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
