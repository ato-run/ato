use anyhow::Error as AnyhowError;
use serde::Serialize;
use serde_json::Value;

use capsule_core::execution_plan::error::{
    AtoExecutionError, CleanupActionRecord, CleanupStatus, ManifestSuggestion,
};
use capsule_core::interactive_resolution::InteractiveResolutionEnvelope;

use crate::application::pipeline::cleanup::PipelineAttemptError;

pub const EXIT_USER_ERROR: i32 = 1;
pub const EXIT_SYSTEM_ERROR: i32 = 2;
pub const EXIT_NETWORK_ERROR: i32 = 3;
pub const EXIT_RUNTIME_ERROR: i32 = 5;

pub const ATO_ERR_AUTH_REQUIRED: &str = "ATO_ERR_AUTH_REQUIRED";
pub const ATO_ERR_INTEGRITY_FAILURE: &str = "ATO_ERR_INTEGRITY_FAILURE";
pub const ATO_ERR_MISSING_MATERIALIZATION: &str = "ATO_ERR_MISSING_MATERIALIZATION";

#[derive(Debug, Serialize)]
struct AtoErrorEvent<'a> {
    level: &'static str,
    code: &'a str,
    name: &'a str,
    phase: &'a str,
    classification: &'a str,
    message: &'a str,
    retryable: bool,
    interactive_resolution: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a Value>,
    /// #96 — additive typed envelope alongside the legacy `details`. Only
    /// populated for variants with an interactive-resolution UI today
    /// (E103, E302). Skipped from output when None so callers reading
    /// the legacy shape see no new fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    interactive_resolution_required: Option<&'a InteractiveResolutionEnvelope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_status: Option<CleanupStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cleanup_actions: &'a Vec<CleanupActionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_suggestion: Option<&'a ManifestSuggestion>,
}

pub fn emit_ato_error_jsonl(err: &AtoExecutionError) {
    let payload = AtoErrorEvent {
        level: "fatal",
        code: err.code,
        name: err.name,
        phase: err.phase,
        classification: err.classification.as_str(),
        message: &err.message,
        retryable: err.retryable,
        interactive_resolution: err.interactive_resolution,
        resource: err.resource.as_deref(),
        target: err.target.as_deref(),
        hint: err.hint.as_deref(),
        details: err.details.as_ref(),
        interactive_resolution_required: err.interactive_resolution_required.as_ref(),
        cleanup_status: err.cleanup_status,
        cleanup_actions: &err.cleanup_actions,
        manifest_suggestion: err.manifest_suggestion.as_ref(),
    };

    if let Ok(line) = serde_json::to_string(&payload) {
        eprintln!("{}", line);
    }
}

pub fn try_emit_from_anyhow(err: &AnyhowError, json_mode: bool) -> bool {
    if !json_mode {
        return false;
    }

    if let Some(attempt_err) = err.downcast_ref::<PipelineAttemptError>() {
        if let Some(execution_err) = attempt_err
            .source_error()
            .downcast_ref::<AtoExecutionError>()
        {
            let enriched = execution_err.clone().with_cleanup(
                attempt_err.cleanup_report().status,
                attempt_err.cleanup_report().actions.clone(),
            );
            emit_ato_error_jsonl(&enriched);
            return true;
        }
    }

    if let Some(execution_err) = err.downcast_ref::<AtoExecutionError>() {
        emit_ato_error_jsonl(execution_err);
        return true;
    }

    legacy_message_fallback(err)
}

/// Emit the structured JSON envelope to stderr when the underlying error
/// reports `interactive_resolution = true` (today: missing-env, consent
/// required, auth required, ambiguous entrypoint, manual intervention).
///
/// This is the non-TTY companion to `try_emit_from_anyhow`. Unlike that
/// function, it does not require `--json` — the rationale is that any
/// caller running `ato run` against a non-interactive shell must have a
/// machine-readable way to see the typed identity fields (consent
/// scoped_id / target_label / policy hashes etc.), or they cannot
/// resolve the requirement at all.
///
/// Returns `true` if an envelope was emitted. Callers should still
/// continue to the human-readable diagnostic path so the user message
/// is not swallowed.
pub fn try_emit_interactive_resolution_envelope(err: &AnyhowError) -> bool {
    if let Some(attempt_err) = err.downcast_ref::<PipelineAttemptError>() {
        if let Some(execution_err) = attempt_err
            .source_error()
            .downcast_ref::<AtoExecutionError>()
        {
            if execution_err.interactive_resolution {
                let enriched = execution_err.clone().with_cleanup(
                    attempt_err.cleanup_report().status,
                    attempt_err.cleanup_report().actions.clone(),
                );
                emit_ato_error_jsonl(&enriched);
                return true;
            }
        }
    }

    if let Some(execution_err) = err.downcast_ref::<AtoExecutionError>() {
        if execution_err.interactive_resolution {
            emit_ato_error_jsonl(execution_err);
            return true;
        }
    }

    false
}

fn legacy_message_fallback(err: &AnyhowError) -> bool {
    let message = if let Some(attempt_err) = err.downcast_ref::<PipelineAttemptError>() {
        attempt_err.source_error().to_string()
    } else {
        err.to_string()
    };

    if message.contains("capsule.lock manifest hash mismatch") {
        emit_ato_error_jsonl(&AtoExecutionError::lockfile_tampered(message, None));
        return true;
    }

    if message.contains("deno.lock")
        && (message.contains("not found") || message.contains("missing"))
    {
        emit_ato_error_jsonl(&AtoExecutionError::lock_incomplete(
            message,
            Some("deno.lock"),
        ));
        return true;
    }

    if message.contains("runtime=oci") && message.contains("not supported") {
        emit_ato_error_jsonl(&AtoExecutionError::execution_contract_invalid(
            message, None, None,
        ));
        return true;
    }

    if message.contains("sandbox") && message.contains("not available") {
        emit_ato_error_jsonl(&AtoExecutionError::compat_hardware(message, None));
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::execution_plan::error::AtoExecutionError;
    use capsule_core::AtoError;

    /// `try_emit_from_anyhow` keeps its existing contract: gated on
    /// `--json`. Without it, no envelope is emitted (regardless of error
    /// kind). This protects existing callers (Desktop bridge etc.) from
    /// behaviour shifts.
    #[test]
    fn try_emit_from_anyhow_remains_gated_on_json_mode() {
        let err = anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::ExecutionPlanConsentRequired {
                message: "consent required".to_string(),
                hint: None,
                scoped_id: "publisher/app".to_string(),
                version: "1.0.0".to_string(),
                target_label: "cli".to_string(),
                policy_segment_hash: "blake3:aaa".to_string(),
                provisioning_policy_hash: "blake3:bbb".to_string(),
                summary: "Capsule: publisher/app@1.0.0".to_string(),
            },
        ));

        // Without --json, the legacy emit fn is a no-op for this error.
        assert!(!try_emit_from_anyhow(&err, /* json_mode */ false));
    }

    /// `try_emit_interactive_resolution_envelope` fires for any error
    /// reporting `interactive_resolution = true` regardless of `--json`.
    /// The envelope itself is verified by the consent-store integration
    /// test that scrapes stderr — here we just confirm the hot path
    /// returns true (i.e. an envelope was emitted).
    #[test]
    fn interactive_resolution_envelope_fires_for_consent_error() {
        let err = anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::ExecutionPlanConsentRequired {
                message: "consent required".to_string(),
                hint: None,
                scoped_id: "publisher/app".to_string(),
                version: "1.0.0".to_string(),
                target_label: "cli".to_string(),
                policy_segment_hash: "blake3:aaa".to_string(),
                provisioning_policy_hash: "blake3:bbb".to_string(),
                summary: "Capsule: publisher/app@1.0.0".to_string(),
            },
        ));

        assert!(try_emit_interactive_resolution_envelope(&err));
    }

    /// The envelope helper must NOT fire for non-interactive errors (e.g.
    /// internal/system errors). Otherwise scripted callers would receive
    /// JSON for everything, defeating the targeted nature of #126.
    #[test]
    fn interactive_resolution_envelope_skips_non_interactive_errors() {
        let err = anyhow::Error::new(AtoExecutionError::internal("boom".to_string()));

        assert!(!try_emit_interactive_resolution_envelope(&err));
    }

    /// Non-`AtoExecutionError` errors are passed through untouched. The
    /// envelope helper must not panic or false-positive on plain anyhow
    /// errors that propagate from callsites that haven't adopted the
    /// typed error model yet.
    #[test]
    fn interactive_resolution_envelope_passes_through_plain_anyhow() {
        let err = anyhow::anyhow!("plain string error");

        assert!(!try_emit_interactive_resolution_envelope(&err));
    }

    /// #96 — the on-the-wire JSONL must carry the new
    /// `interactive_resolution_required` envelope alongside the legacy
    /// `details` payload for E302. This locks the additive shape so a
    /// future regression that drops the field would fail loudly here.
    #[test]
    fn jsonl_event_carries_envelope_alongside_details_for_consent_error() {
        let exec_err = AtoExecutionError::from_ato_error(AtoError::ExecutionPlanConsentRequired {
            message: "consent required".to_string(),
            hint: Some("approve via desktop".to_string()),
            scoped_id: "publisher/app".to_string(),
            version: "1.0.0".to_string(),
            target_label: "cli".to_string(),
            policy_segment_hash: "blake3:aaa".to_string(),
            provisioning_policy_hash: "blake3:bbb".to_string(),
            summary: "Capsule: publisher/app@1.0.0".to_string(),
        });

        // The wire payload is the same shape `emit_ato_error_jsonl`
        // writes to stderr; serialize via the same struct rather than
        // capturing stderr to avoid coupling the test to test-runner
        // capture quirks.
        let payload = AtoErrorEvent {
            level: "fatal",
            code: exec_err.code,
            name: exec_err.name,
            phase: exec_err.phase,
            classification: exec_err.classification.as_str(),
            message: &exec_err.message,
            retryable: exec_err.retryable,
            interactive_resolution: exec_err.interactive_resolution,
            resource: exec_err.resource.as_deref(),
            target: exec_err.target.as_deref(),
            hint: exec_err.hint.as_deref(),
            details: exec_err.details.as_ref(),
            interactive_resolution_required: exec_err.interactive_resolution_required.as_ref(),
            cleanup_status: exec_err.cleanup_status,
            cleanup_actions: &exec_err.cleanup_actions,
            manifest_suggestion: exec_err.manifest_suggestion.as_ref(),
        };
        let json: serde_json::Value = serde_json::to_value(&payload).expect("serialize event");

        // Legacy details path stays intact (desktop's
        // `ConsentRequiredDetailsDto` still consumes this).
        assert_eq!(
            json["details"]["reason"].as_str(),
            Some("execution_plan_consent_required"),
            "legacy details.reason discriminator must stay; got: {json}"
        );
        assert_eq!(json["details"]["scoped_id"].as_str(), Some("publisher/app"));

        // New typed envelope is emitted at the top level.
        let env = &json["interactive_resolution_required"];
        assert_eq!(
            env["kind"]["type"].as_str(),
            Some("consent_required"),
            "envelope.kind.type discriminator missing; got: {json}"
        );
        assert_eq!(env["kind"]["scoped_id"].as_str(), Some("publisher/app"));
        assert_eq!(env["kind"]["target_label"].as_str(), Some("cli"));
        assert_eq!(
            env["kind"]["policy_segment_hash"].as_str(),
            Some("blake3:aaa")
        );
        assert_eq!(env["display"]["message"].as_str(), Some("consent required"));
    }

    /// Errors that have no interactive-resolution UI today must NOT
    /// emit the envelope (omitted via `skip_serializing_if`). Keeps
    /// the wire shape compact for the common error case and prevents
    /// future consumers from spuriously routing on `null`.
    #[test]
    fn jsonl_event_omits_envelope_for_non_interactive_error() {
        let exec_err = AtoExecutionError::internal("boom".to_string());

        let payload = AtoErrorEvent {
            level: "fatal",
            code: exec_err.code,
            name: exec_err.name,
            phase: exec_err.phase,
            classification: exec_err.classification.as_str(),
            message: &exec_err.message,
            retryable: exec_err.retryable,
            interactive_resolution: exec_err.interactive_resolution,
            resource: exec_err.resource.as_deref(),
            target: exec_err.target.as_deref(),
            hint: exec_err.hint.as_deref(),
            details: exec_err.details.as_ref(),
            interactive_resolution_required: exec_err.interactive_resolution_required.as_ref(),
            cleanup_status: exec_err.cleanup_status,
            cleanup_actions: &exec_err.cleanup_actions,
            manifest_suggestion: exec_err.manifest_suggestion.as_ref(),
        };
        let json: serde_json::Value = serde_json::to_value(&payload).expect("serialize event");

        assert!(
            json.get("interactive_resolution_required").is_none(),
            "envelope must be omitted for non-interactive errors; got: {json}"
        );
    }
}
