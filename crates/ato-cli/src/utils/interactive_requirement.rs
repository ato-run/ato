//! Stable, machine-readable surface for "the CLI needs interactive
//! resolution but cannot prompt here" cases.
//!
//! Today there is exactly one such case: E302 with
//! `details.reason == "execution_plan_consent_required"`, emitted from
//! `application::auth::consent_store::require_consent` when stdin/stdout
//! are not a TTY.
//!
//! When the human-readable CLI error is rendered (the non-`--json`
//! path), a script / AODD harness / MCP host caller has no way to lift
//! the five identity fields it needs to call
//! `ato internal consent approve-execution-plan`. This module emits a
//! single deterministic line on stderr so non-TTY callers can scrape
//! the envelope without parsing miette output:
//!
//! ```text
//! ATO_INTERACTIVE_REQUIREMENT: {"reason":"execution_plan_consent_required",...}
//! ```
//!
//! The `ATO_INTERACTIVE_REQUIREMENT:` prefix is the contract — keep it
//! exactly. The desktop stdio bridge (which already gets the typed
//! envelope through `--json` / its own IPC path) is unaffected; this
//! emission is additive on the human-readable path.

use std::io::IsTerminal;

use anyhow::Error as AnyhowError;
use capsule_core::execution_plan::error::AtoExecutionError;
use serde::Serialize;

use crate::application::pipeline::cleanup::PipelineAttemptError;

/// Stable contract: every `InteractiveRequirementEnvelope` line on
/// stderr starts with this exact prefix so non-TTY callers can scrape
/// without parsing miette output.
const ATO_INTERACTIVE_REQUIREMENT_PREFIX: &str = "ATO_INTERACTIVE_REQUIREMENT:";

/// Mirrors the `details.reason` value already shipped in the typed
/// `AtoError::ExecutionPlanConsentRequired` envelope.
const REASON_EXECUTION_PLAN_CONSENT_REQUIRED: &str = "execution_plan_consent_required";

/// Wire shape of the JSON line emitted after the
/// `ATO_INTERACTIVE_REQUIREMENT:` prefix. Field order is fixed by the
/// declaration order so `serde_json::to_string` produces a stable
/// payload across releases.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct InteractiveRequirementEnvelope<'a> {
    reason: &'a str,
    scoped_id: &'a str,
    version: &'a str,
    target_label: &'a str,
    policy_segment_hash: &'a str,
    provisioning_policy_hash: &'a str,
    summary: &'a str,
}

/// If `err` carries a non-TTY E302 consent-required envelope and stderr
/// is not a TTY, write the `ATO_INTERACTIVE_REQUIREMENT: <json>` line to
/// stderr. Returns `true` when the line was emitted.
pub(crate) fn try_emit_for_anyhow(err: &AnyhowError) -> bool {
    let Some(execution_err) = downcast_execution_error(err) else {
        return false;
    };
    try_emit_for_execution_error(execution_err)
}

/// Same as [`try_emit_for_anyhow`] but takes the typed error directly.
pub(crate) fn try_emit_for_execution_error(execution_err: &AtoExecutionError) -> bool {
    if std::io::stderr().is_terminal() {
        return false;
    }
    let Some(envelope) = consent_envelope_from_execution_error(execution_err) else {
        return false;
    };
    emit_envelope(&envelope);
    true
}

fn downcast_execution_error(err: &AnyhowError) -> Option<&AtoExecutionError> {
    if let Some(attempt_err) = err.downcast_ref::<PipelineAttemptError>() {
        if let Some(execution_err) = attempt_err
            .source_error()
            .downcast_ref::<AtoExecutionError>()
        {
            return Some(execution_err);
        }
    }
    err.downcast_ref::<AtoExecutionError>()
}

/// Decodes `details` and returns the envelope iff this is the consent-
/// required sub-shape of E302. Returns `None` for any other E302 (or
/// any other error code) so unrelated errors keep their existing path.
fn consent_envelope_from_execution_error(
    err: &AtoExecutionError,
) -> Option<InteractiveRequirementEnvelope<'_>> {
    let details = err.details.as_ref()?;
    let reason = details.get("reason").and_then(|v| v.as_str())?;
    if reason != REASON_EXECUTION_PLAN_CONSENT_REQUIRED {
        return None;
    }
    let scoped_id = details.get("scoped_id").and_then(|v| v.as_str())?;
    let version = details.get("version").and_then(|v| v.as_str())?;
    let target_label = details.get("target_label").and_then(|v| v.as_str())?;
    let policy_segment_hash = details
        .get("policy_segment_hash")
        .and_then(|v| v.as_str())?;
    let provisioning_policy_hash = details
        .get("provisioning_policy_hash")
        .and_then(|v| v.as_str())?;
    let summary = details.get("summary").and_then(|v| v.as_str())?;

    Some(InteractiveRequirementEnvelope {
        reason: REASON_EXECUTION_PLAN_CONSENT_REQUIRED,
        scoped_id,
        version,
        target_label,
        policy_segment_hash,
        provisioning_policy_hash,
        summary,
    })
}

fn emit_envelope(envelope: &InteractiveRequirementEnvelope<'_>) {
    if let Ok(payload) = serde_json::to_string(envelope) {
        eprintln!("{ATO_INTERACTIVE_REQUIREMENT_PREFIX} {payload}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn consent_required_execution_error() -> AtoExecutionError {
        AtoExecutionError::from_ato_error(capsule_core::AtoError::ExecutionPlanConsentRequired {
            message: "ExecutionPlan consent required for this capsule.".to_string(),
            hint: Some("approve via desktop or CLI".to_string()),
            scoped_id: "publisher/app".to_string(),
            version: "1.0.0".to_string(),
            target_label: "cli".to_string(),
            policy_segment_hash: "blake3:aaa".to_string(),
            provisioning_policy_hash: "blake3:bbb".to_string(),
            summary: "Capsule: publisher/app@1.0.0\nTarget: cli".to_string(),
        })
    }

    #[test]
    fn consent_envelope_extracts_seven_keys_from_details() {
        let err = consent_required_execution_error();
        let envelope = consent_envelope_from_execution_error(&err).expect("should match");
        assert_eq!(envelope.reason, REASON_EXECUTION_PLAN_CONSENT_REQUIRED);
        assert_eq!(envelope.scoped_id, "publisher/app");
        assert_eq!(envelope.version, "1.0.0");
        assert_eq!(envelope.target_label, "cli");
        assert_eq!(envelope.policy_segment_hash, "blake3:aaa");
        assert_eq!(envelope.provisioning_policy_hash, "blake3:bbb");
        assert_eq!(envelope.summary, "Capsule: publisher/app@1.0.0\nTarget: cli");
    }

    #[test]
    fn unrelated_e302_yields_no_envelope() {
        // Plain ExecutionContractInvalid (no `reason` discriminator) must
        // fall through unchanged so we don't accidentally route generic
        // E302s to the consent path.
        let err = AtoExecutionError::execution_contract_invalid(
            "some other contract failure".to_string(),
            None,
            None,
        );
        assert!(consent_envelope_from_execution_error(&err).is_none());
    }

    #[test]
    fn details_without_summary_yields_no_envelope() {
        // If any required key is missing we'd rather fall through than
        // emit a half-populated envelope.
        let mut err = consent_required_execution_error();
        if let Some(details) = err.details.as_mut() {
            if let Some(obj) = details.as_object_mut() {
                obj.remove("summary");
            }
        }
        assert!(consent_envelope_from_execution_error(&err).is_none());
    }

    #[test]
    fn json_field_order_is_stable() {
        // The wire contract is "fixed key order"; serde follows struct
        // declaration order, so this is really a guard against someone
        // re-ordering the struct.
        let envelope = InteractiveRequirementEnvelope {
            reason: REASON_EXECUTION_PLAN_CONSENT_REQUIRED,
            scoped_id: "publisher/app",
            version: "1.0.0",
            target_label: "cli",
            policy_segment_hash: "blake3:aaa",
            provisioning_policy_hash: "blake3:bbb",
            summary: "summary",
        };
        let rendered = serde_json::to_string(&envelope).expect("serialize");
        assert_eq!(
            rendered,
            r#"{"reason":"execution_plan_consent_required","scoped_id":"publisher/app","version":"1.0.0","target_label":"cli","policy_segment_hash":"blake3:aaa","provisioning_policy_hash":"blake3:bbb","summary":"summary"}"#
        );
    }

    #[test]
    fn anyhow_path_finds_consent_envelope_under_attempt_wrapper() {
        // PipelineAttemptError wraps the AtoExecutionError; the helper
        // must unwrap it so the run dispatcher's cleanup path doesn't
        // hide the envelope from non-TTY callers.
        use crate::application::pipeline::cleanup::{CleanupReport, PipelineAttemptError};
        use crate::application::pipeline::hourglass::HourglassPhase;
        use capsule_core::execution_plan::error::CleanupStatus;

        let inner: AnyhowError = consent_required_execution_error().into();
        let attempt = PipelineAttemptError::new(
            HourglassPhase::Execute,
            inner,
            CleanupReport {
                status: CleanupStatus::NotRequired,
                actions: Vec::new(),
            },
        );
        let wrapped: AnyhowError = attempt.into();
        let ext = downcast_execution_error(&wrapped).expect("execution err under wrapper");
        assert!(consent_envelope_from_execution_error(ext).is_some());
    }

    #[test]
    fn try_emit_returns_true_for_consent_envelope_on_non_tty() {
        // `cargo test` runs with stderr redirected to a pipe, so the
        // non-TTY branch is exercised here. The function returns true
        // when the line was emitted; we don't capture stderr in this
        // unit test (the dedicated `details_*` tests cover the JSON
        // shape), but this guards the seam between detection and
        // emission.
        let err = consent_required_execution_error();
        assert!(try_emit_for_execution_error(&err));
    }

    #[test]
    fn try_emit_returns_false_for_unrelated_e302() {
        let err = AtoExecutionError::execution_contract_invalid(
            "some other contract failure".to_string(),
            None,
            None,
        );
        assert!(!try_emit_for_execution_error(&err));
    }

    #[test]
    fn details_must_carry_reason_discriminator() {
        // A details blob without the `reason` field is always non-consent.
        let mut err = consent_required_execution_error();
        err.details = Some(json!({
            "scoped_id": "publisher/app",
            "version": "1.0.0",
            "target_label": "cli",
            "policy_segment_hash": "blake3:aaa",
            "provisioning_policy_hash": "blake3:bbb",
            "summary": "summary",
        }));
        assert!(consent_envelope_from_execution_error(&err).is_none());
    }
}
