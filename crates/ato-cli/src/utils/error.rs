use anyhow::Error as AnyhowError;
use serde::Serialize;
use serde_json::Value;

use capsule_core::execution_plan::error::{
    AtoExecutionError, CleanupActionRecord, CleanupStatus, ManifestSuggestion,
};

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
