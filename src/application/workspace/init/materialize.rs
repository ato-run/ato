use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::ato_lock::{self, AtoLock, UnresolvedReason, UnresolvedValue};
use capsule_core::input_resolver::ATO_LOCK_FILE_NAME;
use serde::Serialize;
use serde_json::Value;

use crate::application::source_inference::{
    write_sidecar, MaterializationMode, SourceInferenceInputKind, SourceInferenceProvenance,
    SourceInferenceProvenanceKind, SourceInferenceResult, WorkspaceMaterialization,
};

const INIT_SOURCE_INFERENCE_DIR: &str = ".ato/source-inference";
const BINDING_STATE_DIR: &str = ".ato/binding";
const PROVENANCE_FILE: &str = "provenance.json";
const PROVENANCE_CACHE_FILE: &str = "provenance-cache.json";
const BINDING_SEED_FILE: &str = "seed.json";

#[derive(Debug, Serialize)]
pub(crate) struct ProvenanceCache {
    schema_version: &'static str,
    input_kind: SourceInferenceInputKind,
    lock_path: PathBuf,
    provenance_path: PathBuf,
    binding_seed_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unresolved: Vec<CachedUnresolvedField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    field_index: Vec<CachedFieldRecord>,
    diagnostics_count: usize,
}

#[derive(Debug, Serialize)]
struct CachedUnresolvedField {
    field: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CachedFieldRecord {
    field: String,
    kinds: Vec<SourceInferenceProvenanceKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BindingSeed {
    schema_version: &'static str,
    lock_path: PathBuf,
    provenance_cache_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    entries: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unresolved: Vec<UnresolvedValue>,
}

pub(crate) fn materialize_workspace_result(
    project_root: &Path,
    mut result: SourceInferenceResult,
) -> Result<WorkspaceMaterialization> {
    ato_lock::recompute_lock_id(&mut result.lock)?;
    validate_durable_workspace_lock(&result.lock)?;

    let lock_path = project_root.join(ATO_LOCK_FILE_NAME);
    ato_lock::write_pretty_to_path(&result.lock, &lock_path)?;

    let sidecar_dir = project_root.join(INIT_SOURCE_INFERENCE_DIR);
    fs::create_dir_all(&sidecar_dir)
        .with_context(|| format!("Failed to create {}", sidecar_dir.display()))?;
    let sidecar_path = sidecar_dir.join(PROVENANCE_FILE);
    write_sidecar(&sidecar_path, &result, MaterializationMode::InitWorkspace)?;

    let provenance_cache_path = sidecar_dir.join(PROVENANCE_CACHE_FILE);
    let binding_seed_path = project_root.join(BINDING_STATE_DIR).join(BINDING_SEED_FILE);
    write_provenance_cache(
        &provenance_cache_path,
        &lock_path,
        &sidecar_path,
        &binding_seed_path,
        &result,
    )?;
    write_binding_seed(
        &binding_seed_path,
        &lock_path,
        &provenance_cache_path,
        &result.lock,
    )?;

    Ok(WorkspaceMaterialization {
        lock_path,
        sidecar_path,
        provenance_cache_path,
        binding_seed_path,
        result,
    })
}

pub(crate) fn validate_durable_workspace_lock(lock: &AtoLock) -> Result<()> {
    ato_lock::validate_structural(lock, ato_lock::ValidationMode::Strict)
        .map_err(|errors| anyhow::anyhow!(format_validation_errors(errors)))?;

    if !lock.binding.entries.is_empty() {
        anyhow::bail!(
            "durable workspace lock must not embed binding entries; initialize workspace-local binding seed instead"
        );
    }
    if !lock.attestations.entries.is_empty() {
        anyhow::bail!(
            "durable workspace lock must not embed attestations; attestations remain workspace-local by default"
        );
    }

    ensure_field_or_unresolved(
        lock.contract.entries.contains_key("process"),
        &lock.contract.unresolved,
        "contract.process",
    )?;
    ensure_field_or_unresolved(
        lock.resolution.entries.contains_key("runtime"),
        &lock.resolution.unresolved,
        "resolution.runtime",
    )?;
    ensure_field_or_unresolved(
        has_non_empty_array(lock.resolution.entries.get("resolved_targets")),
        &lock.resolution.unresolved,
        "resolution.resolved_targets",
    )?;
    ensure_field_or_unresolved(
        lock.resolution.entries.contains_key("closure"),
        &lock.resolution.unresolved,
        "resolution.closure",
    )?;

    Ok(())
}

pub(crate) fn export_provenance_cache(
    lock_path: &Path,
    provenance_path: &Path,
    binding_seed_path: &Path,
    result: &SourceInferenceResult,
) -> ProvenanceCache {
    ProvenanceCache {
        schema_version: "1",
        input_kind: result.input_kind,
        lock_path: lock_path.to_path_buf(),
        provenance_path: provenance_path.to_path_buf(),
        binding_seed_path: binding_seed_path.to_path_buf(),
        lock_id: result
            .lock
            .lock_id
            .as_ref()
            .map(|value| value.as_str().to_string()),
        generated_at: result.lock.generated_at.clone(),
        unresolved: collect_unresolved_fields(&result.lock),
        field_index: build_field_index(&result.provenance),
        diagnostics_count: result.diagnostics.len(),
    }
}

fn write_provenance_cache(
    provenance_cache_path: &Path,
    lock_path: &Path,
    provenance_path: &Path,
    binding_seed_path: &Path,
    result: &SourceInferenceResult,
) -> Result<()> {
    let cache = export_provenance_cache(lock_path, provenance_path, binding_seed_path, result);
    let raw = serde_json::to_string_pretty(&cache)
        .context("Failed to serialize workspace provenance cache")?;
    fs::write(provenance_cache_path, raw)
        .with_context(|| format!("Failed to write {}", provenance_cache_path.display()))
}

fn write_binding_seed(
    binding_seed_path: &Path,
    lock_path: &Path,
    provenance_cache_path: &Path,
    lock: &AtoLock,
) -> Result<()> {
    if let Some(parent) = binding_seed_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let seed = BindingSeed {
        schema_version: "1",
        lock_path: lock_path.to_path_buf(),
        provenance_cache_path: provenance_cache_path.to_path_buf(),
        lock_id: lock
            .lock_id
            .as_ref()
            .map(|value| value.as_str().to_string()),
        entries: BTreeMap::new(),
        unresolved: lock.binding.unresolved.clone(),
    };
    let raw = serde_json::to_string_pretty(&seed)
        .context("Failed to serialize workspace binding seed")?;
    fs::write(binding_seed_path, raw)
        .with_context(|| format!("Failed to write {}", binding_seed_path.display()))
}

fn ensure_field_or_unresolved(
    has_field: bool,
    unresolved: &[UnresolvedValue],
    field: &str,
) -> Result<()> {
    if has_field {
        return Ok(());
    }
    if unresolved.iter().any(is_reasoned_unresolved) {
        return Ok(());
    }
    anyhow::bail!(
        "durable workspace output must either resolve {field} or emit an inspectable unresolved marker"
    );
}

fn is_reasoned_unresolved(unresolved: &UnresolvedValue) -> bool {
    if !unresolved.reason.is_known() {
        return false;
    }
    let has_detail = unresolved
        .detail
        .as_deref()
        .map(str::trim)
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    let requires_candidates = matches!(
        unresolved.reason,
        UnresolvedReason::Ambiguity | UnresolvedReason::ExplicitSelectionRequired
    );

    has_detail && (!requires_candidates || !unresolved.candidates.is_empty())
}

fn has_non_empty_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .map(|entries| !entries.is_empty())
        .unwrap_or(false)
}

fn collect_unresolved_fields(lock: &AtoLock) -> Vec<CachedUnresolvedField> {
    let mut fields = Vec::new();
    fields.extend(
        lock.contract
            .unresolved
            .iter()
            .map(|value| CachedUnresolvedField {
                field: "contract".to_string(),
                reason: value.reason.as_str().into_owned(),
                detail: value.detail.clone(),
                candidates: value.candidates.clone(),
            }),
    );
    fields.extend(
        lock.resolution
            .unresolved
            .iter()
            .map(|value| CachedUnresolvedField {
                field: "resolution".to_string(),
                reason: value.reason.as_str().into_owned(),
                detail: value.detail.clone(),
                candidates: value.candidates.clone(),
            }),
    );
    fields.extend(
        lock.binding
            .unresolved
            .iter()
            .map(|value| CachedUnresolvedField {
                field: "binding".to_string(),
                reason: value.reason.as_str().into_owned(),
                detail: value.detail.clone(),
                candidates: value.candidates.clone(),
            }),
    );
    fields.extend(
        lock.policy
            .unresolved
            .iter()
            .map(|value| CachedUnresolvedField {
                field: "policy".to_string(),
                reason: value.reason.as_str().into_owned(),
                detail: value.detail.clone(),
                candidates: value.candidates.clone(),
            }),
    );
    fields.extend(
        lock.attestations
            .unresolved
            .iter()
            .map(|value| CachedUnresolvedField {
                field: "attestations".to_string(),
                reason: value.reason.as_str().into_owned(),
                detail: value.detail.clone(),
                candidates: value.candidates.clone(),
            }),
    );
    fields
}

fn build_field_index(provenance: &[SourceInferenceProvenance]) -> Vec<CachedFieldRecord> {
    let mut ordered: Vec<CachedFieldRecord> = Vec::new();
    for record in provenance {
        if let Some(existing) = ordered.iter_mut().find(|value| value.field == record.field) {
            if !existing.kinds.contains(&record.kind) {
                existing.kinds.push(record.kind);
            }
            if let Some(note) = record.note.as_ref() {
                if !existing.notes.contains(note) {
                    existing.notes.push(note.clone());
                }
            }
            continue;
        }

        ordered.push(CachedFieldRecord {
            field: record.field.clone(),
            kinds: vec![record.kind],
            notes: record.note.iter().cloned().collect(),
        });
    }
    ordered
}

fn format_validation_errors(errors: Vec<ato_lock::AtoLockValidationError>) -> String {
    errors
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::application::source_inference::{
        ApprovalGate, InferResult, ResolveResult, SelectionGate, SourceInferenceDiagnostic,
        SourceInferenceInputKind,
    };

    fn sample_result() -> SourceInferenceResult {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": []}),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "resolved_by": "test"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([{"label": "default", "runtime": "source", "driver": "deno", "compatible": true}]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "observed_lockfiles": ["deno.lock"]}),
        );
        SourceInferenceResult {
            input_kind: SourceInferenceInputKind::SourceEvidence,
            lock,
            provenance: vec![SourceInferenceProvenance {
                field: "contract.process".to_string(),
                kind: SourceInferenceProvenanceKind::DeterministicHeuristic,
                source_path: Some(PathBuf::from(".")),
                source_field: Some("package.json".to_string()),
                note: Some("detected start script".to_string()),
            }],
            diagnostics: vec![SourceInferenceDiagnostic {
                severity:
                    crate::application::source_inference::SourceInferenceDiagnosticSeverity::Warning,
                field: "contract.process".to_string(),
                message: "test warning".to_string(),
            }],
            infer: InferResult {
                candidate_sets: Vec::new(),
                unresolved: Vec::new(),
            },
            resolve: ResolveResult {
                resolved_process: true,
                resolved_runtime: true,
                resolved_target_compatibility: true,
                resolved_dependency_closure: true,
                unresolved: Vec::new(),
            },
            selection_gate: None::<SelectionGate>,
            approval_gate: None::<ApprovalGate>,
        }
    }

    #[test]
    fn materialize_workspace_writes_cache_and_binding_seed() {
        let dir = tempdir().expect("tempdir");
        let materialized = materialize_workspace_result(dir.path(), sample_result())
            .expect("materialize workspace");

        assert!(materialized.lock_path.exists());
        assert!(materialized.sidecar_path.exists());
        assert!(materialized.provenance_cache_path.exists());
        assert!(materialized.binding_seed_path.exists());
    }

    #[test]
    fn durable_workspace_lock_rejects_embedded_binding_entries() {
        let mut lock = AtoLock::default();
        lock.binding
            .entries
            .insert("host_port".to_string(), json!(3000));
        lock.contract.unresolved.push(UnresolvedValue {
            reason: UnresolvedReason::InsufficientEvidence,
            detail: Some("process not chosen".to_string()),
            candidates: Vec::new(),
        });
        lock.resolution.unresolved.push(UnresolvedValue {
            reason: UnresolvedReason::InsufficientEvidence,
            detail: Some("runtime not chosen".to_string()),
            candidates: Vec::new(),
        });

        let error = validate_durable_workspace_lock(&lock).expect_err("must reject binding state");
        assert!(error.to_string().contains("must not embed binding entries"));
    }

    #[test]
    fn durable_workspace_lock_requires_unresolved_marker_when_process_missing() {
        let mut lock = AtoLock::default();
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "resolved_by": "test"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([{"label": "default"}]),
        );
        lock.resolution
            .entries
            .insert("closure".to_string(), json!({"kind": "metadata_only"}));

        let error = validate_durable_workspace_lock(&lock)
            .expect_err("missing process without unresolved marker must fail");
        assert!(error
            .to_string()
            .contains("either resolve contract.process or emit an inspectable unresolved marker"));
    }
}
