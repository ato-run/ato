use std::collections::BTreeMap;
use std::path::{Component, PathBuf};

use anyhow::Result;
use capsule_core::ato_lock::{AtoLock, UnresolvedReason, UnresolvedValue};
use serde_json::{json, Value};

use crate::application::engine::build::native_delivery::{
    imported_native_artifact_closure, imported_native_artifact_delivery_contract,
    imported_native_artifact_type, native_delivery_draft_contract_from_manifest,
};

use super::compiler::CompatibilityCompilerInput;
use super::diagnostics::{
    CompatibilityDiagnostic, CompatibilityDiagnosticCode, CompatibilityDiagnosticSeverity,
};
use super::provenance::{CompilerOwnedField, ProvenanceKind, ProvenanceRecord};

pub(super) struct ManifestImportResult {
    pub(super) draft_lock: AtoLock,
    pub(super) diagnostics: Vec<CompatibilityDiagnostic>,
    pub(super) provenance: Vec<ProvenanceRecord>,
}

pub(super) fn import_manifest(
    input: &CompatibilityCompilerInput<'_>,
) -> Result<ManifestImportResult> {
    let manifest = input.manifest;
    let mut draft_lock = AtoLock::default();
    let mut diagnostics = Vec::new();
    let mut provenance = Vec::new();

    let metadata = json!({
        "name": manifest.model.name,
        "version": manifest.model.version,
        "capsule_type": manifest.model.capsule_type,
        "default_target": manifest.model.default_target,
    });
    draft_lock
        .contract
        .entries
        .insert("metadata".to_string(), metadata);
    provenance.push(ProvenanceRecord::new(
        CompilerOwnedField::new("contract", "metadata"),
        ProvenanceKind::ManifestExplicit,
        Some(manifest.path.as_path()),
        Some("name/version/default_target"),
        None,
    ));

    let workloads = import_workloads(input, &mut draft_lock, &mut diagnostics, &mut provenance)?;
    draft_lock
        .contract
        .entries
        .insert("workloads".to_string(), Value::Array(workloads));

    if let Some(network) = manifest.model.network.as_ref() {
        draft_lock.contract.entries.insert(
            "network".to_string(),
            serde_json::to_value(network).expect("network serializable"),
        );
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("contract", "network"),
            ProvenanceKind::ManifestExplicit,
            Some(manifest.path.as_path()),
            Some("network"),
            None,
        ));
    }

    if !manifest.model.storage.volumes.is_empty() || !manifest.model.state.is_empty() {
        draft_lock.contract.entries.insert(
            "storage".to_string(),
            json!({
                "volumes": manifest.model.storage.volumes,
                "state": manifest.model.state,
            }),
        );
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("contract", "storage"),
            ProvenanceKind::ManifestExplicit,
            Some(manifest.path.as_path()),
            Some("storage/state"),
            Some("stateful requirements remain contract-scoped in compatibility import"),
        ));
    }

    let resolved_targets =
        import_target_hints(input, &mut draft_lock, &mut diagnostics, &mut provenance)?;
    draft_lock.resolution.entries.insert(
        "resolved_targets".to_string(),
        Value::Array(resolved_targets),
    );
    draft_lock.resolution.entries.insert(
        "target_selection".to_string(),
        json!({
            "default_target": manifest.model.default_target,
            "source": "manifest",
        }),
    );
    provenance.push(ProvenanceRecord::new(
        CompilerOwnedField::new("resolution", "target_selection"),
        ProvenanceKind::ManifestExplicit,
        Some(manifest.path.as_path()),
        Some("default_target"),
        None,
    ));

    let imported_artifact_closure = import_native_artifact_closure(input)?;
    if let Some(delivery) =
        import_native_delivery_contract(input, imported_artifact_closure.as_ref())?
    {
        draft_lock
            .contract
            .entries
            .insert("delivery".to_string(), delivery);
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("contract", "delivery"),
            ProvenanceKind::CompilerInferred,
            Some(manifest.path.as_path()),
            Some("targets.<default_target>"),
            Some(
                "desktop native delivery mode is recorded in contract.delivery so source-derivation and artifact-import remain distinct",
            ),
        ));
    }

    if let Some(closure) = imported_artifact_closure {
        draft_lock
            .resolution
            .entries
            .insert("closure".to_string(), closure);
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("resolution", "closure"),
            ProvenanceKind::CompilerInferred,
            Some(manifest.path.as_path()),
            Some("targets.<default_target>.entrypoint"),
            Some(
                "imported_artifact_closure derived from an existing native artifact on disk; provenance is intentionally limited",
            ),
        ));
    }

    Ok(ManifestImportResult {
        draft_lock,
        diagnostics,
        provenance,
    })
}

fn import_native_delivery_contract(
    input: &CompatibilityCompilerInput<'_>,
    imported_artifact_closure: Option<&Value>,
) -> Result<Option<Value>> {
    if imported_artifact_closure.is_some() {
        let manifest = input.manifest;
        let target = manifest.model.resolve_default_target()?;
        let entrypoint = {
            let ep = target.entrypoint.trim();
            if ep.is_empty() {
                target.run_command.as_deref().map(str::trim).unwrap_or("")
            } else {
                ep
            }
        };
        if entrypoint.is_empty() {
            return Ok(None);
        }
        return Ok(Some(imported_native_artifact_delivery_contract(
            &PathBuf::from(entrypoint),
            "macos_app_bundle",
        )));
    }

    native_delivery_draft_contract_from_manifest(input.manifest.path.as_path())
}

fn import_workloads(
    input: &CompatibilityCompilerInput<'_>,
    draft_lock: &mut AtoLock,
    diagnostics: &mut Vec<CompatibilityDiagnostic>,
    provenance: &mut Vec<ProvenanceRecord>,
) -> Result<Vec<Value>> {
    let manifest = input.manifest;
    let mut workloads = Vec::new();

    if let Some(services) = manifest.model.services.as_ref() {
        let mut names = services.keys().cloned().collect::<Vec<_>>();
        names.sort();

        for name in names {
            let service = services.get(&name).expect("service present");
            workloads.push(json!({
                "name": name,
                "target": service.target,
                "process": {
                    "entrypoint": service.entrypoint,
                    "env": service.env.clone().unwrap_or_default(),
                },
                "depends_on": service.depends_on.clone().unwrap_or_default(),
                "readiness_probe": service.readiness_probe,
            }));
        }

        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("contract", "workloads"),
            ProvenanceKind::ManifestExplicit,
            Some(manifest.path.as_path()),
            Some("services"),
            None,
        ));

        if workloads.len() == 1 {
            draft_lock.contract.entries.insert(
                "process".to_string(),
                workloads[0]
                    .get("process")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            );
            provenance.push(ProvenanceRecord::new(
                CompilerOwnedField::new("contract", "process"),
                ProvenanceKind::CompilerInferred,
                Some(manifest.path.as_path()),
                Some("services.<single>.entrypoint"),
                Some("single imported workload selected as primary process"),
            ));
        } else {
            draft_lock.contract.unresolved.push(UnresolvedValue {
                field: Some("contract.process".to_string()),
                reason: UnresolvedReason::ExplicitSelectionRequired,
                detail: Some(
                    "multiple imported workloads exist; contract.process is intentionally unresolved"
                        .to_string(),
                ),
                candidates: workloads
                    .iter()
                    .filter_map(|workload| workload.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect(),
            });
            diagnostics.push(CompatibilityDiagnostic::new(
                CompatibilityDiagnosticCode::PrimaryProcessUnresolved,
                CompatibilityDiagnosticSeverity::Warning,
                "contract.process",
                "multiple services were imported; no deterministic primary process was selected",
                Some(manifest.path.as_path()),
            ));
        }

        return Ok(workloads);
    }

    let default_target = manifest.model.resolve_default_target()?;
    let synthesized_name = manifest.model.default_target.clone();
    let synthesized = json!({
        "name": synthesized_name,
        "target": manifest.model.default_target,
        "process": {
            "entrypoint": default_target.entrypoint,
            "run_command": default_target.run_command,
            "env": default_target.env,
            "required_env": default_target.required_env,
        },
        "depends_on": [],
    });

    draft_lock.contract.entries.insert(
        "process".to_string(),
        synthesized
            .get("process")
            .cloned()
            .unwrap_or_else(|| json!({})),
    );
    provenance.push(ProvenanceRecord::new(
        CompilerOwnedField::new("contract", "process"),
        ProvenanceKind::CompilerInferred,
        Some(manifest.path.as_path()),
        Some("targets.<default_target>"),
        Some("single-process compatibility project synthesized from default target"),
    ));
    provenance.push(ProvenanceRecord::new(
        CompilerOwnedField::new("contract", "workloads"),
        ProvenanceKind::CompilerInferred,
        Some(manifest.path.as_path()),
        Some("default_target"),
        Some("single-process project synthesized into one workload for downstream consistency"),
    ));
    workloads.push(synthesized);
    Ok(workloads)
}

fn import_target_hints(
    input: &CompatibilityCompilerInput<'_>,
    draft_lock: &mut AtoLock,
    diagnostics: &mut Vec<CompatibilityDiagnostic>,
    provenance: &mut Vec<ProvenanceRecord>,
) -> Result<Vec<Value>> {
    let manifest = input.manifest;
    let Some(targets) = manifest.model.targets.as_ref() else {
        draft_lock.resolution.unresolved.push(UnresolvedValue {
            field: Some("resolution.resolved_targets".to_string()),
            reason: UnresolvedReason::InsufficientEvidence,
            detail: Some("manifest has no targets to lift into resolution hints".to_string()),
            candidates: Vec::new(),
        });
        diagnostics.push(CompatibilityDiagnostic::new(
            CompatibilityDiagnosticCode::MissingTargets,
            CompatibilityDiagnosticSeverity::Error,
            "resolution.resolved_targets",
            "compatibility manifest did not contain any targets to import",
            Some(manifest.path.as_path()),
        ));
        return Ok(Vec::new());
    };

    let mut target_names = targets.named.keys().cloned().collect::<Vec<_>>();
    target_names.sort();

    let mut runtime_hints = BTreeMap::new();
    let mut imported = Vec::new();
    for name in target_names {
        let target = targets.named.get(&name).expect("target present");
        imported.push(json!({
            "label": name,
            "runtime": target.runtime,
            "driver": target.driver,
            "language": target.language,
            "runtime_version": target.runtime_version,
            "image": target.image,
            "component": target.component,
            "entrypoint": target.entrypoint,
            "run_command": target.run_command,
            "runtime_tools": target.runtime_tools,
            "cmd": target.cmd,
            "required_env": target.required_env,
            "port": target.port,
            "working_dir": target.working_dir,
        }));

        if let Some(driver) = target.driver.as_deref() {
            if let Some(version) = target.runtime_version.as_deref() {
                runtime_hints.insert(driver.to_string(), Value::String(version.to_string()));
            }
        }
    }

    if !runtime_hints.is_empty() {
        draft_lock.resolution.entries.insert(
            "runtime_hints".to_string(),
            Value::Object(runtime_hints.into_iter().collect()),
        );
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("resolution", "runtime_hints"),
            ProvenanceKind::ManifestExplicit,
            Some(manifest.path.as_path()),
            Some("targets.<label>.runtime_version"),
            None,
        ));
    }

    provenance.push(ProvenanceRecord::new(
        CompilerOwnedField::new("resolution", "resolved_targets"),
        ProvenanceKind::ManifestExplicit,
        Some(manifest.path.as_path()),
        Some("targets"),
        Some("manifest target definitions lifted as resolution-owned hints"),
    ));
    Ok(imported)
}

fn import_native_artifact_closure(input: &CompatibilityCompilerInput<'_>) -> Result<Option<Value>> {
    let manifest = input.manifest;
    let Some(targets) = manifest.model.targets.as_ref() else {
        return Ok(None);
    };
    let Some(target) = targets.named.get(&manifest.model.default_target) else {
        return Ok(None);
    };
    if target.driver.as_deref() != Some("native") {
        return Ok(None);
    }

    let entrypoint = {
        let ep = target.entrypoint.trim();
        if ep.is_empty() {
            target.run_command.as_deref().map(str::trim).unwrap_or("")
        } else {
            ep
        }
    };
    if entrypoint.is_empty() {
        return Ok(None);
    }

    let manifest_dir = manifest
        .path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let artifact_relative = PathBuf::from(entrypoint);
    if artifact_relative.is_absolute()
        || artifact_relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Ok(None);
    }

    let artifact_path = manifest_dir.join(&artifact_relative);
    let Some(artifact_type) = imported_native_artifact_type(&artifact_path) else {
        return Ok(None);
    };

    Ok(Some(imported_native_artifact_closure(
        &artifact_path,
        artifact_type,
    )?))
}
