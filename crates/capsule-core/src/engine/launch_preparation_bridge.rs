//! Launch-preparation **bridge contract** (#581 ↔ #593).
//!
//! [`super::launch_preparation::LaunchPreparationPlan`] is the rich, in-process
//! plan capsule-core produces. The control plane (ato-api) cannot import Rust,
//! and does not need the full plan — it needs a small, stable, JSON-shaped
//! result it can persist and reason about. This module defines that boundary:
//! [`LaunchPreparationBridgeResult`], a deliberately minimal projection of the
//! launch-preparation decision.
//!
//! # What crosses the boundary
//!
//! On `prepared`, only the flat identity refs the dispatch / API layer correlates
//! on, plus the reference-only `PrepareSession` command. The nested
//! `launch_template` and `materialization` records are **not** exported — they are
//! internal composition detail. On `not_prepared`, only a stable blocker `code`
//! (plus an optional human `detail`) — never the typed Rust error chain.
//!
//! # What must never cross the boundary
//!
//! No raw secret value, dynamic port, pid, container id, live route, log cursor,
//! readiness / observed status, or timestamp-as-identity. The source
//! [`LaunchPreparationPlan`] already excludes these (see the secrets / observed
//! diagnostics tests in [`super::launch_preparation`]); the bridge projection
//! only narrows the surface further, so the guarantee is preserved by
//! construction. Tests in this module re-assert it on the serialized JSON.
//!
//! The matching JSON Schema + prose contract live at
//! `docs/specs/launch-preparation-plan.schema.json` and
//! `docs/specs/launch-preparation-plan.md`; the golden fixtures consumed by both
//! this crate and ato-api live at
//! `crates/capsule-core/tests/fixtures/launch_preparation/`.

use serde::{Deserialize, Serialize};

use crate::foundation::install_lifecycle::launch_template::RunnerClass;

use super::launch_preparation::{
    LaunchPreparationBlocker, LaunchPreparationDecision, LaunchPreparationPlan,
};
use super::runner_command::RunnerCommandPayload;

/// The control-plane-facing projection of a launch-preparation decision.
///
/// Tagged on `status` so JSON consumers can discriminate without structural
/// guessing: `{"status":"prepared","plan":{…}}` or
/// `{"status":"not_prepared","blockers":[…]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LaunchPreparationBridgeResult {
    /// A launch can be prepared; `plan` carries the minimal identity surface.
    Prepared { plan: LaunchPreparationBridgePlan },
    /// A launch cannot be prepared; `blockers` lists stable codes.
    NotPrepared {
        blockers: Vec<LaunchPreparationBridgeBlocker>,
    },
}

/// The flat, reference-only plan projection that crosses the bridge.
///
/// Every field is a content hash, a control-plane reference, or the
/// reference-only `PrepareSession` command. The typed-newtype ids of the source
/// plan are flattened to `String` so the control plane has no dependency on
/// capsule-core's id types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPreparationBridgePlan {
    pub install_revision_id: String,
    pub capsule_instance_key: String,
    pub execution_id: String,
    /// Requirement graph **content** hash (`blake3:<hex>`) — never the snapshot.
    pub requirement_graph_hash: String,
    /// Requirement graph **snapshot** identity (`blake3:<hex>`) — kept distinct
    /// from the content hash (#588/#596).
    pub requirement_graph_snapshot_hash: String,
    /// The reused launch template's key hash.
    pub launch_template_key_hash: String,
    /// The concrete runner class (serialized snake_case, e.g. `managed_runner`).
    pub selected_runner_class: RunnerClass,
    /// A control-plane reference to the selected runner (never a pid / container).
    pub selected_runner_ref: String,
    /// Idempotency / correlation key for the later dispatch layer.
    pub command_request_id: String,
    /// The reference-only `PrepareSession` command (never dispatched here).
    pub prepare_command: RunnerCommandPayload,
}

/// A stable, control-plane-facing blocker code with an optional human detail.
///
/// `code` is from a closed, documented vocabulary (see [`bridge_blocker_code`]);
/// `detail` is a non-authoritative diagnostic string and may be `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPreparationBridgeBlocker {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Map a typed [`LaunchPreparationBlocker`] to its stable bridge code.
///
/// This is the canonical #581 → control-plane blocker vocabulary. ato-api maps
/// these conservatively onto its own Run blockers; see
/// `docs/specs/launch-preparation-plan.md`.
pub fn bridge_blocker_code(blocker: &LaunchPreparationBlocker) -> &'static str {
    match blocker {
        LaunchPreparationBlocker::ReusableInputsInvalid(_) => "reusable_inputs_invalid",
        LaunchPreparationBlocker::LaunchTemplateNotReusable(_) => "launch_template_not_reusable",
        LaunchPreparationBlocker::LaunchMaterializationFailed(_) => "launch_materialization_failed",
        LaunchPreparationBlocker::MaterializationPersistFailed { .. } => {
            "launch_materialization_failed"
        }
        LaunchPreparationBlocker::PrepareSessionCommandFailed(_) => {
            "prepare_session_command_failed"
        }
    }
}

impl LaunchPreparationBridgePlan {
    /// Project the rich plan into the flat bridge plan.
    pub fn from_plan(plan: &LaunchPreparationPlan) -> Self {
        Self {
            install_revision_id: plan.install_revision_id.as_str().to_owned(),
            capsule_instance_key: plan.capsule_instance_key.as_str().to_owned(),
            execution_id: plan.execution_id.as_str().to_owned(),
            requirement_graph_hash: plan.requirement_graph_hash.clone(),
            requirement_graph_snapshot_hash: plan.requirement_graph_snapshot_hash.clone(),
            launch_template_key_hash: plan.launch_template_key_hash.clone(),
            selected_runner_class: plan.selected_runner_class,
            selected_runner_ref: plan.selected_runner_ref.clone(),
            command_request_id: plan.command_request_id.clone(),
            prepare_command: plan.prepare_command.clone(),
        }
    }
}

impl LaunchPreparationBridgeBlocker {
    /// Build a bridge blocker from a typed blocker, carrying its stable code and
    /// its `Display` text as a non-authoritative detail.
    pub fn from_blocker(blocker: &LaunchPreparationBlocker) -> Self {
        Self {
            code: bridge_blocker_code(blocker).to_owned(),
            detail: Some(blocker.to_string()),
        }
    }
}

impl LaunchPreparationBridgeResult {
    /// Project a launch-preparation decision into the bridge result.
    pub fn from_decision(decision: &LaunchPreparationDecision) -> Self {
        match decision {
            LaunchPreparationDecision::Prepared(plan) => Self::Prepared {
                plan: LaunchPreparationBridgePlan::from_plan(plan),
            },
            LaunchPreparationDecision::NotPrepared { blockers } => Self::NotPrepared {
                blockers: blockers
                    .iter()
                    .map(LaunchPreparationBridgeBlocker::from_blocker)
                    .collect(),
            },
        }
    }

    /// True only for [`Self::Prepared`].
    pub fn is_prepared(&self) -> bool {
        matches!(self, Self::Prepared { .. })
    }
}

impl From<&LaunchPreparationDecision> for LaunchPreparationBridgeResult {
    fn from(decision: &LaunchPreparationDecision) -> Self {
        Self::from_decision(decision)
    }
}
