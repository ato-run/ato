use std::collections::BTreeMap;

use capsule_core::ato_lock::{AtoLock, UnresolvedReason, UnresolvedValue};
use capsule_core::lockfile::{CapsuleLock, LockedCapsuleDependency};
use serde_json::Value;

use super::compiler::CompatibilityCompilerInput;
use super::diagnostics::{
    CompatibilityDiagnostic, CompatibilityDiagnosticCode, CompatibilityDiagnosticSeverity,
};
use super::provenance::{CompilerOwnedField, ProvenanceKind, ProvenanceRecord};

pub(super) struct LegacyLockImportResult {
    pub(super) resolution_entries: BTreeMap<String, Value>,
    pub(super) unresolved: Vec<UnresolvedValue>,
    pub(super) diagnostics: Vec<CompatibilityDiagnostic>,
    pub(super) provenance: Vec<ProvenanceRecord>,
}

pub(super) fn import_legacy_lock(
    input: &CompatibilityCompilerInput<'_>,
    manifest_draft: &AtoLock,
    legacy_lock: &CapsuleLock,
) -> LegacyLockImportResult {
    let mut resolution_entries = BTreeMap::new();
    let mut unresolved = Vec::new();
    let mut diagnostics = Vec::new();
    let mut provenance = Vec::new();

    if let Some(runtimes) = legacy_lock.runtimes.as_ref() {
        let value = serde_json::to_value(runtimes).expect("runtimes serializable");
        resolution_entries.insert("locked_runtimes".to_string(), value);
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("resolution", "locked_runtimes"),
            ProvenanceKind::LegacyLockResolved,
            input.legacy_lock_path,
            Some("runtimes"),
            None,
        ));
        compare_runtime_hints(
            input,
            manifest_draft,
            runtimes,
            &mut unresolved,
            &mut diagnostics,
        );
    }

    if let Some(tools) = legacy_lock.tools.as_ref() {
        resolution_entries.insert(
            "locked_tools".to_string(),
            serde_json::to_value(tools).expect("tools serializable"),
        );
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("resolution", "locked_tools"),
            ProvenanceKind::LegacyLockResolved,
            input.legacy_lock_path,
            Some("tools"),
            None,
        ));
    }

    if !legacy_lock.capsule_dependencies.is_empty() {
        resolution_entries.insert(
            "locked_dependencies".to_string(),
            serde_json::to_value(&legacy_lock.capsule_dependencies)
                .expect("capsule dependencies serializable"),
        );
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("resolution", "locked_dependencies"),
            ProvenanceKind::LegacyLockResolved,
            input.legacy_lock_path,
            Some("capsule_dependencies"),
            None,
        ));
    }

    if !legacy_lock.injected_data.is_empty() {
        resolution_entries.insert(
            "locked_injected_data".to_string(),
            serde_json::to_value(&legacy_lock.injected_data).expect("injected data serializable"),
        );
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("resolution", "locked_injected_data"),
            ProvenanceKind::LegacyLockResolved,
            input.legacy_lock_path,
            Some("injected_data"),
            Some("legacy injected data remains resolution-scoped and is not promoted to portable contract semantics"),
        ));
    }

    if !legacy_lock.targets.is_empty() {
        resolution_entries.insert(
            "locked_target_artifacts".to_string(),
            serde_json::to_value(&legacy_lock.targets).expect("targets serializable"),
        );
        provenance.push(ProvenanceRecord::new(
            CompilerOwnedField::new("resolution", "locked_target_artifacts"),
            ProvenanceKind::LegacyLockResolved,
            input.legacy_lock_path,
            Some("targets"),
            None,
        ));
    }

    compare_dependency_bindings(
        input,
        manifest_draft,
        &legacy_lock.capsule_dependencies,
        &mut unresolved,
        &mut diagnostics,
    );

    if resolution_entries.is_empty() {
        diagnostics.push(CompatibilityDiagnostic::new(
            CompatibilityDiagnosticCode::LegacyLockWithoutResolutionData,
            CompatibilityDiagnosticSeverity::Warning,
            "resolution",
            "legacy lock was present but did not contribute any resolution-owned data",
            input.legacy_lock_path,
        ));
    }

    LegacyLockImportResult {
        resolution_entries,
        unresolved,
        diagnostics,
        provenance,
    }
}

fn compare_runtime_hints(
    input: &CompatibilityCompilerInput<'_>,
    manifest_draft: &AtoLock,
    runtimes: &capsule_core::lockfile::RuntimeSection,
    unresolved: &mut Vec<UnresolvedValue>,
    diagnostics: &mut Vec<CompatibilityDiagnostic>,
) {
    let Some(runtime_hints) = manifest_draft
        .resolution
        .entries
        .get("runtime_hints")
        .and_then(Value::as_object)
    else {
        return;
    };

    let locked_versions = [
        (
            "deno",
            runtimes.deno.as_ref().map(|entry| entry.version.as_str()),
        ),
        (
            "node",
            runtimes.node.as_ref().map(|entry| entry.version.as_str()),
        ),
        (
            "python",
            runtimes.python.as_ref().map(|entry| entry.version.as_str()),
        ),
    ];

    for (runtime, locked_version) in locked_versions {
        let Some(locked_version) = locked_version else {
            continue;
        };
        let Some(hinted_version) = runtime_hints.get(runtime).and_then(Value::as_str) else {
            continue;
        };
        if hinted_version != locked_version {
            unresolved.push(UnresolvedValue {
                reason: UnresolvedReason::Ambiguity,
                detail: Some(format!(
                    "runtime version conflict for {runtime}: manifest hinted {hinted_version}, legacy lock resolved {locked_version}"
                )),
                candidates: vec![hinted_version.to_string(), locked_version.to_string()],
            });
            diagnostics.push(CompatibilityDiagnostic::new(
                CompatibilityDiagnosticCode::LegacyRuntimeConflict,
                CompatibilityDiagnosticSeverity::Warning,
                "resolution.locked_runtimes",
                format!(
                    "runtime version conflict for {runtime}: manifest hinted {hinted_version}, legacy lock resolved {locked_version}"
                ),
                input.legacy_lock_path,
            ));
        }
    }
}

fn compare_dependency_bindings(
    input: &CompatibilityCompilerInput<'_>,
    manifest_draft: &AtoLock,
    locked_dependencies: &[LockedCapsuleDependency],
    unresolved: &mut Vec<UnresolvedValue>,
    diagnostics: &mut Vec<CompatibilityDiagnostic>,
) {
    let manifest_targets = manifest_draft
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut manifest_bindings = BTreeMap::new();
    for target in manifest_targets {
        let Some(runtime_tools) = target.get("runtime_tools") else {
            continue;
        };
        manifest_bindings.insert(
            target
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            runtime_tools.clone(),
        );
    }

    for dependency in locked_dependencies {
        if dependency.injection_bindings.is_empty() {
            continue;
        }

        if manifest_bindings.values().any(|bindings| {
            bindings
                .as_object()
                .map(|map| map.contains_key(&dependency.name))
                .unwrap_or(false)
        }) {
            unresolved.push(UnresolvedValue {
                reason: UnresolvedReason::Ambiguity,
                detail: Some(format!(
                    "legacy dependency '{}' overlaps with manifest runtime tooling names",
                    dependency.name
                )),
                candidates: vec![dependency.name.clone()],
            });
            diagnostics.push(CompatibilityDiagnostic::new(
                CompatibilityDiagnosticCode::LegacyTargetConflict,
                CompatibilityDiagnosticSeverity::Warning,
                "resolution.locked_dependencies",
                format!(
                    "legacy dependency '{}' overlaps with manifest-owned target hints; keeping contract untouched and recording explicit ambiguity",
                    dependency.name
                ),
                input.legacy_lock_path,
            ));
        }
    }
}
