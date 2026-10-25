//! Immutable evidence of successful Source exploration, never a Capsule identity.
use crate::{
    exploration::ExplorationSubmission,
    retained::{RetainedCandidateV1, RetainedError, content_ref},
    search::SearchStateV1,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SOURCE_RESULT_SCHEMA: &str = "ato.formation-source-result/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceResultV1 {
    pub schema: String,
    pub owner_scope: String,
    pub search_id: String,
    pub attempt_id: String,
    pub source_closure_ref: String,
    pub contract_ref: String,
    pub derivation_ref: String,
    pub retained_ref: String,
    pub runtime_id: String,
    pub environment_id: String,
    pub capability_profile_ref: String,
    pub target_triple: String,
}

impl SourceResultV1 {
    /// API supplies the authenticated durable attempt's saved profile, not the
    /// Runtime's current advertisement or any producer-authored target.
    pub fn project(
        state: &SearchStateV1,
        submission: &ExplorationSubmission,
        retained_ref: &str,
        descriptor_json: &str,
        capability_profile_ref: &str,
        facts: &BTreeMap<String, String>,
    ) -> Result<Self, RetainedError> {
        state
            .validate()
            .map_err(|_| RetainedError("source_result_search_invalid"))?;
        submission
            .validate(state)
            .map_err(|_| RetainedError("source_result_submission_invalid"))?;
        let descriptor = RetainedCandidateV1::parse(descriptor_json.as_bytes(), retained_ref)?;
        descriptor.match_assignment(&state.frozen.contract_ref, &submission.derivation_ref)?;
        let source = state
            .frozen
            .initial_source
            .as_ref()
            .ok_or(RetainedError("source_result_source_required"))?;
        if descriptor.source_closure_ref != source.closure_ref
            || descriptor.creation_attempt_id != submission.attempt_id
            || descriptor.derivation != submission.derivation
            || descriptor.base_contract != submission.contract
        {
            return Err(RetainedError("source_result_assignment_mismatch"));
        }
        let attempt = state
            .attempts
            .iter()
            .find(|a| a.attempt_id == submission.attempt_id)
            .ok_or(RetainedError("source_result_attempt_required"))?;
        if attempt.retained_ref.as_deref() != Some(retained_ref) || !attempt.route_accepted {
            return Err(RetainedError("source_result_retention_unconfirmed"));
        }
        if facts.is_empty()
            || facts.len() > 128
            || facts
                .iter()
                .any(|(k, v)| k.is_empty() || k.len() > 128 || v.len() > 256)
            || content_ref(
                &serde_jcs::to_vec(facts)
                    .map_err(|_| RetainedError("source_result_profile_invalid"))?,
            ) != capability_profile_ref
        {
            return Err(RetainedError("source_result_profile_invalid"));
        }
        let target = match (
            facts.get("platform.os").map(String::as_str),
            facts.get("platform.arch").map(String::as_str),
        ) {
            (Some("linux"), Some("x86_64")) => "x86_64-unknown-linux-gnu",
            (Some("linux"), Some("aarch64")) => "aarch64-unknown-linux-gnu",
            (Some("macos"), Some("x86_64")) => "x86_64-apple-darwin",
            (Some("macos"), Some("aarch64")) => "aarch64-apple-darwin",
            (Some("windows"), Some("x86_64")) => "x86_64-pc-windows-msvc",
            _ => return Err(RetainedError("source_result_target_unsupported")),
        };
        Ok(Self {
            schema: SOURCE_RESULT_SCHEMA.into(),
            owner_scope: state.owner_scope.clone(),
            search_id: state.search_id.clone(),
            attempt_id: attempt.attempt_id.clone(),
            source_closure_ref: source.closure_ref.clone(),
            contract_ref: state.frozen.contract_ref.clone(),
            derivation_ref: submission.derivation_ref.clone(),
            retained_ref: retained_ref.into(),
            runtime_id: attempt.runtime_id.clone(),
            environment_id: attempt.environment_id.clone(),
            capability_profile_ref: capability_profile_ref.into(),
            target_triple: target.into(),
        })
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RetainedError> {
        serde_jcs::to_vec(self).map_err(|_| RetainedError("source_result_encoding_failed"))
    }
    pub fn result_ref(&self) -> Result<String, RetainedError> {
        Ok(content_ref(&self.canonical_bytes()?))
    }
}
