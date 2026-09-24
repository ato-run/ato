//! A Formation request and its result — the Phase 1 (local Formation) seam.
//!
//! The model is "I -- D on R --> C, C |= K": a request names the Initial
//! Condition, where the Contract comes from, which Runtime may be used, and a
//! budget. The result is either a Contract the candidate was OBSERVED
//! satisfying, or the evidence of every attempt that did not get there.
//!
//! What is deliberately not here: candidate generation (presets live in
//! 'preset', authoring in 'capsule_toml'), execution (the worker's), and any
//! runtime model richer than a flat fact map.
//!
//! These types are the seam Phase 1 needs, not a stable contract. Contract
//! normalization from a prompt, other verifiers and a Runtime Network may
//! extend or replace them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::browser::{BrowserContractV0, BrowserVerificationReceipt};
use crate::verify::{ContractVerification, ContractVerificationReceipt};

/// What the Formation starts from. 'I' in the model.
///
/// A local directory is the only kind Phase 1 needs; a content-addressed
/// closure or a checkpoint are the same slot, later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialCondition {
    LocalDirectory {
        path: PathBuf,
    },
    /// A source archive that was already snapshotted elsewhere — a Runtime
    /// Network attempt is handed the same frozen bytes the requester
    /// measured, never a live directory. `expected_digest` is the archive
    /// digest the ticket names; the bytes are refused if they differ.
    Archive {
        bytes: Vec<u8>,
        expected_digest: String,
    },
}

/// Where the Contract comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractSource {
    /// An authored 'capsule.toml', verbatim. Parsed strictly and never
    /// substituted by a guess.
    Authored { toml: String },
    /// No authored document: derive candidates from detection evidence.
    Infer,
}

/// Which Runtime may execute the Derivation.
///
/// Phase 1 admits exactly one value — 'Exact { "local" }' — which is the
/// whole point of the variant: the constraint is part of the request, so a
/// failed local attempt produces evidence, never a fallback to somewhere the
/// requester did not name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeConstraint {
    Exact { runtime_id: String },
}

/// Whether a build step may reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormationNetworkPolicy {
    Denied,
    DependencyResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormationPolicy {
    pub network: FormationNetworkPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    /// Candidate Derivations to try before giving up.
    pub max_attempts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormationRequest {
    pub initial_condition: InitialCondition,
    pub contract: ContractSource,
    pub runtime: RuntimeConstraint,
    pub policy: FormationPolicy,
    pub budget: SearchBudget,
    /// An acceptance prompt to verify in a browser, on top of the Contract's
    /// typed observations. Opt-in: `None` leaves Formation exactly as it is.
    pub browser_contract: Option<BrowserContractV0>,
}

/// The facts a Runtime reports about itself, as a flat map.
///
/// Phase 1 admission reads only 'platform.os', 'platform.arch' and
/// 'formation.containment'. Deliberately not a facts framework.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeProfile {
    pub runtime_id: String,
    pub capabilities: BTreeMap<String, String>,
}

impl RuntimeProfile {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.capabilities.get(key).map(String::as_str)
    }
}

/// Why an attempt did not produce a verified route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptFailure {
    pub code: String,
    pub stage: String,
    /// Written for the person who asked, not copied from a log.
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    /// Every Contract observation was decided Satisfied.
    Verified,
    /// The candidate ran and did not satisfy the Contract, or could not run.
    Failed,
    /// Never executed: the Runtime or policy ruled it out beforehand.
    Filtered,
}

/// One tried candidate, with whatever it proved.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FormationAttempt {
    /// Which candidate this was: '"authored"' or a preset id.
    pub candidate: String,
    /// The attempt this evidence belongs to. Absent only for a candidate
    /// that was never planned (detection found nothing to try).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derivation_ref: Option<String>,
    /// The K this attempt verified: the base Contract, or — when a browser
    /// Contract is part of the request — the effective Contract naming both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_ref: Option<String>,
    /// The base Contract's own ref, kept for provenance when `contract_ref`
    /// is an effective Contract. Absent when the two are the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_contract_ref: Option<String>,
    pub runtime_id: String,
    pub status: AttemptStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<ContractVerification>,
    /// How the candidate was run to be observed, when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realization: Option<RealizationEvidence>,
    /// The browser verification of the request's acceptance prompt, when one
    /// was asked for and the candidate got that far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_verification: Option<BrowserVerificationReceipt>,
    /// The receipt for this attempt's verification point, when the candidate
    /// was observed. Immutable: nothing after it — keeping the artifact,
    /// stopping the candidate — changes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ContractVerificationReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<AttemptFailure>,
}

/// The conditions a candidate was observed under.
///
/// Recorded so "verified" always says verified WHERE: which executor, what
/// containment, what network, and that the realization was taken down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RealizationEvidence {
    /// Which execution machinery ran the candidate.
    pub executor: String,
    pub containment: String,
    /// What the candidate's filesystem was, and what happened to its writes.
    pub workspace: String,
    /// What the build was allowed.
    pub build_network: String,
    /// What the running candidate was allowed — stated separately because it
    /// is not the same thing.
    pub candidate_network: String,
    /// Logical port id to where it was realized.
    pub endpoints: BTreeMap<String, String>,
    /// The realization was stopped and its scratch removed.
    pub destroyed: bool,
}

/// A route that was observed satisfying the Contract on a concrete Runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedRoute {
    /// The attempt whose receipt this route is derived from.
    pub attempt_id: String,
    pub derivation_ref: String,
    pub runtime_id: String,
    pub materialization_ref: String,
}

/// What a Formation run produced.
///
/// An enum rather than a struct with an optional 'contract_ref': 'Formed'
/// without a Contract identity is a state that must not be representable.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FormationResult {
    /// At least one 'D x R' was observed satisfying one canonical Contract.
    Formed {
        contract_ref: String,
        verified_routes: Vec<VerifiedRoute>,
        attempts: Vec<FormationAttempt>,
    },
    /// No candidate got there. The attempts are the evidence.
    NoVerifiedRoute {
        attempted_contract_refs: Vec<String>,
        attempts: Vec<FormationAttempt>,
    },
}
