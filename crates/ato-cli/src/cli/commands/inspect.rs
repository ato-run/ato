use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Error as AnyhowError, Result};
use capsule_core::ato_lock::{closure_info, AtoLock, UnresolvedValue};
use capsule_core::common::paths::ato_runs_dir;
use capsule_core::execution_identity::{
    ExecutionReceipt, ReproducibilityCause, ReproducibilityClass, Tracked, TrackingStatus,
};
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput, ATO_LOCK_FILE_NAME,
};
use capsule_core::manifest;
use capsule_core::types::{
    CapsuleManifest, EgressIdType, ServiceSpec, StateAttach, StateDurability, StateKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::application::compat_import::{
    CompatibilityCompileResult, CompatibilityDiagnosticSeverity,
};
use crate::application::execution_receipts;
use crate::application::source_inference::{
    self, CanonicalLockInput, MaterializationMode, SourceEvidenceInput, SourceInferenceDiagnostic,
    SourceInferenceDiagnosticSeverity, SourceInferenceInput, SourceInferenceInputKind,
    SourceInferenceProvenance, SourceInferenceProvenanceKind, SourceInferenceResult,
};
use crate::reporters::CliReporter;

const REQUIREMENTS_SCHEMA_VERSION: &str = "1";
const NETWORK_REQUIREMENT_KEY: &str = "external-network";
const CONSENT_FILESYSTEM_WRITE_KEY: &str = "filesystem.write";
const CONSENT_NETWORK_EGRESS_KEY: &str = "network.egress";
const CONSENT_SECRETS_ACCESS_KEY: &str = "secrets.access";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectRequirementsResult {
    pub schema_version: &'static str,
    pub target: InspectTarget,
    pub requirements: RequirementCategories,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectTarget {
    pub input: String,
    pub kind: &'static str,
    pub resolved: ResolvedTarget,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResolvedTarget {
    Local { path: String },
    Remote { publisher: String, slug: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ExecutionInspectView {
    Receipt {
        receipt: Box<ExecutionReceipt>,
        gaps: Vec<ExecutionTrackingGap>,
    },
    Comparison {
        comparison: ExecutionReceiptComparison,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionReceiptComparison {
    pub left_execution_id: String,
    pub right_execution_id: String,
    pub differences: Vec<ExecutionReceiptDiff>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ExecutionReceiptDiff {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExecutionTrackingGap {
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RequirementCategories {
    pub secrets: Vec<SecretRequirement>,
    pub state: Vec<StateRequirementItem>,
    pub env: Vec<EnvRequirement>,
    pub network: Vec<NetworkRequirement>,
    pub services: Vec<ServiceRequirement>,
    pub consent: Vec<ConsentRequirement>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateRequirementItem {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<StateKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability: Option<StateDurability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attach: Option<StateAttach>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<NetworkIdentity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkIdentity {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsentRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct InspectRequirementsError {
    code: &'static str,
    message: String,
    details: Value,
}

#[derive(Debug, Serialize)]
struct InspectRequirementsErrorEnvelope<'a> {
    error: InspectRequirementsErrorPayload<'a>,
}

#[derive(Debug, Serialize)]
struct InspectRequirementsErrorPayload<'a> {
    code: &'a str,
    message: &'a str,
    details: &'a Value,
}

struct ResolvedInspection {
    target: InspectTarget,
    manifest: CapsuleManifest,
}

#[derive(Debug, Default)]
struct EnvRequirementAccumulator {
    required: bool,
    required_targets: BTreeSet<String>,
    allowlisted: bool,
}

pub async fn execute_requirements(
    input: String,
    registry: Option<String>,
    json_output: bool,
) -> Result<InspectRequirementsResult, InspectRequirementsError> {
    let resolved = resolve_target(&input, registry.as_deref()).await?;
    let result = InspectRequirementsResult {
        schema_version: REQUIREMENTS_SCHEMA_VERSION,
        target: resolved.target,
        requirements: build_requirements(&resolved.manifest),
    };

    if json_output {
        let payload = serde_json::to_string_pretty(&result).map_err(|err| {
            InspectRequirementsError::requirements_resolution_failed(
                &input,
                format!("Failed to serialize requirements JSON: {err}"),
            )
        })?;
        println!("{payload}");
    } else {
        print_human_readable(&result);
    }

    Ok(result)
}

pub fn try_emit_json_error(err: &AnyhowError) -> bool {
    let Some(inspect_err) = err.downcast_ref::<InspectRequirementsError>() else {
        return false;
    };

    inspect_err.emit_json();
    true
}

const INSPECT_SCHEMA_VERSION: &str = "1";
const GLOBAL_RUN_SOURCE_INFERENCE_DIR: &str = "source-inference";
const WORKSPACE_SOURCE_INFERENCE_DIR: &str = ".ato/source-inference";
const WORKSPACE_BINDING_SEED_PATH: &str = ".ato/binding/seed.json";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectLockView {
    pub schema_version: &'static str,
    pub target: LockSurfaceTarget,
    pub summary: InspectLockSummary,
    pub fields: Vec<InspectFieldView>,
    pub unresolved: Vec<InspectUnresolvedView>,
    pub diagnostics: Vec<InspectDiagnosticView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewLockView {
    pub schema_version: &'static str,
    pub target: LockSurfaceTarget,
    pub preview: PreviewSurfaceSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsLockView {
    pub schema_version: &'static str,
    pub target: LockSurfaceTarget,
    pub diagnostics: Vec<InspectDiagnosticView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemediationLockView {
    pub schema_version: &'static str,
    pub target: LockSurfaceTarget,
    pub suggestions: Vec<RemediationSuggestionView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockSurfaceTarget {
    pub input: String,
    pub authoritative_kind: String,
    pub project_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authoritative_path: Option<String>,
    pub lock_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_cache_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_seed_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectLockSummary {
    pub input_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    pub total_fields: usize,
    pub resolved_fields: usize,
    pub unresolved_fields: usize,
    pub diagnostics_count: usize,
    pub fallback_fields: usize,
    pub observed_fields: usize,
    pub user_confirmed_fields: usize,
    pub selection_gate_involved: bool,
    pub approval_gate_involved: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectFieldView {
    pub lock_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    pub resolved: bool,
    pub explicit: bool,
    pub inferred: bool,
    pub observed: bool,
    pub user_confirmed: bool,
    pub fallback: bool,
    pub selection_gate_involved: bool,
    pub approval_gate_involved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_digestable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure_provenance_limited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<InspectProvenanceView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectProvenanceView {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub importer_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectUnresolvedView {
    pub lock_path: String,
    pub reason_class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectDiagnosticView {
    pub severity: String,
    pub lock_path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_mapping: Option<SourceMappingView>,
    pub inspect_command: String,
    pub preview_command: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemediationSuggestionView {
    pub lock_path: String,
    pub reason_class: String,
    pub message: String,
    pub recommended_action: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_mapping: Option<SourceMappingView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMappingView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSurfaceSummary {
    pub input_kind: String,
    pub durable_lock_state: String,
    pub durable_materialization: PreviewMaterializationView,
    pub run_attempt_materialization: PreviewMaterializationView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<InspectUnresolvedView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<InspectDiagnosticView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMaterializationView {
    pub kind: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<PreviewOutputPathView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewOutputPathView {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone)]
struct InspectionSnapshot {
    requested_input: String,
    authoritative_kind: String,
    project_root: PathBuf,
    authoritative_path: Option<PathBuf>,
    lock_path: PathBuf,
    provenance_path: Option<PathBuf>,
    provenance_cache_path: Option<PathBuf>,
    binding_seed_path: Option<PathBuf>,
    run_attempt_root: PathBuf,
    input_kind: String,
    lock: AtoLock,
    provenance: Vec<StoredProvenanceRecord>,
    diagnostics: Vec<StoredDiagnosticRecord>,
    infer_unresolved: Vec<String>,
    resolve_unresolved: Vec<String>,
    selection_gate: Option<StoredSelectionGate>,
    approval_gate: Option<StoredApprovalGate>,
    advisories: Vec<String>,
}

#[derive(Debug, Clone)]
struct StoredProvenanceRecord {
    field: String,
    kind: String,
    source_path: Option<PathBuf>,
    importer_id: Option<String>,
    evidence_kind: Option<String>,
    source_field: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Clone)]
struct StoredDiagnosticRecord {
    severity: String,
    field: String,
    message: String,
}

#[derive(Debug, Clone)]
struct StoredSelectionGate {
    field: String,
}

#[derive(Debug, Clone)]
struct StoredApprovalGate {
    capability: String,
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StoredSidecar {
    input_kind: String,
    #[serde(default)]
    provenance: Vec<StoredSidecarProvenance>,
    #[serde(default)]
    diagnostics: Vec<StoredSidecarDiagnostic>,
    selection_gate: Option<StoredSidecarSelectionGate>,
    approval_gate: Option<StoredSidecarApprovalGate>,
    #[serde(default)]
    infer: StoredSidecarResolve,
    #[serde(default)]
    resolve: StoredSidecarResolve,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct StoredSidecarResolve {
    #[serde(default)]
    unresolved: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StoredSidecarProvenance {
    field: String,
    kind: String,
    source_path: Option<PathBuf>,
    #[serde(default)]
    importer_id: Option<String>,
    #[serde(default)]
    evidence_kind: Option<String>,
    source_field: Option<String>,
    note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StoredSidecarDiagnostic {
    severity: String,
    field: String,
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StoredSidecarSelectionGate {
    field: String,
}

#[derive(Debug, Clone, Deserialize)]
struct StoredSidecarApprovalGate {
    capability: String,
    message: String,
}

pub fn execute_lock_view(path: PathBuf, json_output: bool) -> Result<InspectLockView> {
    let snapshot = load_inspection_snapshot(&path)?;
    let view = build_lock_view(&snapshot);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print_lock_view(&view);
    }
    Ok(view)
}

pub fn execute_preview_view(path: PathBuf, json_output: bool) -> Result<PreviewLockView> {
    let snapshot = load_inspection_snapshot(&path)?;
    let view = build_preview_view(&snapshot);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print_preview_view(&view);
    }
    Ok(view)
}

pub fn execute_diagnostics_view(path: PathBuf, json_output: bool) -> Result<DiagnosticsLockView> {
    let snapshot = load_inspection_snapshot(&path)?;
    let view = build_diagnostics_view(&snapshot);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print_diagnostics_view(&view);
    }
    Ok(view)
}

pub fn execute_remediation_view(path: PathBuf, json_output: bool) -> Result<RemediationLockView> {
    let snapshot = load_inspection_snapshot(&path)?;
    let view = build_remediation_view(&snapshot);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print_remediation_view(&view);
    }
    Ok(view)
}

pub fn execute_execution_view(
    execution_id: String,
    compare: Option<String>,
    json_output: bool,
) -> Result<ExecutionInspectView> {
    let left = execution_receipts::read_receipt(&execution_id)?;
    let view = if let Some(right_id) = compare {
        let right = execution_receipts::read_receipt(&right_id)?;
        ExecutionInspectView::Comparison {
            comparison: compare_execution_receipts(&left, &right)?,
        }
    } else {
        ExecutionInspectView::Receipt {
            gaps: collect_tracking_gaps(&left)?,
            receipt: Box::new(left),
        }
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        print_execution_view(&view);
    }
    Ok(view)
}

fn load_inspection_snapshot(path: &Path) -> Result<InspectionSnapshot> {
    let reporter = Arc::new(CliReporter::new(true));
    let resolved = resolve_authoritative_input(path, ResolveInputOptions::default())?;
    match resolved {
        ResolvedInput::CanonicalLock {
            canonical,
            provenance,
            advisories,
        } => {
            let mut result = source_inference::execute_shared_engine(
                SourceInferenceInput::CanonicalLock(CanonicalLockInput {
                    project_root: canonical.project_root.clone(),
                    canonical_path: canonical.path.clone(),
                    lock: canonical.lock.clone(),
                }),
                MaterializationMode::InitWorkspace,
                true,
                reporter,
            )?;
            let sidecar_paths = workspace_sidecar_paths(&canonical.project_root);
            if let Some(sidecar) = load_stored_sidecar(&sidecar_paths.provenance_path)? {
                apply_stored_sidecar(&mut result, sidecar);
            }

            Ok(snapshot_from_result(
                path.display().to_string(),
                provenance.selected_kind.as_str().to_string(),
                canonical.project_root.clone(),
                Some(canonical.path.clone()),
                canonical.path,
                Some(sidecar_paths.provenance_path),
                Some(sidecar_paths.cache_path),
                Some(sidecar_paths.binding_seed_path),
                ato_runs_dir().join(GLOBAL_RUN_SOURCE_INFERENCE_DIR),
                advisories.into_iter().map(|value| value.message).collect(),
                result,
            ))
        }
        ResolvedInput::CompatibilityProject {
            project,
            provenance,
            advisories,
        } => {
            let (draft_input, compiled) =
                source_inference::draft_lock_input_from_compatibility(&project)?;
            let mut result = source_inference::execute_shared_engine(
                SourceInferenceInput::DraftLock(draft_input),
                MaterializationMode::InitWorkspace,
                true,
                reporter,
            )?;
            append_compatibility_diagnostics(&mut result, &compiled);
            Ok(snapshot_from_result(
                path.display().to_string(),
                provenance.selected_kind.as_str().to_string(),
                project.project_root.clone(),
                Some(project.manifest.path.clone()),
                project.project_root.join(ATO_LOCK_FILE_NAME),
                Some(
                    project
                        .project_root
                        .join(WORKSPACE_SOURCE_INFERENCE_DIR)
                        .join("provenance.json"),
                ),
                Some(
                    project
                        .project_root
                        .join(WORKSPACE_SOURCE_INFERENCE_DIR)
                        .join("provenance-cache.json"),
                ),
                Some(project.project_root.join(WORKSPACE_BINDING_SEED_PATH)),
                ato_runs_dir().join(GLOBAL_RUN_SOURCE_INFERENCE_DIR),
                advisories.into_iter().map(|value| value.message).collect(),
                result,
            ))
        }
        ResolvedInput::SourceOnly {
            source,
            provenance,
            advisories,
        } => {
            let result = source_inference::execute_shared_engine(
                SourceInferenceInput::SourceEvidence(SourceEvidenceInput {
                    project_root: source.project_root.clone(),
                    explicit_native_artifact: None,
                    single_script_language: source
                        .single_script
                        .as_ref()
                        .map(|script| script.language),
                    authoritative_root: Some(source.project_root.clone()),
                }),
                MaterializationMode::InitWorkspace,
                true,
                reporter,
            )?;
            Ok(snapshot_from_result(
                path.display().to_string(),
                provenance.selected_kind.as_str().to_string(),
                source.project_root.clone(),
                None,
                source.project_root.join(ATO_LOCK_FILE_NAME),
                Some(
                    source
                        .project_root
                        .join(WORKSPACE_SOURCE_INFERENCE_DIR)
                        .join("provenance.json"),
                ),
                Some(
                    source
                        .project_root
                        .join(WORKSPACE_SOURCE_INFERENCE_DIR)
                        .join("provenance-cache.json"),
                ),
                Some(source.project_root.join(WORKSPACE_BINDING_SEED_PATH)),
                ato_runs_dir().join(GLOBAL_RUN_SOURCE_INFERENCE_DIR),
                advisories.into_iter().map(|value| value.message).collect(),
                result,
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn snapshot_from_result(
    requested_input: String,
    authoritative_kind: String,
    project_root: PathBuf,
    authoritative_path: Option<PathBuf>,
    lock_path: PathBuf,
    provenance_path: Option<PathBuf>,
    provenance_cache_path: Option<PathBuf>,
    binding_seed_path: Option<PathBuf>,
    run_attempt_root: PathBuf,
    advisories: Vec<String>,
    result: SourceInferenceResult,
) -> InspectionSnapshot {
    InspectionSnapshot {
        requested_input,
        authoritative_kind,
        project_root,
        authoritative_path,
        lock_path,
        provenance_path,
        provenance_cache_path,
        binding_seed_path,
        run_attempt_root,
        input_kind: input_kind_label(result.input_kind).to_string(),
        lock: result.lock,
        provenance: result
            .provenance
            .iter()
            .map(convert_provenance_record)
            .collect(),
        diagnostics: result
            .diagnostics
            .iter()
            .map(convert_diagnostic_record)
            .collect(),
        infer_unresolved: result.infer.unresolved,
        resolve_unresolved: result.resolve.unresolved,
        selection_gate: result
            .selection_gate
            .map(|gate| StoredSelectionGate { field: gate.field }),
        approval_gate: result.approval_gate.map(|gate| StoredApprovalGate {
            capability: gate.capability,
            message: gate.message,
        }),
        advisories,
    }
}

fn build_lock_view(snapshot: &InspectionSnapshot) -> InspectLockView {
    let unresolved = collect_unresolved_views(snapshot);
    let diagnostics = collect_diagnostic_views(snapshot);
    let fields = collect_field_views(snapshot, &unresolved);
    let summary = InspectLockSummary {
        input_kind: snapshot.input_kind.clone(),
        lock_id: snapshot
            .lock
            .lock_id
            .as_ref()
            .map(|value| value.as_str().to_string()),
        generated_at: snapshot.lock.generated_at.clone(),
        total_fields: fields.len(),
        resolved_fields: fields.iter().filter(|field| field.resolved).count(),
        unresolved_fields: unresolved.len(),
        diagnostics_count: diagnostics.len(),
        fallback_fields: fields.iter().filter(|field| field.fallback).count(),
        observed_fields: fields.iter().filter(|field| field.observed).count(),
        user_confirmed_fields: fields.iter().filter(|field| field.user_confirmed).count(),
        selection_gate_involved: fields.iter().any(|field| field.selection_gate_involved),
        approval_gate_involved: fields.iter().any(|field| field.approval_gate_involved),
    };

    InspectLockView {
        schema_version: INSPECT_SCHEMA_VERSION,
        target: snapshot_target(snapshot),
        summary,
        fields,
        unresolved,
        diagnostics,
        advisories: snapshot.advisories.clone(),
    }
}

fn build_preview_view(snapshot: &InspectionSnapshot) -> PreviewLockView {
    let diagnostics = collect_diagnostic_views(snapshot);
    let unresolved = collect_unresolved_views(snapshot);
    let durable_state = if snapshot.lock_path.exists() {
        "present"
    } else {
        "would_write"
    };

    PreviewLockView {
        schema_version: INSPECT_SCHEMA_VERSION,
        target: snapshot_target(snapshot),
        preview: PreviewSurfaceSummary {
            input_kind: snapshot.input_kind.clone(),
            durable_lock_state: durable_state.to_string(),
            durable_materialization: PreviewMaterializationView {
                kind: "init_workspace".to_string(),
                state: durable_state.to_string(),
                outputs: vec![
                    PreviewOutputPathView {
                        label: "lock".to_string(),
                        path: snapshot.lock_path.display().to_string(),
                    },
                    PreviewOutputPathView {
                        label: "provenance".to_string(),
                        path: snapshot
                            .provenance_path
                            .as_ref()
                            .map(|value| value.display().to_string())
                            .unwrap_or_else(|| {
                                snapshot
                                    .project_root
                                    .join(WORKSPACE_SOURCE_INFERENCE_DIR)
                                    .join("provenance.json")
                                    .display()
                                    .to_string()
                            }),
                    },
                    PreviewOutputPathView {
                        label: "provenance_cache".to_string(),
                        path: snapshot
                            .provenance_cache_path
                            .as_ref()
                            .map(|value| value.display().to_string())
                            .unwrap_or_else(|| {
                                snapshot
                                    .project_root
                                    .join(WORKSPACE_SOURCE_INFERENCE_DIR)
                                    .join("provenance-cache.json")
                                    .display()
                                    .to_string()
                            }),
                    },
                    PreviewOutputPathView {
                        label: "binding_seed".to_string(),
                        path: snapshot
                            .binding_seed_path
                            .as_ref()
                            .map(|value| value.display().to_string())
                            .unwrap_or_else(|| {
                                snapshot
                                    .project_root
                                    .join(WORKSPACE_BINDING_SEED_PATH)
                                    .display()
                                    .to_string()
                            }),
                    },
                ],
            },
            run_attempt_materialization: PreviewMaterializationView {
                kind: "run_attempt".to_string(),
                state: "ephemeral".to_string(),
                outputs: vec![
                    PreviewOutputPathView {
                        label: "attempt_lock".to_string(),
                        path: snapshot
                            .run_attempt_root
                            .join("<attempt>")
                            .join(ATO_LOCK_FILE_NAME)
                            .display()
                            .to_string(),
                    },
                    PreviewOutputPathView {
                        label: "attempt_provenance".to_string(),
                        path: snapshot
                            .run_attempt_root
                            .join("<attempt>")
                            .join("provenance.json")
                            .display()
                            .to_string(),
                    },
                ],
            },
            unresolved,
            diagnostics,
        },
        advisories: snapshot.advisories.clone(),
    }
}

fn build_diagnostics_view(snapshot: &InspectionSnapshot) -> DiagnosticsLockView {
    DiagnosticsLockView {
        schema_version: INSPECT_SCHEMA_VERSION,
        target: snapshot_target(snapshot),
        diagnostics: collect_diagnostic_views(snapshot),
        advisories: snapshot.advisories.clone(),
    }
}

fn build_remediation_view(snapshot: &InspectionSnapshot) -> RemediationLockView {
    let diagnostics = collect_diagnostic_views(snapshot);
    let unresolved = collect_unresolved_views(snapshot);
    let mut suggestions = Vec::new();

    for unresolved_item in &unresolved {
        let mapping = source_mapping_for_field(snapshot, &unresolved_item.lock_path);
        suggestions.push(RemediationSuggestionView {
            lock_path: unresolved_item.lock_path.clone(),
            reason_class: unresolved_item.reason_class.clone(),
            message: unresolved_item
                .detail
                .clone()
                .unwrap_or_else(|| unresolved_message(unresolved_item)),
            recommended_action: remediation_action(
                &unresolved_item.lock_path,
                &unresolved_item.reason_class,
            ),
            commands: remediation_commands(&snapshot.requested_input),
            source_mapping: mapping,
        });
    }

    for diagnostic in diagnostics {
        if suggestions
            .iter()
            .any(|value| value.lock_path == diagnostic.lock_path)
        {
            continue;
        }
        suggestions.push(RemediationSuggestionView {
            lock_path: diagnostic.lock_path.clone(),
            reason_class: diagnostic
                .reason_class
                .clone()
                .unwrap_or_else(|| diagnostic.severity.clone()),
            message: diagnostic.message.clone(),
            recommended_action: remediation_action(
                &diagnostic.lock_path,
                diagnostic
                    .reason_class
                    .as_deref()
                    .unwrap_or(diagnostic.severity.as_str()),
            ),
            commands: remediation_commands(&snapshot.requested_input),
            source_mapping: diagnostic.source_mapping.clone(),
        });
    }

    suggestions.sort_by(|left, right| {
        left.lock_path
            .cmp(&right.lock_path)
            .then(left.reason_class.cmp(&right.reason_class))
    });

    RemediationLockView {
        schema_version: INSPECT_SCHEMA_VERSION,
        target: snapshot_target(snapshot),
        suggestions,
        advisories: snapshot.advisories.clone(),
    }
}

fn snapshot_target(snapshot: &InspectionSnapshot) -> LockSurfaceTarget {
    LockSurfaceTarget {
        input: snapshot.requested_input.clone(),
        authoritative_kind: snapshot.authoritative_kind.clone(),
        project_root: snapshot.project_root.display().to_string(),
        authoritative_path: snapshot
            .authoritative_path
            .as_ref()
            .map(|value| value.display().to_string()),
        lock_path: snapshot.lock_path.display().to_string(),
        provenance_path: snapshot
            .provenance_path
            .as_ref()
            .map(|value| value.display().to_string()),
        provenance_cache_path: snapshot
            .provenance_cache_path
            .as_ref()
            .map(|value| value.display().to_string()),
        binding_seed_path: snapshot
            .binding_seed_path
            .as_ref()
            .map(|value| value.display().to_string()),
    }
}

fn collect_field_views(
    snapshot: &InspectionSnapshot,
    unresolved: &[InspectUnresolvedView],
) -> Vec<InspectFieldView> {
    let mut entries = lock_field_entries(&snapshot.lock);
    let unresolved_fields = unresolved
        .iter()
        .map(|value| value.lock_path.clone())
        .collect::<BTreeSet<_>>();
    for field in unresolved_fields {
        entries.entry(field).or_insert(None);
    }

    let mut fields = entries
        .into_iter()
        .map(|(lock_path, value)| {
            let mut provenance = provenance_for_field(snapshot, &lock_path);
            if provenance.is_empty() {
                provenance = default_provenance(snapshot, &lock_path);
            }
            let selection_gate_involved = snapshot
                .selection_gate
                .as_ref()
                .map(|value| value.field == lock_path)
                .unwrap_or(false)
                || provenance
                    .iter()
                    .any(|value| value.kind == "selection_gate");
            let approval_gate_involved =
                provenance.iter().any(|value| value.kind == "approval_gate")
                    || snapshot
                        .approval_gate
                        .as_ref()
                        .map(|gate| !gate.capability.is_empty() || !gate.message.is_empty())
                        .unwrap_or(false);
            let closure_surface = closure_surface_for_field(&lock_path, value.as_ref());
            let delivery_mode = delivery_mode_for_field(&lock_path, value.as_ref());
            InspectFieldView {
                resolved: value.is_some(),
                explicit: provenance.iter().any(provenance_is_explicit),
                inferred: provenance.iter().any(provenance_is_inferred),
                observed: provenance.iter().any(provenance_is_observed),
                user_confirmed: provenance.iter().any(provenance_is_user_confirmed),
                fallback: provenance.iter().any(provenance_uses_fallback),
                selection_gate_involved,
                approval_gate_involved,
                closure_kind: closure_surface.as_ref().map(|value| value.kind.clone()),
                closure_status: closure_surface.as_ref().map(|value| value.status.clone()),
                closure_digestable: closure_surface.as_ref().map(|value| value.digestable),
                closure_provenance_limited: closure_surface
                    .as_ref()
                    .map(|value| value.provenance_limited),
                delivery_mode,
                lock_path,
                value,
                provenance: provenance
                    .into_iter()
                    .map(|record| InspectProvenanceView {
                        kind: record.kind,
                        source_path: record.source_path.map(|value| value.display().to_string()),
                        importer_id: record.importer_id,
                        evidence_kind: record.evidence_kind,
                        source_field: record.source_field,
                        note: record.note,
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();

    fields.sort_by(|left, right| left.lock_path.cmp(&right.lock_path));
    fields
}

fn collect_unresolved_views(snapshot: &InspectionSnapshot) -> Vec<InspectUnresolvedView> {
    let unresolved_paths = if snapshot.resolve_unresolved.is_empty() {
        snapshot.infer_unresolved.clone()
    } else {
        snapshot.resolve_unresolved.clone()
    };

    let mut contract_markers = snapshot.lock.contract.unresolved.iter();
    let mut resolution_markers = snapshot.lock.resolution.unresolved.iter();
    let mut binding_markers = snapshot.lock.binding.unresolved.iter();
    let mut policy_markers = snapshot.lock.policy.unresolved.iter();
    let mut attestation_markers = snapshot.lock.attestations.unresolved.iter();
    let mut records = Vec::new();

    for path in unresolved_paths {
        let marker = match path.split('.').next().unwrap_or_default() {
            "contract" => contract_markers.next(),
            "resolution" => resolution_markers.next(),
            "binding" => binding_markers.next(),
            "policy" => policy_markers.next(),
            "attestations" => attestation_markers.next(),
            _ => None,
        };
        records.push(build_unresolved_view(&path, marker));
    }

    for marker in contract_markers {
        records.push(build_unresolved_view("contract", Some(marker)));
    }
    for marker in resolution_markers {
        records.push(build_unresolved_view("resolution", Some(marker)));
    }
    for marker in binding_markers {
        records.push(build_unresolved_view("binding", Some(marker)));
    }
    for marker in policy_markers {
        records.push(build_unresolved_view("policy", Some(marker)));
    }
    for marker in attestation_markers {
        records.push(build_unresolved_view("attestations", Some(marker)));
    }

    records.sort_by(|left, right| {
        left.lock_path
            .cmp(&right.lock_path)
            .then(left.reason_class.cmp(&right.reason_class))
    });
    records.dedup_by(|left, right| {
        left.lock_path == right.lock_path
            && left.reason_class == right.reason_class
            && left.detail == right.detail
    });
    records
}

fn collect_diagnostic_views(snapshot: &InspectionSnapshot) -> Vec<InspectDiagnosticView> {
    let inspect_command = format!("ato inspect lock {}", snapshot.requested_input);
    let preview_command = format!("ato inspect preview {}", snapshot.requested_input);
    let mut diagnostics = snapshot
        .diagnostics
        .iter()
        .map(|record| InspectDiagnosticView {
            severity: record.severity.clone(),
            lock_path: record.field.clone(),
            message: record.message.clone(),
            reason_class: None,
            source_mapping: source_mapping_for_field(snapshot, &record.field),
            inspect_command: inspect_command.clone(),
            preview_command: preview_command.clone(),
        })
        .collect::<Vec<_>>();

    for unresolved in collect_unresolved_views(snapshot) {
        if diagnostics
            .iter()
            .any(|value| value.lock_path == unresolved.lock_path)
        {
            continue;
        }
        diagnostics.push(InspectDiagnosticView {
            severity: diagnostic_severity_for_reason(&unresolved.reason_class).to_string(),
            lock_path: unresolved.lock_path.clone(),
            message: unresolved_message(&unresolved),
            reason_class: Some(unresolved.reason_class.clone()),
            source_mapping: source_mapping_for_field(snapshot, &unresolved.lock_path),
            inspect_command: inspect_command.clone(),
            preview_command: preview_command.clone(),
        });
    }

    if let Some(closure) = snapshot.lock.resolution.entries.get("closure") {
        if let Ok(info) = closure_info(closure) {
            if info.status == "incomplete"
                && !diagnostics
                    .iter()
                    .any(|value| value.lock_path == "resolution.closure")
            {
                diagnostics.push(InspectDiagnosticView {
                    severity: "warning".to_string(),
                    lock_path: "resolution.closure".to_string(),
                    message: format!("resolution.closure remains incomplete ({})", info.kind),
                    reason_class: Some("incomplete_closure".to_string()),
                    source_mapping: source_mapping_for_field(snapshot, "resolution.closure"),
                    inspect_command: inspect_command.clone(),
                    preview_command: preview_command.clone(),
                });
            }
        }
    }

    diagnostics.sort_by(|left, right| {
        left.lock_path
            .cmp(&right.lock_path)
            .then(left.message.cmp(&right.message))
    });
    diagnostics
}

fn build_unresolved_view(
    lock_path: &str,
    marker: Option<&UnresolvedValue>,
) -> InspectUnresolvedView {
    InspectUnresolvedView {
        lock_path: lock_path.to_string(),
        reason_class: marker
            .map(|value| value.reason.as_str().into_owned())
            .unwrap_or_else(|| "unresolved".to_string()),
        detail: marker.and_then(|value| value.detail.clone()),
        candidates: marker
            .map(|value| value.candidates.clone())
            .unwrap_or_default(),
    }
}

fn unresolved_message(unresolved: &InspectUnresolvedView) -> String {
    unresolved.detail.clone().unwrap_or_else(|| {
        format!(
            "{} remains unresolved ({})",
            unresolved.lock_path, unresolved.reason_class
        )
    })
}

fn remediation_action(lock_path: &str, reason_class: &str) -> String {
    match (lock_path, reason_class) {
        ("contract.process", "explicit_selection_required") => {
            "select a concrete process and persist it through ato init or an explicit compatibility source field".to_string()
        }
        ("contract.process", _) => {
            "declare a runnable process entrypoint so contract.process can be resolved".to_string()
        }
        ("resolution.runtime", _) => {
            "declare runtime-target metadata explicitly and regenerate the lock-first baseline".to_string()
        }
        ("resolution.closure", _) => {
            "materialize dependency closure state such as package lockfiles before regenerating ato.lock.json".to_string()
        }
        _ if lock_path == "binding" || lock_path.starts_with("binding.") => {
            "populate workspace-local binding seed entries or accept the host-local binding prompt".to_string()
        }
        _ if lock_path == "policy" || lock_path.starts_with("policy.") => {
            "update the workspace-local policy bundle or embedded lock policy; policy gates execution but does not change lock identity".to_string()
        }
        _ if lock_path == "attestations" || lock_path.starts_with("attestations.") => {
            "record or refresh workspace-local attestation/observation evidence; attestation state is not part of canonical lock content".to_string()
        }
        _ => "update the source field mapped by provenance, then rerun ato inspect preview to verify the lock path".to_string(),
    }
}

fn remediation_commands(input: &str) -> Vec<String> {
    vec![
        format!("ato inspect lock {}", input),
        format!("ato inspect preview {}", input),
    ]
}

fn source_mapping_for_field(
    snapshot: &InspectionSnapshot,
    field: &str,
) -> Option<SourceMappingView> {
    provenance_for_field(snapshot, field)
        .into_iter()
        .find(|record| {
            record.source_path.is_some() || record.source_field.is_some() || record.note.is_some()
        })
        .map(|record| SourceMappingView {
            source_path: record.source_path.map(|value| value.display().to_string()),
            source_field: record.source_field,
            note: record.note,
        })
}

fn provenance_for_field(snapshot: &InspectionSnapshot, field: &str) -> Vec<StoredProvenanceRecord> {
    snapshot
        .provenance
        .iter()
        .filter(|record| record.field == field || record.field == "root")
        .cloned()
        .collect()
}

fn default_provenance(snapshot: &InspectionSnapshot, field: &str) -> Vec<StoredProvenanceRecord> {
    snapshot
        .authoritative_path
        .as_ref()
        .map(|path| StoredProvenanceRecord {
            field: field.to_string(),
            kind: if snapshot.authoritative_kind == "canonical_lock" {
                "canonical_input".to_string()
            } else {
                "explicit_artifact".to_string()
            },
            source_path: Some(path.clone()),
            importer_id: None,
            evidence_kind: None,
            source_field: Some(field.to_string()),
            note: Some("no persisted provenance sidecar was present, so authoritative input ownership was used".to_string()),
        })
        .into_iter()
        .collect()
}

fn lock_field_entries(lock: &AtoLock) -> BTreeMap<String, Option<Value>> {
    let mut fields = BTreeMap::new();
    if !lock.features.declared.is_empty() {
        fields.insert(
            "features.declared".to_string(),
            Some(json!(lock
                .features
                .declared
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>())),
        );
    }
    if !lock.features.required_for_execution.is_empty() {
        fields.insert(
            "features.required_for_execution".to_string(),
            Some(json!(lock
                .features
                .required_for_execution
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>())),
        );
    }
    if !lock.features.implementation_phase.is_empty() {
        fields.insert(
            "features.implementation_phase".to_string(),
            Some(json!(lock.features.implementation_phase)),
        );
    }
    append_section_fields(&mut fields, "resolution", &lock.resolution.entries);
    append_section_fields(&mut fields, "contract", &lock.contract.entries);
    append_section_fields(&mut fields, "binding", &lock.binding.entries);
    append_section_fields(&mut fields, "policy", &lock.policy.entries);
    append_section_fields(&mut fields, "attestations", &lock.attestations.entries);
    fields
}

fn append_section_fields(
    fields: &mut BTreeMap<String, Option<Value>>,
    section: &str,
    entries: &BTreeMap<String, Value>,
) {
    for (key, value) in entries {
        fields.insert(format!("{}.{}", section, key), Some(value.clone()));
    }
}

struct ClosureSurfaceView {
    kind: String,
    status: String,
    digestable: bool,
    provenance_limited: bool,
}

fn closure_surface_for_field(lock_path: &str, value: Option<&Value>) -> Option<ClosureSurfaceView> {
    if lock_path != "resolution.closure" {
        return None;
    }

    let info = closure_info(value?).ok()?;
    Some(ClosureSurfaceView {
        kind: info.kind,
        status: info.status,
        digestable: info.digestable,
        provenance_limited: info.provenance_limited,
    })
}

fn delivery_mode_for_field(lock_path: &str, value: Option<&Value>) -> Option<String> {
    if lock_path != "contract.delivery" {
        return None;
    }

    value?
        .get("mode")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn append_compatibility_diagnostics(
    result: &mut SourceInferenceResult,
    compiled: &CompatibilityCompileResult,
) {
    result
        .diagnostics
        .extend(
            compiled
                .diagnostics
                .iter()
                .map(|diagnostic| SourceInferenceDiagnostic {
                    severity: match diagnostic.severity {
                        CompatibilityDiagnosticSeverity::Warning => {
                            SourceInferenceDiagnosticSeverity::Warning
                        }
                        CompatibilityDiagnosticSeverity::Error => {
                            SourceInferenceDiagnosticSeverity::Error
                        }
                    },
                    field: diagnostic.lock_path.clone(),
                    message: diagnostic.message.clone(),
                }),
        );
}

fn load_stored_sidecar(path: &Path) -> Result<Option<StoredSidecar>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let sidecar = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(sidecar))
}

fn apply_stored_sidecar(result: &mut SourceInferenceResult, sidecar: StoredSidecar) {
    result.provenance = sidecar
        .provenance
        .into_iter()
        .map(|record| SourceInferenceProvenance {
            field: record.field,
            kind: provenance_kind_from_str(&record.kind),
            source_path: record.source_path,
            importer_id: record.importer_id,
            evidence_kind: record.evidence_kind,
            source_field: record.source_field,
            note: record.note,
        })
        .collect();
    result.diagnostics = sidecar
        .diagnostics
        .into_iter()
        .map(|record| SourceInferenceDiagnostic {
            severity: if record.severity == "error" {
                SourceInferenceDiagnosticSeverity::Error
            } else {
                SourceInferenceDiagnosticSeverity::Warning
            },
            field: record.field,
            message: record.message,
        })
        .collect();
    result.infer.unresolved = sidecar.infer.unresolved;
    result.resolve.unresolved = sidecar.resolve.unresolved;
    result.selection_gate = sidecar
        .selection_gate
        .map(|gate| source_inference::SelectionGate {
            field: gate.field,
            candidates: Vec::new(),
            message: "persisted selection gate".to_string(),
        });
    result.approval_gate = sidecar
        .approval_gate
        .map(|gate| source_inference::ApprovalGate {
            capability: gate.capability,
            message: gate.message,
        });
    result.input_kind = input_kind_from_str(&sidecar.input_kind);
}

fn convert_provenance_record(record: &SourceInferenceProvenance) -> StoredProvenanceRecord {
    StoredProvenanceRecord {
        field: record.field.clone(),
        kind: provenance_kind_label(record.kind).to_string(),
        source_path: record.source_path.clone(),
        importer_id: record.importer_id.clone(),
        evidence_kind: record.evidence_kind.clone(),
        source_field: record.source_field.clone(),
        note: record.note.clone(),
    }
}

fn convert_diagnostic_record(record: &SourceInferenceDiagnostic) -> StoredDiagnosticRecord {
    StoredDiagnosticRecord {
        severity: diagnostic_severity_label(record.severity).to_string(),
        field: record.field.clone(),
        message: record.message.clone(),
    }
}

fn workspace_sidecar_paths(project_root: &Path) -> WorkspaceSidecarPaths {
    WorkspaceSidecarPaths {
        provenance_path: project_root
            .join(WORKSPACE_SOURCE_INFERENCE_DIR)
            .join("provenance.json"),
        cache_path: project_root
            .join(WORKSPACE_SOURCE_INFERENCE_DIR)
            .join("provenance-cache.json"),
        binding_seed_path: project_root.join(WORKSPACE_BINDING_SEED_PATH),
    }
}

struct WorkspaceSidecarPaths {
    provenance_path: PathBuf,
    cache_path: PathBuf,
    binding_seed_path: PathBuf,
}

fn input_kind_label(kind: SourceInferenceInputKind) -> &'static str {
    match kind {
        SourceInferenceInputKind::SourceEvidence => "source_evidence",
        SourceInferenceInputKind::DraftLock => "draft_lock",
        SourceInferenceInputKind::CanonicalLock => "canonical_lock",
    }
}

fn input_kind_from_str(value: &str) -> SourceInferenceInputKind {
    match value {
        "source_evidence" => SourceInferenceInputKind::SourceEvidence,
        "draft_lock" => SourceInferenceInputKind::DraftLock,
        _ => SourceInferenceInputKind::CanonicalLock,
    }
}

fn provenance_kind_label(kind: SourceInferenceProvenanceKind) -> &'static str {
    match kind {
        SourceInferenceProvenanceKind::ExplicitArtifact => "explicit_artifact",
        SourceInferenceProvenanceKind::CompatibilityImport => "compatibility_import",
        SourceInferenceProvenanceKind::CanonicalInput => "canonical_input",
        SourceInferenceProvenanceKind::DeterministicHeuristic => "deterministic_heuristic",
        SourceInferenceProvenanceKind::ImporterObservation => "importer_observation",
        SourceInferenceProvenanceKind::MetadataObservation => "metadata_observation",
        SourceInferenceProvenanceKind::SelectionGate => "selection_gate",
        SourceInferenceProvenanceKind::ApprovalGate => "approval_gate",
    }
}

fn provenance_kind_from_str(value: &str) -> SourceInferenceProvenanceKind {
    match value {
        "explicit_artifact" => SourceInferenceProvenanceKind::ExplicitArtifact,
        "compatibility_import" => SourceInferenceProvenanceKind::CompatibilityImport,
        "canonical_input" => SourceInferenceProvenanceKind::CanonicalInput,
        "importer_observation" => SourceInferenceProvenanceKind::ImporterObservation,
        "metadata_observation" => SourceInferenceProvenanceKind::MetadataObservation,
        "selection_gate" => SourceInferenceProvenanceKind::SelectionGate,
        "approval_gate" => SourceInferenceProvenanceKind::ApprovalGate,
        _ => SourceInferenceProvenanceKind::DeterministicHeuristic,
    }
}

fn diagnostic_severity_label(severity: SourceInferenceDiagnosticSeverity) -> &'static str {
    match severity {
        SourceInferenceDiagnosticSeverity::Warning => "warning",
        SourceInferenceDiagnosticSeverity::Error => "error",
    }
}

fn diagnostic_severity_for_reason(reason: &str) -> &'static str {
    match reason {
        "ambiguity" | "explicit_selection_required" => "warning",
        _ => "error",
    }
}

fn provenance_is_explicit(record: &StoredProvenanceRecord) -> bool {
    matches!(
        record.kind.as_str(),
        "explicit_artifact" | "canonical_input"
    )
}

fn provenance_is_inferred(record: &StoredProvenanceRecord) -> bool {
    matches!(
        record.kind.as_str(),
        "compatibility_import" | "deterministic_heuristic"
    )
}

fn provenance_is_observed(record: &StoredProvenanceRecord) -> bool {
    record.kind == "metadata_observation"
        || record.kind == "importer_observation"
        || record
            .note
            .as_deref()
            .map(|value| value.contains("observed"))
            .unwrap_or(false)
}

fn provenance_is_user_confirmed(record: &StoredProvenanceRecord) -> bool {
    record.kind == "selection_gate"
}

fn provenance_uses_fallback(record: &StoredProvenanceRecord) -> bool {
    matches!(record.kind.as_str(), "compatibility_import")
        || record
            .note
            .as_deref()
            .map(|value| {
                value.contains("fallback")
                    || value.contains("metadata-only")
                    || value.contains("promoted")
            })
            .unwrap_or(false)
}

fn print_lock_view(view: &InspectLockView) {
    println!("Lock target: {}", view.target.input);
    println!("  Authoritative input: {}", view.target.authoritative_kind);
    println!("  Lock path: {}", view.target.lock_path);
    if let Some(path) = view.target.provenance_path.as_ref() {
        println!("  Provenance: {}", path);
    }
    println!(
        "  Fields: {} total, {} unresolved, {} diagnostics",
        view.summary.total_fields, view.summary.unresolved_fields, view.summary.diagnostics_count
    );
    println!("Fields:");
    for field in &view.fields {
        let mut labels = Vec::new();
        if field.explicit {
            labels.push("explicit");
        }
        if field.inferred {
            labels.push("inferred");
        }
        if field.observed {
            labels.push("observed");
        }
        if field.user_confirmed {
            labels.push("user_confirmed");
        }
        if field.fallback {
            labels.push("fallback");
        }
        let status = if field.resolved {
            "resolved"
        } else {
            "unresolved"
        };
        if labels.is_empty() {
            print!("  - {} [{}]", field.lock_path, status);
        } else {
            print!(
                "  - {} [{}; {}]",
                field.lock_path,
                status,
                labels.join(", ")
            );
        }
        if let Some(kind) = field.closure_kind.as_ref() {
            print!(
                " kind={}, status={}, digestable={}, provenance_limited={}",
                kind,
                field.closure_status.as_deref().unwrap_or("unknown"),
                field.closure_digestable.unwrap_or(false),
                field.closure_provenance_limited.unwrap_or(false)
            );
        }
        if let Some(mode) = field.delivery_mode.as_deref() {
            print!(" mode={}", mode);
        }
        println!();
    }
    if !view.unresolved.is_empty() {
        println!("Unresolved:");
        for unresolved in &view.unresolved {
            println!(
                "  - {} ({}){}",
                unresolved.lock_path,
                unresolved.reason_class,
                unresolved
                    .detail
                    .as_deref()
                    .map(|value| format!(": {}", value))
                    .unwrap_or_default()
            );
        }
    }
}

fn print_execution_view(view: &ExecutionInspectView) {
    match view {
        ExecutionInspectView::Receipt { receipt, gaps } => {
            print_execution_receipt(receipt, gaps);
        }
        ExecutionInspectView::Comparison { comparison } => {
            println!(
                "Execution comparison: {} -> {}",
                comparison.left_execution_id, comparison.right_execution_id
            );
            if comparison.differences.is_empty() {
                println!("  No differences");
                return;
            }
            println!("Differences:");
            for diff in &comparison.differences {
                println!(
                    "  - {}: {} -> {}",
                    diff.path,
                    summarize_json_value(diff.left.as_ref()),
                    summarize_json_value(diff.right.as_ref())
                );
            }
        }
    }
}

fn print_execution_receipt(receipt: &ExecutionReceipt, gaps: &[ExecutionTrackingGap]) {
    println!("Execution: {}", receipt.execution_id);
    println!("  Computed at: {}", receipt.computed_at);
    println!(
        "  Identity: {} {}",
        receipt.identity.hash_algorithm, receipt.identity.input_hash
    );
    println!(
        "  Class: {}",
        reproducibility_class_label(receipt.reproducibility.class)
    );
    if !receipt.reproducibility.causes.is_empty() {
        println!("Causes:");
        for cause in &receipt.reproducibility.causes {
            println!("  - {}", reproducibility_cause_label(*cause));
        }
    }

    println!("Source:");
    print_tracked_string("  source_ref", &receipt.source.source_ref);
    print_tracked_string("  source_tree_hash", &receipt.source.source_tree_hash);

    println!("Dependencies:");
    print_tracked_string("  derivation_hash", &receipt.dependencies.derivation_hash);
    print_tracked_string("  output_hash", &receipt.dependencies.output_hash);

    println!("Runtime:");
    if let Some(declared) = receipt.runtime.declared.as_deref() {
        println!("  declared: {declared}");
    }
    if let Some(resolved) = receipt.runtime.resolved.as_deref() {
        println!("  resolved: {resolved}");
    }
    print_tracked_string("  binary_hash", &receipt.runtime.binary_hash);
    print_tracked_string("  dynamic_linkage", &receipt.runtime.dynamic_linkage);
    println!(
        "  platform: {}/{}/{}",
        receipt.runtime.platform.os, receipt.runtime.platform.arch, receipt.runtime.platform.libc
    );

    println!("Environment:");
    print_tracked_string("  closure_hash", &receipt.environment.closure_hash);
    println!("  mode: {:?}", receipt.environment.mode);
    if !receipt.environment.tracked_keys.is_empty() {
        println!(
            "  tracked_keys: {}",
            receipt.environment.tracked_keys.join(", ")
        );
    }
    if !receipt.environment.redacted_keys.is_empty() {
        println!(
            "  redacted_keys: {}",
            receipt.environment.redacted_keys.join(", ")
        );
    }

    println!("Filesystem:");
    print_tracked_string("  view_hash", &receipt.filesystem.view_hash);
    println!(
        "  projection_strategy: {}",
        receipt.filesystem.projection_strategy
    );
    if !receipt.filesystem.writable_dirs.is_empty() {
        println!(
            "  writable_dirs: {}",
            receipt.filesystem.writable_dirs.join(", ")
        );
    }
    if !receipt.filesystem.persistent_state.is_empty() {
        println!(
            "  persistent_state: {}",
            receipt.filesystem.persistent_state.join(", ")
        );
    }

    println!("Policy:");
    print_tracked_string("  network_policy_hash", &receipt.policy.network_policy_hash);
    print_tracked_string(
        "  capability_policy_hash",
        &receipt.policy.capability_policy_hash,
    );

    println!("Launch:");
    println!("  entry_point: {}", receipt.launch.entry_point);
    println!("  argv: {}", receipt.launch.argv.join(" "));
    println!("  working_directory: {}", receipt.launch.working_directory);

    if !gaps.is_empty() {
        println!("Unknown/untracked:");
        for gap in gaps {
            match gap.reason.as_deref() {
                Some(reason) => println!("  - {}: {} ({})", gap.path, gap.status, reason),
                None => println!("  - {}: {}", gap.path, gap.status),
            }
        }
    }
}

fn print_tracked_string(label: &str, tracked: &Tracked<String>) {
    match tracked.status {
        TrackingStatus::Known => {
            println!(
                "{label}: {}",
                tracked.value.as_deref().unwrap_or("<missing-known-value>")
            );
        }
        TrackingStatus::Unknown | TrackingStatus::Untracked | TrackingStatus::NotApplicable => {
            let status = tracking_status_label(tracked.status);
            match tracked.reason.as_deref() {
                Some(reason) => println!("{label}: {status} ({reason})"),
                None => println!("{label}: {status}"),
            }
        }
    }
}

fn compare_execution_receipts(
    left: &ExecutionReceipt,
    right: &ExecutionReceipt,
) -> Result<ExecutionReceiptComparison> {
    let left_value = serde_json::to_value(left)?;
    let right_value = serde_json::to_value(right)?;
    let mut differences = Vec::new();
    diff_json_values("", &left_value, &right_value, &mut differences);
    Ok(ExecutionReceiptComparison {
        left_execution_id: left.execution_id.clone(),
        right_execution_id: right.execution_id.clone(),
        differences,
    })
}

fn diff_json_values(
    path: &str,
    left: &Value,
    right: &Value,
    differences: &mut Vec<ExecutionReceiptDiff>,
) {
    match (left, right) {
        (Value::Object(left_map), Value::Object(right_map)) => {
            let keys = left_map
                .keys()
                .chain(right_map.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            for key in keys {
                let child_path = join_json_path(path, &key);
                match (left_map.get(&key), right_map.get(&key)) {
                    (Some(left_child), Some(right_child)) => {
                        diff_json_values(&child_path, left_child, right_child, differences);
                    }
                    (left_child, right_child) => differences.push(ExecutionReceiptDiff {
                        path: child_path,
                        left: left_child.cloned(),
                        right: right_child.cloned(),
                    }),
                }
            }
        }
        _ if left == right => {}
        _ => differences.push(ExecutionReceiptDiff {
            path: if path.is_empty() {
                "$".to_string()
            } else {
                path.to_string()
            },
            left: Some(left.clone()),
            right: Some(right.clone()),
        }),
    }
}

fn collect_tracking_gaps(receipt: &ExecutionReceipt) -> Result<Vec<ExecutionTrackingGap>> {
    let value = serde_json::to_value(receipt)?;
    let mut gaps = Vec::new();
    collect_tracking_gaps_from_value("", &value, &mut gaps);
    Ok(gaps)
}

fn collect_tracking_gaps_from_value(
    path: &str,
    value: &Value,
    gaps: &mut Vec<ExecutionTrackingGap>,
) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(status)) = map.get("status") {
                if status == "unknown" || status == "untracked" {
                    gaps.push(ExecutionTrackingGap {
                        path: if path.is_empty() {
                            "$".to_string()
                        } else {
                            path.to_string()
                        },
                        status: status.clone(),
                        reason: map
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    });
                }
            }
            for (key, child) in map {
                collect_tracking_gaps_from_value(&join_json_path(path, key), child, gaps);
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_tracking_gaps_from_value(&format!("{path}[{index}]"), child, gaps);
            }
        }
        _ => {}
    }
}

fn join_json_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}.{child}")
    }
}

fn summarize_json_value(value: Option<&Value>) -> String {
    match value {
        Some(value) => {
            let raw = serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string());
            if raw.len() > 96 {
                format!("{}...", &raw[..96])
            } else {
                raw
            }
        }
        None => "<missing>".to_string(),
    }
}

fn tracking_status_label(status: TrackingStatus) -> &'static str {
    match status {
        TrackingStatus::Known => "known",
        TrackingStatus::Unknown => "unknown",
        TrackingStatus::Untracked => "untracked",
        TrackingStatus::NotApplicable => "not-applicable",
    }
}

fn reproducibility_class_label(class: ReproducibilityClass) -> &'static str {
    match class {
        ReproducibilityClass::Pure => "pure",
        ReproducibilityClass::HostBound => "host-bound",
        ReproducibilityClass::StateBound => "state-bound",
        ReproducibilityClass::TimeBound => "time-bound",
        ReproducibilityClass::NetworkBound => "network-bound",
        ReproducibilityClass::BestEffort => "best-effort",
    }
}

fn reproducibility_cause_label(cause: ReproducibilityCause) -> &'static str {
    match cause {
        ReproducibilityCause::HostBound => "host-bound",
        ReproducibilityCause::StateBound => "state-bound",
        ReproducibilityCause::TimeBound => "time-bound",
        ReproducibilityCause::NetworkBound => "network-bound",
        ReproducibilityCause::UnknownDependencyOutput => "unknown-dependency-output",
        ReproducibilityCause::UnknownRuntimeIdentity => "unknown-runtime-identity",
        ReproducibilityCause::UntrackedEnvironment => "untracked-environment",
        ReproducibilityCause::UntrackedFilesystemView => "untracked-filesystem-view",
        ReproducibilityCause::LifecycleUnknown => "lifecycle-unknown",
    }
}

fn print_preview_view(view: &PreviewLockView) {
    println!("Preview target: {}", view.target.input);
    println!("  Durable lock state: {}", view.preview.durable_lock_state);
    println!("  Init workspace outputs:");
    for output in &view.preview.durable_materialization.outputs {
        println!("    - {}: {}", output.label, output.path);
    }
    println!("  Run attempt outputs:");
    for output in &view.preview.run_attempt_materialization.outputs {
        println!("    - {}: {}", output.label, output.path);
    }
    if !view.preview.unresolved.is_empty() {
        println!("  Unresolved: {}", view.preview.unresolved.len());
    }
    if !view.preview.diagnostics.is_empty() {
        println!("  Diagnostics: {}", view.preview.diagnostics.len());
    }
}

fn print_diagnostics_view(view: &DiagnosticsLockView) {
    println!("Diagnostics target: {}", view.target.input);
    for diagnostic in &view.diagnostics {
        println!(
            "  - [{}] {}: {}",
            diagnostic.severity, diagnostic.lock_path, diagnostic.message
        );
        println!("    inspect: {}", diagnostic.inspect_command);
        println!("    preview: {}", diagnostic.preview_command);
    }
}

fn print_remediation_view(view: &RemediationLockView) {
    println!("Remediation target: {}", view.target.input);
    for suggestion in &view.suggestions {
        println!("  - {} ({})", suggestion.lock_path, suggestion.reason_class);
        println!("    {}", suggestion.recommended_action);
        if let Some(mapping) = suggestion.source_mapping.as_ref() {
            if let Some(path) = mapping.source_path.as_ref() {
                println!("    source: {}", path);
            }
        }
    }
}

impl InspectRequirementsError {
    fn target_not_found(input: &str, reason: impl Into<String>) -> Self {
        Self {
            code: "TARGET_NOT_FOUND",
            message: "Could not resolve target".to_string(),
            details: json!({
                "input": input,
                "reason": reason.into(),
            }),
        }
    }

    fn capsule_toml_not_found(input: &str, reason: impl Into<String>) -> Self {
        Self {
            code: "CAPSULE_TOML_NOT_FOUND",
            message: "capsule.toml was not found".to_string(),
            details: json!({
                "input": input,
                "reason": reason.into(),
            }),
        }
    }

    fn requirements_resolution_failed(input: &str, reason: impl Into<String>) -> Self {
        Self {
            code: "REQUIREMENTS_RESOLUTION_FAILED",
            message: "Could not resolve requirements from capsule.toml".to_string(),
            details: json!({
                "input": input,
                "reason": reason.into(),
            }),
        }
    }

    fn emit_json(&self) {
        let payload = InspectRequirementsErrorEnvelope {
            error: InspectRequirementsErrorPayload {
                code: self.code,
                message: &self.message,
                details: &self.details,
            },
        };

        if let Ok(serialized) = serde_json::to_string(&payload) {
            eprintln!("{serialized}");
        }
    }
}

async fn resolve_target(
    input: &str,
    registry: Option<&str>,
) -> Result<ResolvedInspection, InspectRequirementsError> {
    let expanded_path = crate::local_input::expand_local_path(input);
    let should_treat_as_local =
        crate::local_input::should_treat_input_as_local(input, &expanded_path);
    if should_treat_as_local {
        return resolve_local_target(input, &expanded_path);
    }

    resolve_remote_target(input, registry).await
}

fn resolve_local_target(
    input: &str,
    expanded_path: &Path,
) -> Result<ResolvedInspection, InspectRequirementsError> {
    if !expanded_path.exists() {
        return Err(InspectRequirementsError::target_not_found(
            input,
            format!("Local path does not exist: {}", expanded_path.display()),
        ));
    }

    let resolved_path = expanded_path.canonicalize().map_err(|err| {
        InspectRequirementsError::target_not_found(
            input,
            format!(
                "Failed to resolve local path '{}': {err}",
                expanded_path.display()
            ),
        )
    })?;

    let manifest_path = if resolved_path.is_dir() {
        resolved_path.join("capsule.toml")
    } else {
        resolved_path.clone()
    };
    if !manifest_path.exists() {
        return Err(InspectRequirementsError::capsule_toml_not_found(
            input,
            format!("Expected manifest at {}", manifest_path.display()),
        ));
    }

    let loaded = manifest::load_manifest(&manifest_path).map_err(|err| {
        InspectRequirementsError::requirements_resolution_failed(input, err.to_string())
    })?;

    Ok(ResolvedInspection {
        target: InspectTarget {
            input: input.to_string(),
            kind: "local",
            resolved: ResolvedTarget::Local {
                path: resolved_path.display().to_string(),
            },
        },
        manifest: loaded.model,
    })
}

async fn resolve_remote_target(
    input: &str,
    registry: Option<&str>,
) -> Result<ResolvedInspection, InspectRequirementsError> {
    let scoped_ref = crate::install::parse_capsule_ref(input).map_err(|err| {
        InspectRequirementsError::target_not_found(input, format!("Invalid remote ref: {err}"))
    })?;
    let manifest_toml = crate::install::fetch_capsule_manifest_toml(input, registry)
        .await
        .map_err(|err| classify_remote_error(input, err))?;
    let manifest = parse_remote_manifest(input, &manifest_toml)?;

    Ok(ResolvedInspection {
        target: InspectTarget {
            input: input.to_string(),
            kind: "remote",
            resolved: ResolvedTarget::Remote {
                publisher: scoped_ref.publisher,
                slug: scoped_ref.slug,
            },
        },
        manifest,
    })
}

fn parse_remote_manifest(
    input: &str,
    manifest_toml: &str,
) -> Result<CapsuleManifest, InspectRequirementsError> {
    let manifest = CapsuleManifest::from_toml(manifest_toml).map_err(|err| {
        InspectRequirementsError::requirements_resolution_failed(
            input,
            format!("Failed to parse remote capsule.toml: {err}"),
        )
    })?;

    if let Err(errors) = manifest.validate() {
        let details = errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(InspectRequirementsError::requirements_resolution_failed(
            input,
            format!("Remote capsule.toml validation failed: {details}"),
        ));
    }

    Ok(manifest)
}

fn classify_remote_error(input: &str, err: anyhow::Error) -> InspectRequirementsError {
    let message = err.to_string();
    if message.contains("Capsule not found") {
        InspectRequirementsError::target_not_found(input, message)
    } else if message.contains("capsule.toml") {
        InspectRequirementsError::capsule_toml_not_found(input, message)
    } else {
        InspectRequirementsError::requirements_resolution_failed(input, message)
    }
}

fn build_requirements(manifest: &CapsuleManifest) -> RequirementCategories {
    let (secrets, env) = build_env_requirements(manifest);
    let state = build_state_requirements(manifest);
    let network = build_network_requirements(manifest);
    let services = build_service_requirements(manifest);
    let consent = build_consent_requirements(&secrets, &state, &network);

    RequirementCategories {
        secrets,
        state,
        env,
        network,
        services,
        consent,
    }
}

fn build_env_requirements(
    manifest: &CapsuleManifest,
) -> (Vec<SecretRequirement>, Vec<EnvRequirement>) {
    let mut entries = BTreeMap::<String, EnvRequirementAccumulator>::new();

    if let Some(targets) = manifest.targets.as_ref() {
        for (target_label, target) in &targets.named {
            for key in &target.required_env {
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                let entry = entries.entry(key.to_string()).or_default();
                entry.required = true;
                entry.required_targets.insert(target_label.clone());
            }
        }
    }

    if let Some(isolation) = manifest.isolation.as_ref() {
        for key in &isolation.allow_env {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            entries.entry(key.to_string()).or_default().allowlisted = true;
        }
    }

    let mut secrets = Vec::new();
    let mut env = Vec::new();

    for (key, entry) in entries {
        let description = env_requirement_description(&entry);
        if is_secret_like_key(&key) {
            secrets.push(SecretRequirement {
                key,
                required: entry.required,
                description,
            });
        } else {
            env.push(EnvRequirement {
                key,
                required: entry.required,
                description,
            });
        }
    }

    (secrets, env)
}

fn build_state_requirements(manifest: &CapsuleManifest) -> Vec<StateRequirementItem> {
    let mut items = manifest
        .state
        .iter()
        .map(|(key, requirement)| StateRequirementItem {
            key: key.clone(),
            required: true,
            description: None,
            kind: Some(requirement.kind),
            durability: Some(requirement.durability),
            purpose: (!requirement.purpose.trim().is_empty()).then(|| requirement.purpose.clone()),
            attach: Some(requirement.attach),
            schema_id: requirement.schema_id.clone(),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.key.cmp(&right.key));
    items
}

fn build_network_requirements(manifest: &CapsuleManifest) -> Vec<NetworkRequirement> {
    let Some(network) = manifest.network.as_ref() else {
        return Vec::new();
    };

    let mut hosts = network
        .egress_allow
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    hosts.sort();
    hosts.dedup();

    let mut identities = network
        .egress_id_allow
        .iter()
        .map(|rule| NetworkIdentity {
            kind: egress_id_type_as_str(&rule.rule_type).to_string(),
            value: rule.value.clone(),
        })
        .collect::<Vec<_>>();
    identities.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then(left.value.cmp(&right.value))
    });

    if hosts.is_empty() && identities.is_empty() {
        return Vec::new();
    }

    vec![NetworkRequirement {
        key: NETWORK_REQUIREMENT_KEY.to_string(),
        required: true,
        description: Some("Requires outbound network access".to_string()),
        hosts,
        identities,
    }]
}

fn build_service_requirements(manifest: &CapsuleManifest) -> Vec<ServiceRequirement> {
    let Some(services) = manifest.services.as_ref() else {
        return Vec::new();
    };

    let mut items = services
        .iter()
        .map(|(key, service)| ServiceRequirement {
            key: key.clone(),
            required: true,
            description: Some(service_description(key, service)),
            target: service.target.clone(),
            depends_on: service
                .depends_on
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|dependency| !dependency.trim().is_empty())
                .collect(),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.key.cmp(&right.key));
    items
}

fn build_consent_requirements(
    secrets: &[SecretRequirement],
    state: &[StateRequirementItem],
    network: &[NetworkRequirement],
) -> Vec<ConsentRequirement> {
    let mut items = Vec::new();

    if !network.is_empty() {
        items.push(ConsentRequirement {
            key: CONSENT_NETWORK_EGRESS_KEY.to_string(),
            required: true,
            description: Some("Requires consent for outbound network access".to_string()),
        });
    }

    if !state.is_empty() {
        items.push(ConsentRequirement {
            key: CONSENT_FILESYSTEM_WRITE_KEY.to_string(),
            required: true,
            description: Some("Writes files to bound application state".to_string()),
        });
    }

    if !secrets.is_empty() {
        items.push(ConsentRequirement {
            key: CONSENT_SECRETS_ACCESS_KEY.to_string(),
            required: true,
            description: Some("Requires secret provisioning before launch".to_string()),
        });
    }

    items
}

fn env_requirement_description(entry: &EnvRequirementAccumulator) -> Option<String> {
    if entry.required {
        if entry.required_targets.is_empty() {
            return Some("Required environment variable declared in capsule.toml".to_string());
        }

        return Some(format!(
            "Required environment variable for target(s): {}",
            entry
                .required_targets
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if entry.allowlisted {
        return Some("Optional host environment variable passthrough".to_string());
    }

    None
}

fn is_secret_like_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || upper.ends_with("_KEY")
}

fn service_description(name: &str, service: &ServiceSpec) -> String {
    if name == "main" {
        return "Primary runtime service".to_string();
    }
    if let Some(target) = service
        .target
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!("Service declared in capsule.toml targeting '{target}'");
    }
    "Service declared in capsule.toml".to_string()
}

fn egress_id_type_as_str(value: &EgressIdType) -> &'static str {
    match value {
        EgressIdType::Ip => "ip",
        EgressIdType::Cidr => "cidr",
        EgressIdType::Spiffe => "spiffe",
    }
}

fn print_human_readable(result: &InspectRequirementsResult) {
    println!("Requirements for {}", result.target.input);
    print_category(
        "Secrets",
        result.requirements.secrets.iter().map(|item| &item.key),
    );
    print_category(
        "State",
        result.requirements.state.iter().map(|item| &item.key),
    );
    print_category("Env", result.requirements.env.iter().map(|item| &item.key));
    print_category(
        "Network",
        result.requirements.network.iter().map(|item| &item.key),
    );
    print_category(
        "Services",
        result.requirements.services.iter().map(|item| &item.key),
    );
    print_category(
        "Consent",
        result.requirements.consent.iter().map(|item| &item.key),
    );
}

fn print_category<'a>(label: &str, values: impl Iterator<Item = &'a String>) {
    let values = values.cloned().collect::<Vec<_>>();
    if values.is_empty() {
        println!("  {label}: none");
    } else {
        println!("  {label}: {}", values.join(", "));
    }
}
