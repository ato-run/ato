//! Component-level execution-receipt drift diff (#496).
//!
//! Two execution receipts can carry the same launch *intent* yet resolve to
//! different concrete objects, or describe a different launch graph entirely.
//! Comparing only `execution_id` answers "did anything change?" but never
//! "*what* changed?". This module answers the second question: it compares the
//! declared / resolved graph evidence of two receipts and reports the specific
//! nodes, edges, and facet fields that differ, each classified as
//! [`DriftClass::DeclaredDrift`] or [`DriftClass::ResolvedDrift`].
//!
//! ## Drift domains
//!
//! * [`DriftClass::DeclaredDrift`] — the *requested* launch changed: manifest /
//!   launch profile / network policy / entrypoint / state declaration. These are
//!   the declared launch inputs a user controls.
//! * [`DriftClass::ResolvedDrift`] — Ato *resolved* a different concrete object:
//!   a runtime / tool hash, dependency output, materialized source/artifact, or
//!   service-graph projection. These are host-resolution outputs.
//! * [`DriftClass::ObservedDrift`] — the *observed launch envelope* changed
//!   (#496): two runtime-observed receipts (#490) bound a different
//!   `observed_execution_id` or differ in an envelope fact (runtime
//!   kind/identity, entrypoint, working directory, env **keys**, mount
//!   **targets**, provider-projection digest). This is **envelope-level only**:
//!   the differ still does NOT compare per-node/edge lifecycle `status`
//!   (#495/#521/#522), and never compares diagnostic/ephemeral facts (PID,
//!   container id, bound port, local URL, log path, session id, timestamps), so
//!   it never claims runtime *behaviour* equivalence — only whether the observed
//!   launch envelope changed. Receipts with no observed layer emit no
//!   `ObservedDrift`.
//!
//! When classification is ambiguous the differ is conservative: a field is only
//! [`DriftClass::ResolvedDrift`] when it is clearly a resolved / provider-derived
//! value; everything else (declared graph / launch-input fields, and unknown
//! node kinds) is [`DriftClass::DeclaredDrift`]. The differ never claims runtime
//! observation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    ExecutionReceipt, ExecutionReceiptDocument, ExecutionReceiptV2, LaunchArg,
    ObservedLaunchEnvelope, Tracked, TrackingStatus,
};

const COMPONENT_NODE: &str = "node";
const COMPONENT_EDGE: &str = "edge";
const COMPONENT_RECEIPT_FIELD: &str = "receipt_field";
const COMPONENT_PROVIDER_PROJECTION: &str = "provider_projection";

/// Domain a [`ReceiptDriftChange`] belongs to.
///
/// See the module docs for the declared/resolved/observed distinction.
/// `ObservedDrift` covers the runtime-observed launch envelope (#490/#496); it
/// never reflects diagnostic facts or per-node lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DriftClass {
    /// The declared launch request changed (manifest / launch profile / network
    /// policy / entrypoint / state declaration).
    DeclaredDrift,
    /// Ato resolved a different concrete object (runtime/tool hash, dependency
    /// output, materialized source/artifact, provider/service projection).
    ResolvedDrift,
    /// Reserved seam for runtime-observation drift (#495 / #521 / #522). Never
    /// emitted in v1 — the differ does not observe the runtime.
    ObservedDrift,
}

impl DriftClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriftClass::DeclaredDrift => "declared-drift",
            DriftClass::ResolvedDrift => "resolved-drift",
            DriftClass::ObservedDrift => "observed-drift",
        }
    }

    /// Short, human-facing descriptor of what a change in this domain means.
    fn descriptor(&self) -> &'static str {
        match self {
            DriftClass::DeclaredDrift => "declared launch input",
            DriftClass::ResolvedDrift => "resolved/provider-derived value",
            DriftClass::ObservedDrift => "observed value",
        }
    }
}

/// One component-level difference between two receipts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptDriftChange {
    pub class: DriftClass,
    /// `node` | `edge` | `receipt_field` | `provider_projection`.
    pub component_kind: String,
    /// Stable identifier of the component: a node identifier, an
    /// `from -> to` edge label, a receipt field path, or a provider/service
    /// label.
    pub component_id: String,
    /// The specific field that changed within the component.
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Value>,
    pub reason: String,
}

/// Structured, machine-readable drift report between two receipts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptDriftReport {
    /// Top-level `execution_id` of the old receipt (the JCS-derived canonical
    /// identity), surfaced as a summary alongside the localized changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_execution_id: Option<String>,
    pub has_drift: bool,
    pub changes: Vec<ReceiptDriftChange>,
}

/// Error raised when a receipt cannot be compared for drift.
///
/// Parsing / IO errors are handled at the CLI boundary; this enum only covers
/// the pure-layer condition where a parsed receipt carries nothing the differ
/// can compare. For any valid v1/v2 receipt this never fires (every receipt has
/// at least an `execution_id`), so it is a defensive guard rather than a
/// routine outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftError {
    /// Neither receipt carried any comparable graph evidence.
    NoComparableEvidence,
}

impl std::fmt::Display for DriftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriftError::NoComparableEvidence => write!(
                f,
                "receipts have no comparable graph evidence (no execution id, facet, node, edge, \
                 or provider projection to diff)"
            ),
        }
    }
}

impl std::error::Error for DriftError {}

/// Compare two execution-receipt documents and report component-level drift.
///
/// The primary path is v2-vs-v2, where node/edge/provider projections exist.
/// v1 (legacy) receipts are normalized into the same comparable shape and
/// compared on their shared facets (source / dependencies / runtime / policy /
/// launch); they carry no graph projection so only facet drift surfaces. Mixed
/// v1-vs-v2 comparisons fall back to whatever facet keys align.
pub fn diff_receipt_documents(
    old: &ExecutionReceiptDocument,
    new: &ExecutionReceiptDocument,
) -> Result<ReceiptDriftReport, DriftError> {
    let old = DriftSubject::from_document(old);
    let new = DriftSubject::from_document(new);
    if !old.has_comparable_evidence() && !new.has_comparable_evidence() {
        return Err(DriftError::NoComparableEvidence);
    }
    Ok(diff_subjects(&old, &new))
}

/// A receipt normalized into the fields the differ compares, independent of
/// schema version.
struct DriftSubject {
    execution_id: Option<String>,
    declared_execution_id: Option<String>,
    resolved_execution_id: Option<String>,
    observed_execution_id: Option<String>,
    declared_fields: Vec<FacetField>,
    resolved_fields: Vec<FacetField>,
    /// Observed launch-envelope facts (#490/#496). Empty for receipts with no
    /// runtime observation. Never carries diagnostic facts (bound port, etc.).
    observed_fields: Vec<FacetField>,
    nodes: Vec<NodeEntry>,
    edges: Vec<EdgeEntry>,
    provider_projections: Vec<ProjectionEntry>,
}

/// One comparable receipt facet, tagged with the graph component it belongs to.
struct FacetField {
    field: String,
    component_id: String,
    value: Value,
}

impl FacetField {
    fn new(field: impl Into<String>, component_id: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            component_id: component_id.into(),
            value,
        }
    }
}

struct NodeEntry {
    identifier: String,
    kind: String,
}

struct EdgeEntry {
    source: String,
    target: String,
    kind: String,
}

struct ProjectionEntry {
    label: String,
    value: Value,
}

impl DriftSubject {
    fn from_document(document: &ExecutionReceiptDocument) -> Self {
        match document {
            ExecutionReceiptDocument::V1(receipt) => Self::from_v1(receipt),
            ExecutionReceiptDocument::V2(receipt) => Self::from_v2(receipt),
        }
    }

    fn has_comparable_evidence(&self) -> bool {
        self.execution_id.is_some()
            || self.declared_execution_id.is_some()
            || self.resolved_execution_id.is_some()
            || self.observed_execution_id.is_some()
            || !self.declared_fields.is_empty()
            || !self.resolved_fields.is_empty()
            || !self.observed_fields.is_empty()
            || !self.nodes.is_empty()
            || !self.edges.is_empty()
            || !self.provider_projections.is_empty()
    }

    fn from_v2(receipt: &ExecutionReceiptV2) -> Self {
        let mut declared = Vec::new();
        let mut resolved = Vec::new();

        // --- Declared-domain launch inputs ---
        declared.push(FacetField::new(
            "launch.entry_point",
            "entrypoint",
            to_value(&receipt.launch.entry_point),
        ));
        declared.push(FacetField::new(
            "launch.argv",
            "entrypoint",
            argv_value(&receipt.launch.argv),
        ));
        declared.push(FacetField::new(
            "launch.working_directory",
            "entrypoint",
            tracked_value(&receipt.launch.working_directory),
        ));
        declared.push(FacetField::new(
            "policy.network_policy_hash",
            "network",
            tracked_value(&receipt.policy.network_policy_hash),
        ));
        declared.push(FacetField::new(
            "policy.capability_policy_hash",
            "capability",
            tracked_value(&receipt.policy.capability_policy_hash),
        ));
        declared.push(FacetField::new(
            "policy.sandbox_policy_hash",
            "sandbox",
            tracked_value(&receipt.policy.sandbox_policy_hash),
        ));
        declared.push(FacetField::new(
            "source.manifest_path_role",
            "source",
            tracked_value(&receipt.source.manifest_path_role),
        ));
        if let Some(declared_runtime) = &receipt.runtime.declared {
            declared.push(FacetField::new(
                "runtime.declared",
                "runtime",
                Value::String(declared_runtime.clone()),
            ));
        }
        declared.push(FacetField::new(
            "environment.mode",
            "env",
            to_value(&receipt.environment.mode),
        ));
        for entry in &receipt.environment.entries {
            // Env entries are a declared launch input captured into identity;
            // each key's value hash is compared individually so a changed key
            // surfaces as a component-level field, not a single opaque blob.
            declared.push(FacetField::new(
                format!("environment.entry.{}", entry.key),
                "env",
                tracked_value(&entry.value_hash),
            ));
        }
        // The declared shape of a persistent-state binding (its name + kind).
        for binding in &receipt.filesystem.persistent_state {
            declared.push(FacetField::new(
                format!("filesystem.state.{}", binding.name),
                "state",
                to_value(&binding.kind),
            ));
        }

        // --- Resolved-domain concrete objects ---
        resolved.push(FacetField::new(
            "source.source_tree_hash",
            "source",
            tracked_value(&receipt.source.source_tree_hash),
        ));
        resolved.push(FacetField::new(
            "runtime.resolved_ref",
            "runtime",
            tracked_value(&receipt.runtime.resolved_ref),
        ));
        resolved.push(FacetField::new(
            "runtime.binary_hash",
            "runtime",
            tracked_value(&receipt.runtime.binary_hash),
        ));
        resolved.push(FacetField::new(
            "runtime.dynamic_linkage",
            "runtime",
            tracked_value(&receipt.runtime.dynamic_linkage),
        ));
        resolved.push(FacetField::new(
            "dependencies.derivation_hash",
            "dependency-output",
            tracked_value(&receipt.dependencies.derivation_hash),
        ));
        resolved.push(FacetField::new(
            "dependencies.output_hash",
            "dependency-output",
            tracked_value(&receipt.dependencies.output_hash),
        ));
        resolved.push(FacetField::new(
            "filesystem.view_hash",
            "filesystem",
            tracked_value(&receipt.filesystem.view_hash),
        ));
        // The resolved/materialized identity bound to each state binding.
        for binding in &receipt.filesystem.persistent_state {
            resolved.push(FacetField::new(
                format!("filesystem.state.{}.identity", binding.name),
                "state",
                tracked_value(&binding.identity),
            ));
        }

        let nodes = receipt
            .node_receipts
            .iter()
            .map(|node| NodeEntry {
                identifier: node.node_identifier.clone(),
                kind: node.kind.clone(),
            })
            .collect();
        let edges = receipt
            .edge_receipts
            .iter()
            .map(|edge| EdgeEntry {
                source: edge.source.clone(),
                target: edge.target.clone(),
                kind: edge.kind.clone(),
            })
            .collect();
        let provider_projections = receipt
            .provider_projections
            .iter()
            .enumerate()
            .map(|(index, projection)| ProjectionEntry {
                label: projection
                    .service_label
                    .clone()
                    .unwrap_or_else(|| format!("projection[{index}]")),
                value: to_value(projection),
            })
            .collect();

        // --- Observed-domain launch envelope (#490/#496) ---
        // Only present once the workload was runtime-observed; envelope facts
        // only (no diagnostic bound port / local URL, no resolved-id anchor).
        let observed_fields = receipt
            .observed_runtime
            .as_ref()
            .map(|evidence| observed_envelope_fields(&evidence.envelope))
            .unwrap_or_default();

        Self {
            execution_id: Some(receipt.execution_id.clone()),
            declared_execution_id: receipt.declared_execution_id.clone(),
            resolved_execution_id: receipt.resolved_execution_id.clone(),
            observed_execution_id: receipt.observed_execution_id.clone(),
            declared_fields: declared,
            resolved_fields: resolved,
            observed_fields,
            nodes,
            edges,
            provider_projections,
        }
    }

    fn from_v1(receipt: &ExecutionReceipt) -> Self {
        let mut declared = Vec::new();
        let mut resolved = Vec::new();

        // --- Declared-domain launch inputs ---
        declared.push(FacetField::new(
            "launch.entry_point",
            "entrypoint",
            Value::String(receipt.launch.entry_point.clone()),
        ));
        declared.push(FacetField::new(
            "launch.argv",
            "entrypoint",
            to_value(&receipt.launch.argv),
        ));
        declared.push(FacetField::new(
            "launch.working_directory",
            "entrypoint",
            Value::String(receipt.launch.working_directory.clone()),
        ));
        declared.push(FacetField::new(
            "policy.network_policy_hash",
            "network",
            tracked_value(&receipt.policy.network_policy_hash),
        ));
        declared.push(FacetField::new(
            "policy.capability_policy_hash",
            "capability",
            tracked_value(&receipt.policy.capability_policy_hash),
        ));
        declared.push(FacetField::new(
            "policy.sandbox_policy_hash",
            "sandbox",
            tracked_value(&receipt.policy.sandbox_policy_hash),
        ));
        declared.push(FacetField::new(
            "source.source_ref",
            "source",
            tracked_value(&receipt.source.source_ref),
        ));
        declared.push(FacetField::new(
            "environment.mode",
            "env",
            to_value(&receipt.environment.mode),
        ));
        declared.push(FacetField::new(
            "environment.closure_hash",
            "env",
            tracked_value(&receipt.environment.closure_hash),
        ));
        if let Some(declared_runtime) = &receipt.runtime.declared {
            declared.push(FacetField::new(
                "runtime.declared",
                "runtime",
                Value::String(declared_runtime.clone()),
            ));
        }

        // --- Resolved-domain concrete objects ---
        resolved.push(FacetField::new(
            "source.source_tree_hash",
            "source",
            tracked_value(&receipt.source.source_tree_hash),
        ));
        if let Some(resolved_runtime) = &receipt.runtime.resolved {
            // Keyed as `runtime.resolved_ref` to align with the v2 field name so
            // a v1-vs-v2 comparison of a Known resolved runtime can still match.
            resolved.push(FacetField::new(
                "runtime.resolved_ref",
                "runtime",
                Value::String(resolved_runtime.clone()),
            ));
        }
        resolved.push(FacetField::new(
            "runtime.binary_hash",
            "runtime",
            tracked_value(&receipt.runtime.binary_hash),
        ));
        resolved.push(FacetField::new(
            "runtime.dynamic_linkage",
            "runtime",
            tracked_value(&receipt.runtime.dynamic_linkage),
        ));
        resolved.push(FacetField::new(
            "dependencies.derivation_hash",
            "dependency-output",
            tracked_value(&receipt.dependencies.derivation_hash),
        ));
        resolved.push(FacetField::new(
            "dependencies.output_hash",
            "dependency-output",
            tracked_value(&receipt.dependencies.output_hash),
        ));
        resolved.push(FacetField::new(
            "filesystem.view_hash",
            "filesystem",
            tracked_value(&receipt.filesystem.view_hash),
        ));

        Self {
            execution_id: Some(receipt.execution_id.clone()),
            declared_execution_id: None,
            resolved_execution_id: None,
            // v1 receipts predate the runtime-observation layer.
            observed_execution_id: None,
            declared_fields: declared,
            resolved_fields: resolved,
            observed_fields: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            provider_projections: Vec::new(),
        }
    }
}

/// Project the observed launch envelope (#490/#496) into comparable facet
/// fields. Only the canonical envelope facts are compared; the diagnostic facts
/// on [`super::ObservedRuntimeEvidence`] (bound port, local URL) and the derived
/// `resolved_execution_id` anchor are intentionally excluded, so observed drift
/// reflects what the workload actually bound — never a runtime-assigned port or
/// a value already covered by `resolved_execution_id`.
fn observed_envelope_fields(envelope: &ObservedLaunchEnvelope) -> Vec<FacetField> {
    vec![
        FacetField::new(
            "observed.runtime_kind",
            "runtime",
            to_value(&envelope.runtime_kind),
        ),
        FacetField::new(
            "observed.runtime_identity",
            "runtime",
            to_value(&envelope.runtime_identity),
        ),
        FacetField::new(
            "observed.entrypoint",
            "entrypoint",
            to_value(&envelope.entrypoint),
        ),
        FacetField::new(
            "observed.working_directory",
            "entrypoint",
            to_value(&envelope.working_directory),
        ),
        FacetField::new("observed.env_keys", "env", to_value(&envelope.env_keys)),
        FacetField::new(
            "observed.mount_targets",
            "filesystem",
            to_value(&envelope.mount_targets),
        ),
        FacetField::new(
            "observed.provider_projection_digest",
            "provider",
            to_value(&envelope.provider_projection_digest),
        ),
    ]
}

fn diff_subjects(old: &DriftSubject, new: &DriftSubject) -> ReceiptDriftReport {
    let mut changes = Vec::new();

    diff_execution_id(
        &mut changes,
        "declared_execution_id",
        DriftClass::DeclaredDrift,
        &old.declared_execution_id,
        &new.declared_execution_id,
    );
    diff_execution_id(
        &mut changes,
        "resolved_execution_id",
        DriftClass::ResolvedDrift,
        &old.resolved_execution_id,
        &new.resolved_execution_id,
    );
    diff_execution_id(
        &mut changes,
        "observed_execution_id",
        DriftClass::ObservedDrift,
        &old.observed_execution_id,
        &new.observed_execution_id,
    );

    diff_facet_fields(
        &mut changes,
        DriftClass::DeclaredDrift,
        &old.declared_fields,
        &new.declared_fields,
    );
    diff_facet_fields(
        &mut changes,
        DriftClass::ResolvedDrift,
        &old.resolved_fields,
        &new.resolved_fields,
    );
    diff_facet_fields(
        &mut changes,
        DriftClass::ObservedDrift,
        &old.observed_fields,
        &new.observed_fields,
    );

    diff_nodes(&mut changes, &old.nodes, &new.nodes);
    diff_edges(&mut changes, &old.edges, &new.edges);
    diff_projections(
        &mut changes,
        &old.provider_projections,
        &new.provider_projections,
    );

    // Defensive fallback: if the canonical execution_id changed but no facet or
    // graph difference was localized (e.g. a facet outside the compared set
    // moved), still surface that drift exists rather than silently report none.
    if changes.is_empty() {
        if let (Some(old_id), Some(new_id)) = (&old.execution_id, &new.execution_id) {
            if old_id != new_id {
                changes.push(ReceiptDriftChange {
                    class: DriftClass::DeclaredDrift,
                    component_kind: COMPONENT_RECEIPT_FIELD.to_string(),
                    component_id: "execution_id".to_string(),
                    field: "execution_id".to_string(),
                    old: Some(Value::String(old_id.clone())),
                    new: Some(Value::String(new_id.clone())),
                    reason: "execution_id changed but no component-level difference could be \
                             localized from the comparable evidence"
                        .to_string(),
                });
            }
        }
    }

    ReceiptDriftReport {
        old_execution_id: old.execution_id.clone(),
        new_execution_id: new.execution_id.clone(),
        has_drift: !changes.is_empty(),
        changes,
    }
}

fn diff_execution_id(
    changes: &mut Vec<ReceiptDriftChange>,
    field: &str,
    class: DriftClass,
    old: &Option<String>,
    new: &Option<String>,
) {
    if old == new {
        return;
    }
    changes.push(ReceiptDriftChange {
        class,
        component_kind: COMPONENT_RECEIPT_FIELD.to_string(),
        component_id: field.to_string(),
        field: field.to_string(),
        old: old.clone().map(Value::String),
        new: new.clone().map(Value::String),
        reason: format!("{field} changed"),
    });
}

fn diff_facet_fields(
    changes: &mut Vec<ReceiptDriftChange>,
    class: DriftClass,
    old: &[FacetField],
    new: &[FacetField],
) {
    let old_map: BTreeMap<&str, &FacetField> = old.iter().map(|f| (f.field.as_str(), f)).collect();
    let new_map: BTreeMap<&str, &FacetField> = new.iter().map(|f| (f.field.as_str(), f)).collect();

    let mut keys: BTreeSet<&str> = old_map.keys().copied().collect();
    keys.extend(new_map.keys().copied());

    let descriptor = class.descriptor();
    for key in keys {
        match (old_map.get(key), new_map.get(key)) {
            (Some(old_field), Some(new_field)) if old_field.value != new_field.value => {
                changes.push(ReceiptDriftChange {
                    class,
                    component_kind: COMPONENT_RECEIPT_FIELD.to_string(),
                    component_id: new_field.component_id.clone(),
                    field: key.to_string(),
                    old: Some(old_field.value.clone()),
                    new: Some(new_field.value.clone()),
                    reason: format!("{descriptor} '{key}' changed"),
                });
            }
            (Some(old_field), None) => {
                changes.push(ReceiptDriftChange {
                    class,
                    component_kind: COMPONENT_RECEIPT_FIELD.to_string(),
                    component_id: old_field.component_id.clone(),
                    field: key.to_string(),
                    old: Some(old_field.value.clone()),
                    new: None,
                    reason: format!("{descriptor} '{key}' present only in old receipt"),
                });
            }
            (None, Some(new_field)) => {
                changes.push(ReceiptDriftChange {
                    class,
                    component_kind: COMPONENT_RECEIPT_FIELD.to_string(),
                    component_id: new_field.component_id.clone(),
                    field: key.to_string(),
                    old: None,
                    new: Some(new_field.value.clone()),
                    reason: format!("{descriptor} '{key}' present only in new receipt"),
                });
            }
            _ => {}
        }
    }
}

fn diff_nodes(changes: &mut Vec<ReceiptDriftChange>, old: &[NodeEntry], new: &[NodeEntry]) {
    let old_map: BTreeMap<&str, &str> = old
        .iter()
        .map(|n| (n.identifier.as_str(), n.kind.as_str()))
        .collect();
    let new_map: BTreeMap<&str, &str> = new
        .iter()
        .map(|n| (n.identifier.as_str(), n.kind.as_str()))
        .collect();

    let mut ids: BTreeSet<&str> = old_map.keys().copied().collect();
    ids.extend(new_map.keys().copied());

    for id in ids {
        match (old_map.get(id), new_map.get(id)) {
            (Some(old_kind), Some(new_kind)) if old_kind != new_kind => {
                changes.push(ReceiptDriftChange {
                    class: node_kind_class(new_kind),
                    component_kind: COMPONENT_NODE.to_string(),
                    component_id: id.to_string(),
                    field: "kind".to_string(),
                    old: Some(Value::String((*old_kind).to_string())),
                    new: Some(Value::String((*new_kind).to_string())),
                    reason: format!("graph node kind changed from '{old_kind}' to '{new_kind}'"),
                });
            }
            (Some(old_kind), None) => {
                changes.push(ReceiptDriftChange {
                    class: node_kind_class(old_kind),
                    component_kind: COMPONENT_NODE.to_string(),
                    component_id: id.to_string(),
                    field: "presence".to_string(),
                    old: Some(Value::String((*old_kind).to_string())),
                    new: None,
                    reason: format!("graph node removed ({old_kind})"),
                });
            }
            (None, Some(new_kind)) => {
                changes.push(ReceiptDriftChange {
                    class: node_kind_class(new_kind),
                    component_kind: COMPONENT_NODE.to_string(),
                    component_id: id.to_string(),
                    field: "presence".to_string(),
                    old: None,
                    new: Some(Value::String((*new_kind).to_string())),
                    reason: format!("graph node added ({new_kind})"),
                });
            }
            _ => {}
        }
    }
}

fn diff_edges(changes: &mut Vec<ReceiptDriftChange>, old: &[EdgeEntry], new: &[EdgeEntry]) {
    // Edges have no explicit id; key by (source, target, kind) per #496. A kind
    // change therefore surfaces as a removed + added pair, which we re-pair into
    // a single "kind changed" when exactly one edge per (source, target) moved.
    let edge_key = |edge: &EdgeEntry| (edge.source.clone(), edge.target.clone(), edge.kind.clone());
    let old_set: BTreeSet<(String, String, String)> = old.iter().map(edge_key).collect();
    let new_set: BTreeSet<(String, String, String)> = new.iter().map(edge_key).collect();

    // (source, target) -> (kinds only in old, kinds only in new)
    let mut by_pair: BTreeMap<(String, String), (BTreeSet<String>, BTreeSet<String>)> =
        BTreeMap::new();
    for edge in old {
        if !new_set.contains(&edge_key(edge)) {
            by_pair
                .entry((edge.source.clone(), edge.target.clone()))
                .or_default()
                .0
                .insert(edge.kind.clone());
        }
    }
    for edge in new {
        if !old_set.contains(&edge_key(edge)) {
            by_pair
                .entry((edge.source.clone(), edge.target.clone()))
                .or_default()
                .1
                .insert(edge.kind.clone());
        }
    }

    for ((source, target), (removed, added)) in by_pair {
        let label = format!("{source} -> {target}");
        if removed.len() == 1 && added.len() == 1 {
            let old_kind = removed.iter().next().expect("one removed kind");
            let new_kind = added.iter().next().expect("one added kind");
            if let Some(class) = edge_kind_class(new_kind).or_else(|| edge_kind_class(old_kind)) {
                changes.push(ReceiptDriftChange {
                    class,
                    component_kind: COMPONENT_EDGE.to_string(),
                    component_id: label,
                    field: "kind".to_string(),
                    old: Some(Value::String(old_kind.clone())),
                    new: Some(Value::String(new_kind.clone())),
                    reason: format!("graph edge kind changed from '{old_kind}' to '{new_kind}'"),
                });
            }
            continue;
        }
        for kind in &removed {
            let Some(class) = edge_kind_class(kind) else {
                continue;
            };
            changes.push(ReceiptDriftChange {
                class,
                component_kind: COMPONENT_EDGE.to_string(),
                component_id: label.clone(),
                field: "presence".to_string(),
                old: Some(Value::String(kind.clone())),
                new: None,
                reason: format!("graph edge removed ({kind})"),
            });
        }
        for kind in &added {
            let Some(class) = edge_kind_class(kind) else {
                continue;
            };
            changes.push(ReceiptDriftChange {
                class,
                component_kind: COMPONENT_EDGE.to_string(),
                component_id: label.clone(),
                field: "presence".to_string(),
                old: None,
                new: Some(Value::String(kind.clone())),
                reason: format!("graph edge added ({kind})"),
            });
        }
    }
}

fn diff_projections(
    changes: &mut Vec<ReceiptDriftChange>,
    old: &[ProjectionEntry],
    new: &[ProjectionEntry],
) {
    let old_map: BTreeMap<&str, &Value> =
        old.iter().map(|p| (p.label.as_str(), &p.value)).collect();
    let new_map: BTreeMap<&str, &Value> =
        new.iter().map(|p| (p.label.as_str(), &p.value)).collect();

    let mut labels: BTreeSet<&str> = old_map.keys().copied().collect();
    labels.extend(new_map.keys().copied());

    for label in labels {
        // Provider projections are a resolved/provider-derived view of what the
        // realizer was asked to launch — always ResolvedDrift.
        match (old_map.get(label), new_map.get(label)) {
            (Some(old_value), Some(new_value)) if old_value != new_value => {
                changes.push(ReceiptDriftChange {
                    class: DriftClass::ResolvedDrift,
                    component_kind: COMPONENT_PROVIDER_PROJECTION.to_string(),
                    component_id: label.to_string(),
                    field: "projection".to_string(),
                    old: Some((*old_value).clone()),
                    new: Some((*new_value).clone()),
                    reason: format!("provider projection '{label}' changed"),
                });
            }
            (Some(old_value), None) => {
                changes.push(ReceiptDriftChange {
                    class: DriftClass::ResolvedDrift,
                    component_kind: COMPONENT_PROVIDER_PROJECTION.to_string(),
                    component_id: label.to_string(),
                    field: "projection".to_string(),
                    old: Some((*old_value).clone()),
                    new: None,
                    reason: format!("provider projection '{label}' present only in old receipt"),
                });
            }
            (None, Some(new_value)) => {
                changes.push(ReceiptDriftChange {
                    class: DriftClass::ResolvedDrift,
                    component_kind: COMPONENT_PROVIDER_PROJECTION.to_string(),
                    component_id: label.to_string(),
                    field: "projection".to_string(),
                    old: None,
                    new: Some((*new_value).clone()),
                    reason: format!("provider projection '{label}' present only in new receipt"),
                });
            }
            _ => {}
        }
    }
}

/// Map a graph node kind label to its drift domain.
///
/// Concrete objects Ato resolves on this host are [`DriftClass::ResolvedDrift`];
/// declared launch-input nodes (and any unrecognized kind, conservatively) are
/// [`DriftClass::DeclaredDrift`]. Node kind labels are the stable strings from
/// `ExecutionGraphNode::kind_label`.
fn node_kind_class(kind: &str) -> DriftClass {
    match kind {
        "runtime" | "dependency-output" | "tool-capsule" | "service" | "provider" | "bridge"
        | "bridge-capability" | "runtime-instance" | "process" => DriftClass::ResolvedDrift,
        // entrypoint | network | state | env | filesystem | source | unknown
        _ => DriftClass::DeclaredDrift,
    }
}

/// Map a graph edge kind label to its drift domain, or `None` to skip it.
///
/// `observes` edges are the per-edge runtime-observation seam (#495) and are
/// skipped here — envelope-level observed drift (#496) is handled separately via
/// the observed facet fields, not per-edge lifecycle. Materialization / provider
/// / service-graph edges are [`DriftClass::ResolvedDrift`]; declared structural
/// edges (and unknown kinds, conservatively) are [`DriftClass::DeclaredDrift`].
fn edge_kind_class(kind: &str) -> Option<DriftClass> {
    match kind {
        "observes" => None,
        "materializes-to" | "provides" | "connects-to" | "starts-before" => {
            Some(DriftClass::ResolvedDrift)
        }
        // depends-on | requires | grants | mounts | injects | unknown
        _ => Some(DriftClass::DeclaredDrift),
    }
}

/// Serialize a value for comparison/display, falling back to `null` on the
/// (practically unreachable) serialization failure rather than panicking.
fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Reduce a [`Tracked<String>`] to a comparable, display-friendly value: the
/// inner string when `Known`, otherwise a `{status, reason}` object so a
/// status transition (e.g. `Known -> Unknown`) is still detected as drift.
fn tracked_value(tracked: &Tracked<String>) -> Value {
    if tracked.status == TrackingStatus::Known {
        if let Some(value) = &tracked.value {
            return Value::String(value.clone());
        }
    }
    let mut map = serde_json::Map::new();
    map.insert("status".to_string(), to_value(&tracked.status));
    if let Some(reason) = &tracked.reason {
        map.insert("reason".to_string(), Value::String(reason.clone()));
    }
    Value::Object(map)
}

fn argv_value(args: &[LaunchArg]) -> Value {
    Value::Array(
        args.iter()
            .map(|arg| tracked_value(&arg.value_hash))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    // Pulls in the receipt schema types plus the re-exported drift API.
    use super::super::*;

    fn doc(receipt: ExecutionReceiptV2) -> ExecutionReceiptDocument {
        ExecutionReceiptDocument::V2(receipt)
    }

    /// A graph-backed v2 receipt: declared/resolved facets all `Known`, plus a
    /// three-node / one-edge launch graph projection.
    fn base_v2() -> ExecutionReceiptV2 {
        let input = ExecutionIdentityInputV2::new(
            SourceIdentityV2 {
                source_tree_hash: Tracked::known("blake3:source".to_string()),
                manifest_path_role: Tracked::known("workspace:capsule.toml".to_string()),
            },
            SourceProvenance {
                kind: SourceProvenanceKind::Local,
                git_remote: None,
                git_commit: None,
                registry_ref: None,
            },
            DependencyIdentityV2 {
                derivation_hash: Tracked::known("blake3:deriv".to_string()),
                output_hash: Tracked::known("blake3:depout".to_string()),
                derivation_inputs: None,
            },
            RuntimeIdentityV2 {
                declared: Some("python@3".to_string()),
                resolved_ref: Tracked::known("python@3.12.1".to_string()),
                binary_hash: Tracked::known("sha256:uvbinary".to_string()),
                dynamic_linkage: Tracked::known("blake3:dyn".to_string()),
                completeness: RuntimeCompleteness::BinaryWithDynamicClosure,
                platform: PlatformIdentity {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentityV2 {
                entries: vec![EnvironmentEntry {
                    key: "CONFIG".to_string(),
                    value_hash: Tracked::known("blake3:config".to_string()),
                    normalization: ValueNormalizationStatus::NoHostPath,
                    origin: EnvOrigin::ManifestStatic,
                }],
                fd_layout: Tracked::known(FdLayoutIdentity {
                    stdin: "inherited".to_string(),
                    stdout: "inherited".to_string(),
                    stderr: "inherited".to_string(),
                }),
                umask: Tracked::known("022".to_string()),
                ulimits: Tracked::known(UlimitIdentity {
                    limits: BTreeMap::new(),
                }),
                mode: EnvironmentMode::Closed,
                ambient_untracked_keys: Vec::new(),
            },
            FilesystemIdentityV2 {
                view_hash: Tracked::known("blake3:fs".to_string()),
                partial_view_hash: None,
                source_root: Tracked::known("workspace:.".to_string()),
                working_directory: Tracked::known("workspace:.".to_string()),
                readonly_layers: Vec::new(),
                writable_dirs: Vec::new(),
                persistent_state: Vec::new(),
                semantics: FilesystemSemantics {
                    case_sensitivity: Tracked::known(CaseSensitivity::Sensitive),
                    symlink_policy: Tracked::known(SymlinkPolicy::Preserve),
                    tmp_policy: Tracked::known(TmpPolicy::SessionLocal),
                },
            },
            PolicyIdentityV2 {
                network_policy_hash: Tracked::known("blake3:network".to_string()),
                capability_policy_hash: Tracked::known("blake3:capability".to_string()),
                sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
            },
            LaunchIdentityV2 {
                entry_point: LaunchEntryPoint::Command {
                    name: "python".to_string(),
                },
                argv: vec![LaunchArg {
                    value_hash: Tracked::known("blake3:argv-app".to_string()),
                    normalization: ValueNormalizationStatus::NoHostPath,
                }],
                working_directory: Tracked::known("workspace:.".to_string()),
            },
            None,
            ReproducibilityIdentity {
                class: ReproducibilityClass::Pure,
                causes: Vec::new(),
            },
        );
        let mut receipt = ExecutionReceiptV2::from_input(input, "2026-06-05T00:00:00Z".to_string())
            .expect("receipt");
        receipt.node_receipts = vec![
            NodeReceipt {
                node_identifier: "entrypoint:main".to_string(),
                kind: "entrypoint".to_string(),
                status: None,
            },
            NodeReceipt {
                node_identifier: "runtime:uv".to_string(),
                kind: "runtime".to_string(),
                status: None,
            },
            NodeReceipt {
                node_identifier: "dep:db".to_string(),
                kind: "dependency-output".to_string(),
                status: None,
            },
        ];
        receipt.edge_receipts = vec![EdgeReceipt {
            source: "entrypoint:main".to_string(),
            target: "runtime:uv".to_string(),
            kind: "depends-on".to_string(),
            status: None,
        }];
        receipt
    }

    /// A flat v1 receipt with no graph projection.
    fn base_v1() -> ExecutionReceipt {
        let input = ExecutionIdentityInput::new(
            SourceIdentity {
                source_ref: Tracked::known("local:/app".to_string()),
                source_tree_hash: Tracked::known("blake3:source".to_string()),
            },
            DependencyIdentity {
                derivation_hash: Tracked::known("blake3:deriv".to_string()),
                output_hash: Tracked::known("blake3:depout".to_string()),
            },
            RuntimeIdentity {
                declared: Some("node@20".to_string()),
                resolved: Some("node@20.10.0".to_string()),
                binary_hash: Tracked::known("blake3:runtime".to_string()),
                dynamic_linkage: Tracked::known("blake3:dyn".to_string()),
                platform: PlatformIdentity {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentity {
                closure_hash: Tracked::known("blake3:env".to_string()),
                mode: EnvironmentMode::Closed,
                tracked_keys: vec!["PATH".to_string()],
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
                working_directory: "/app".to_string(),
            },
            ReproducibilityIdentity {
                class: ReproducibilityClass::Pure,
                causes: Vec::new(),
            },
        );
        ExecutionReceipt::from_input(input, "2026-06-05T00:00:00Z".to_string()).expect("receipt")
    }

    #[test]
    fn identical_receipts_have_no_drift() {
        let report = diff_receipt_documents(&doc(base_v2()), &doc(base_v2())).expect("report");
        assert!(!report.has_drift);
        assert!(report.changes.is_empty());
    }

    #[test]
    fn entrypoint_change_is_declared_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.launch.entry_point = LaunchEntryPoint::Command {
            name: "uvicorn".to_string(),
        };
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        assert!(report.has_drift);
        let change = report
            .changes
            .iter()
            .find(|c| c.field == "launch.entry_point")
            .expect("entry_point change");
        assert_eq!(change.class, DriftClass::DeclaredDrift);
        assert_eq!(change.component_kind, "receipt_field");
        assert_eq!(change.component_id, "entrypoint");
    }

    #[test]
    fn argv_change_is_declared_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.launch.argv = vec![LaunchArg {
            value_hash: Tracked::known("blake3:argv-asgi".to_string()),
            normalization: ValueNormalizationStatus::NoHostPath,
        }];
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let change = report
            .changes
            .iter()
            .find(|c| c.field == "launch.argv")
            .expect("argv change");
        assert_eq!(change.class, DriftClass::DeclaredDrift);
    }

    #[test]
    fn runtime_version_and_hash_changes_are_resolved_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.runtime.resolved_ref = Tracked::known("python@3.12.2".to_string());
        new.runtime.binary_hash = Tracked::known("sha256:uvbinary2".to_string());
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let fields: Vec<&str> = report.changes.iter().map(|c| c.field.as_str()).collect();
        assert!(fields.contains(&"runtime.resolved_ref"));
        assert!(fields.contains(&"runtime.binary_hash"));
        for change in &report.changes {
            assert_eq!(
                change.class,
                DriftClass::ResolvedDrift,
                "field {} should be resolved drift",
                change.field
            );
            assert_eq!(change.component_id, "runtime");
        }
    }

    #[test]
    fn network_policy_change_is_declared_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.policy.network_policy_hash = Tracked::known("blake3:network2".to_string());
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let change = report
            .changes
            .iter()
            .find(|c| c.field == "policy.network_policy_hash")
            .expect("network policy change");
        assert_eq!(change.class, DriftClass::DeclaredDrift);
        assert_eq!(change.component_id, "network");
    }

    #[test]
    fn dependency_output_change_is_resolved_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.dependencies.output_hash = Tracked::known("blake3:depout2".to_string());
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let change = report
            .changes
            .iter()
            .find(|c| c.field == "dependencies.output_hash")
            .expect("dependency output change");
        assert_eq!(change.class, DriftClass::ResolvedDrift);
        assert_eq!(change.component_id, "dependency-output");
    }

    #[test]
    fn added_and_removed_nodes_report_component_id() {
        let old = base_v2();
        let mut new = base_v2();
        new.node_receipts = vec![
            NodeReceipt {
                node_identifier: "entrypoint:main".to_string(),
                kind: "entrypoint".to_string(),
                status: None,
            },
            NodeReceipt {
                node_identifier: "runtime:uv".to_string(),
                kind: "runtime".to_string(),
                status: None,
            },
            NodeReceipt {
                node_identifier: "service:web".to_string(),
                kind: "service".to_string(),
                status: None,
            },
        ];
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let removed = report
            .changes
            .iter()
            .find(|c| c.component_kind == "node" && c.component_id == "dep:db")
            .expect("removed node");
        assert_eq!(removed.new, None);
        assert!(removed.reason.contains("removed"));
        let added = report
            .changes
            .iter()
            .find(|c| c.component_kind == "node" && c.component_id == "service:web")
            .expect("added node");
        assert_eq!(added.old, None);
        assert_eq!(added.class, DriftClass::ResolvedDrift);
    }

    #[test]
    fn node_kind_change_reports_old_and_new_kind() {
        let old = base_v2();
        let mut new = base_v2();
        new.node_receipts[1].kind = "tool-capsule".to_string();
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let change = report
            .changes
            .iter()
            .find(|c| c.component_kind == "node" && c.component_id == "runtime:uv")
            .expect("node kind change");
        assert_eq!(change.field, "kind");
        assert_eq!(
            change.old,
            Some(serde_json::Value::String("runtime".to_string()))
        );
        assert_eq!(
            change.new,
            Some(serde_json::Value::String("tool-capsule".to_string()))
        );
    }

    #[test]
    fn added_and_removed_edges_report_component_id() {
        let old = base_v2();
        let mut new = base_v2();
        new.edge_receipts = vec![EdgeReceipt {
            source: "runtime:uv".to_string(),
            target: "dep:db".to_string(),
            kind: "depends-on".to_string(),
            status: None,
        }];
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let removed = report
            .changes
            .iter()
            .find(|c| {
                c.component_kind == "edge" && c.component_id == "entrypoint:main -> runtime:uv"
            })
            .expect("removed edge");
        assert!(removed.reason.contains("removed"));
        let added = report
            .changes
            .iter()
            .find(|c| c.component_kind == "edge" && c.component_id == "runtime:uv -> dep:db")
            .expect("added edge");
        assert!(added.reason.contains("added"));
    }

    #[test]
    fn edge_kind_change_reports_single_change() {
        let old = base_v2();
        let mut new = base_v2();
        new.edge_receipts = vec![EdgeReceipt {
            source: "entrypoint:main".to_string(),
            target: "runtime:uv".to_string(),
            kind: "materializes-to".to_string(),
            status: None,
        }];
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let edge_changes: Vec<_> = report
            .changes
            .iter()
            .filter(|c| c.component_kind == "edge")
            .collect();
        assert_eq!(edge_changes.len(), 1);
        let change = edge_changes[0];
        assert_eq!(change.field, "kind");
        assert_eq!(change.component_id, "entrypoint:main -> runtime:uv");
        assert_eq!(change.class, DriftClass::ResolvedDrift);
        assert!(change.reason.contains("kind changed"));
    }

    #[test]
    fn output_is_component_level_not_only_execution_id() {
        let old = base_v2();
        let mut new = base_v2();
        new.execution_id = "blake3:changed".to_string();
        new.launch.entry_point = LaunchEntryPoint::Command {
            name: "uvicorn".to_string(),
        };
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        assert!(report.has_drift);
        assert!(
            report
                .changes
                .iter()
                .any(|c| c.component_kind == "receipt_field" && c.field == "launch.entry_point"),
            "drift must localize a component-level change"
        );
        assert!(
            report.changes.iter().all(|c| c.field != "execution_id"),
            "must not degrade to an execution_id-only change when a component change exists"
        );
    }

    #[test]
    fn declared_and_resolved_execution_ids_are_classified() {
        let mut old = base_v2();
        old.declared_execution_id = Some("blake3:dec1".to_string());
        old.resolved_execution_id = Some("blake3:res1".to_string());
        let mut new = base_v2();
        new.declared_execution_id = Some("blake3:dec2".to_string());
        new.resolved_execution_id = Some("blake3:res2".to_string());
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let declared = report
            .changes
            .iter()
            .find(|c| c.field == "declared_execution_id")
            .expect("declared id change");
        assert_eq!(declared.class, DriftClass::DeclaredDrift);
        let resolved = report
            .changes
            .iter()
            .find(|c| c.field == "resolved_execution_id")
            .expect("resolved id change");
        assert_eq!(resolved.class, DriftClass::ResolvedDrift);
    }

    /// #496: a changed `observed_execution_id` between two runtime-observed
    /// receipts is classified as `ObservedDrift`.
    #[test]
    fn observed_execution_id_change_is_observed_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.observed_execution_id = Some("sha256:observed-2".to_string());
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let change = report
            .changes
            .iter()
            .find(|c| c.field == "observed_execution_id")
            .expect("observed id change");
        assert_eq!(change.class, DriftClass::ObservedDrift);
    }

    /// #496 still does NOT compare per-node/edge lifecycle `status` (#495/#521/
    /// #522) — only the envelope. A status-only change yields no drift.
    #[test]
    fn per_node_lifecycle_status_is_not_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.node_receipts[0].status = Some("running".to_string());
        new.edge_receipts[0].status = Some("active".to_string());
        // A real declared change so the report is non-empty (proves `status`
        // alone is not what produced it).
        new.launch.entry_point = LaunchEntryPoint::Command {
            name: "uvicorn".to_string(),
        };
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        assert!(
            report.changes.iter().all(|c| c.field != "status"),
            "per-node/edge lifecycle status must not be compared yet"
        );
        assert!(
            report
                .changes
                .iter()
                .any(|c| c.field == "launch.entry_point"),
            "the declared change must still be reported"
        );
    }

    /// #496: an observed *envelope* fact change is `ObservedDrift`, but a change
    /// to diagnostic-only facts (bound port / local URL) is NOT drift.
    #[test]
    fn observed_envelope_drifts_but_diagnostic_facts_do_not() {
        use super::super::{ObservedLaunchEnvelope, ObservedRuntimeEvidence};
        let evidence = |kind: &str, port: u16| ObservedRuntimeEvidence {
            envelope: ObservedLaunchEnvelope {
                runtime_kind: kind.to_string(),
                entrypoint: vec!["node".to_string(), "server.js".to_string()],
                env_keys: vec!["PORT".to_string()],
                ..Default::default()
            },
            bound_port: Some(port),
            local_url: Some(format!("http://127.0.0.1:{port}/")),
        };
        let mut old = base_v2();
        old.observed_runtime = Some(evidence("source/node", 18890));

        // (a) Same envelope, different bound port / local URL → no observed drift.
        let mut same = base_v2();
        same.observed_runtime = Some(evidence("source/node", 40000));
        let report = diff_receipt_documents(&doc(old.clone()), &doc(same)).expect("report");
        assert!(
            report
                .changes
                .iter()
                .all(|c| c.class != DriftClass::ObservedDrift),
            "diagnostic bound port / local URL must not produce observed drift"
        );

        // (b) A real envelope fact change (runtime_kind) → ObservedDrift.
        let mut changed = base_v2();
        changed.observed_runtime = Some(evidence("source/python", 18890));
        let report = diff_receipt_documents(&doc(old), &doc(changed)).expect("report");
        let drift = report
            .changes
            .iter()
            .find(|c| c.field == "observed.runtime_kind")
            .expect("runtime_kind drift");
        assert_eq!(drift.class, DriftClass::ObservedDrift);
    }

    /// Diagnostic / non-identity receipt fields (timestamp, host fingerprint,
    /// runner) are not compared, so changing them produces no drift.
    #[test]
    fn diagnostic_and_timestamp_fields_do_not_drift() {
        let old = base_v2();
        let mut new = base_v2();
        new.computed_at = "2099-12-31T23:59:59Z".to_string();
        new.host_fingerprint = Some("linux:arm64:musl".to_string());
        new.runner = None;
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        assert!(
            !report.has_drift,
            "diagnostic/timestamp fields must not drift: {:?}",
            report.changes
        );
    }

    /// An older / sparser receipt that lacks graph + observed fields must not
    /// panic — it compares on whatever evidence aligns.
    #[test]
    fn missing_graph_and_observed_fields_do_not_panic() {
        let mut old = base_v2();
        old.node_receipts.clear();
        old.edge_receipts.clear();
        old.observed_runtime = None;
        old.observed_execution_id = None;
        old.declared_execution_id = None;
        old.resolved_execution_id = None;
        let new = base_v2();
        // Must not panic; a valid report is produced from the aligned facets.
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let _ = report.has_drift;
    }

    #[test]
    fn report_serializes_to_machine_readable_json() {
        let old = base_v2();
        let mut new = base_v2();
        new.runtime.binary_hash = Tracked::known("sha256:uvbinary2".to_string());
        let report = diff_receipt_documents(&doc(old), &doc(new)).expect("report");
        let json = serde_json::to_string(&report).expect("json");
        assert!(json.contains("resolved-drift"));
        assert!(json.contains("runtime.binary_hash"));
    }

    #[test]
    fn v1_receipts_diff_on_shared_facets() {
        let old = base_v1();
        let mut new = base_v1();
        new.policy.network_policy_hash = Tracked::known("blake3:network2".to_string());
        new.runtime.binary_hash = Tracked::known("blake3:runtime2".to_string());
        let report = diff_receipt_documents(
            &ExecutionReceiptDocument::V1(old),
            &ExecutionReceiptDocument::V1(new),
        )
        .expect("report");
        let network = report
            .changes
            .iter()
            .find(|c| c.field == "policy.network_policy_hash")
            .expect("network change");
        assert_eq!(network.class, DriftClass::DeclaredDrift);
        let runtime = report
            .changes
            .iter()
            .find(|c| c.field == "runtime.binary_hash")
            .expect("runtime change");
        assert_eq!(runtime.class, DriftClass::ResolvedDrift);
    }
}
