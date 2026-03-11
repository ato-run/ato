use anyhow::Error as AnyhowError;
use serde::Serialize;

use capsule_core::execution_plan::error::AtoExecutionError;

#[derive(Debug, Serialize)]
struct AtoErrorEvent<'a> {
    level: &'static str,
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

pub fn emit_ato_error_jsonl(err: &AtoExecutionError) {
    let payload = AtoErrorEvent {
        level: "fatal",
        code: err.code,
        message: &err.message,
        resource: err.resource.as_deref(),
        target: err.target.as_deref(),
        hint: err.hint.as_deref(),
    };

    if let Ok(line) = serde_json::to_string(&payload) {
        eprintln!("{}", line);
    }
}

pub fn try_emit_from_anyhow(err: &AnyhowError, json_mode: bool) -> bool {
    if !json_mode {
        return false;
    }

    if let Some(execution_err) = err.downcast_ref::<AtoExecutionError>() {
        emit_ato_error_jsonl(execution_err);
        return true;
    }

    let message = err.to_string();

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
        emit_ato_error_jsonl(&AtoExecutionError::policy_violation(message));
        return true;
    }

    if message.contains("sandbox") && message.contains("not available") {
        emit_ato_error_jsonl(&AtoExecutionError::compat_hardware(message, None));
        return true;
    }

    false
}
