use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use capsule_core::lockfile::{
    manifest_external_capsule_dependencies, verify_lockfile_external_dependencies,
    CAPSULE_LOCK_FILE_NAME,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::application::source_inference::{
    materialize_run_from_compatibility, materialize_run_from_source_only,
};
use crate::reporters::CliReporter;

#[derive(Debug, Clone, Serialize)]
pub struct ValidateResult {
    pub authoritative_input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_lock_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_label: Option<String>,
    pub lockfile_checked: bool,
    pub warnings: Vec<String>,
}

pub fn execute(path: PathBuf, json_output: bool) -> Result<ValidateResult> {
    let resolved = resolve_authoritative_input(&path, ResolveInputOptions::default())?;
    let reporter = Arc::new(CliReporter::new(false));
    let mut warnings = resolved
        .advisories()
        .iter()
        .map(|advisory| advisory.message.clone())
        .collect::<Vec<_>>();

    let result = match resolved {
        ResolvedInput::CanonicalLock {
            canonical,
            provenance,
            ..
        } => {
            let decision = capsule_core::router::route_lock(
                &canonical.path,
                &canonical.lock,
                &canonical.project_root,
                capsule_core::router::ExecutionProfile::Release,
                None,
            )?;

            ValidateResult {
                authoritative_input: provenance.selected_kind.as_str().to_string(),
                manifest_path: None,
                canonical_lock_path: Some(canonical.path),
                runtime: Some(format!("{:?}", decision.kind).to_lowercase()),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                lockfile_checked: false,
                warnings,
            }
        }
        ResolvedInput::CompatibilityProject {
            project,
            provenance,
            ..
        } => {
            let manifest_path = project.manifest.path.clone();
            let raw_manifest_text = std::fs::read_to_string(&manifest_path)
                .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
            let raw_manifest: toml::Value = toml::from_str(&raw_manifest_text)
                .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

            let materialized =
                materialize_run_from_compatibility(&project, None, reporter.clone(), true)?;
            let decision = capsule_core::router::route_lock(
                &materialized.lock_path,
                &materialized.lock,
                &project.project_root,
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

            let lockfile_checked = if let Some(legacy_lock) = project.legacy_lock.as_ref() {
                capsule_core::lockfile::verify_lockfile_manifest(&manifest_path, &legacy_lock.path)
                    .map_err(|err| {
                        if err.to_string().contains("manifest hash mismatch") {
                            AtoExecutionError::lockfile_tampered(
                                err.to_string(),
                                Some(CAPSULE_LOCK_FILE_NAME),
                            )
                        } else {
                            AtoExecutionError::policy_violation(err.to_string())
                        }
                    })?;
                verify_lockfile_external_dependencies(&decision.plan.manifest, &legacy_lock.lock)?;
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
            warnings.extend(
                ipc_diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.to_string()),
            );

            ValidateResult {
                authoritative_input: provenance.selected_kind.as_str().to_string(),
                manifest_path: Some(manifest_path),
                canonical_lock_path: None,
                runtime: Some(format!("{:?}", decision.kind).to_lowercase()),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                lockfile_checked,
                warnings,
            }
        }
        ResolvedInput::SourceOnly {
            source, provenance, ..
        } => {
            let materialized =
                materialize_run_from_source_only(&source, None, reporter.clone(), true)?;
            let decision = capsule_core::router::route_lock(
                &materialized.lock_path,
                &materialized.lock,
                &source.project_root,
                capsule_core::router::ExecutionProfile::Release,
                None,
            )?;

            ValidateResult {
                authoritative_input: provenance.selected_kind.as_str().to_string(),
                manifest_path: None,
                canonical_lock_path: Some(materialized.lock_path),
                runtime: Some(format!("{:?}", decision.kind).to_lowercase()),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                lockfile_checked: false,
                warnings,
            }
        }
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("✔ Input validation passed");
        println!("  Authoritative input: {}", result.authoritative_input);
        if let Some(path) = result.canonical_lock_path.as_ref() {
            println!("  Canonical lock: {}", path.display());
        }
        if let Some(path) = result.manifest_path.as_ref() {
            println!("  Manifest: {}", path.display());
        }
        if let Some(runtime) = result.runtime.as_ref() {
            println!("  Runtime: {}", runtime);
        }
        if let Some(target_label) = result.target_label.as_ref() {
            println!("  Target: {}", target_label);
        }
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
