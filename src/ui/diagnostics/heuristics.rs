use anyhow::Error as AnyhowError;
use serde_json::Value;

pub(super) fn collect_causes(err: &AnyhowError) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for cause in err.chain().skip(1) {
        let value = cause.to_string();
        if values.last() != Some(&value) {
            values.push(value);
        }
    }
    values
}

pub(super) fn json_string_field<'a>(details: Option<&'a Value>, key: &str) -> Option<&'a str> {
    details
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
}

pub(super) fn is_manifest_parse(message: &str) -> bool {
    message.contains("Failed to parse manifest TOML")
        || message.contains("TOML parse error")
        || message.contains("expected")
            && message.contains("capsule.toml")
            && message.to_ascii_lowercase().contains("parse")
}

pub(super) fn is_required_field_issue(message: &str) -> bool {
    message.contains("default_target is required")
        || message.contains("Missing required field")
        || message.contains("Missing required [targets] table")
        || message.contains("At least one [targets.<label>] entry is required")
        || message.contains("default_target") && message.contains("does not exist under [targets]")
}

pub(super) fn is_entrypoint_issue(message: &str) -> bool {
    message.contains("Entrypoint not found")
        || message.contains("No entrypoint defined in capsule.toml")
        || message.contains("entrypoint")
            && (message.contains("does not exist") || message.contains("Path:"))
}

pub(super) fn is_source_registration_issue(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    message.contains("Source registration")
        || message.contains("GitHub")
        || message.contains("authentication")
        || lower.contains("register source")
        || lower.contains("source register")
}

pub(super) fn is_publish_version_exists_conflict(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    (lower.contains("artifact upload") && lower.contains("(409"))
        && (lower.contains("version_exists")
            || lower.contains("same version is already published")
            || lower.contains("sha256 mismatch"))
}

pub(super) fn is_manual_intervention_issue(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("manual intervention required")
}

pub(super) fn detect_field(message: &str) -> Option<&'static str> {
    if message.contains("default_target") {
        Some("default_target")
    } else if message.contains("[targets") || message.contains("targets.") {
        Some("targets")
    } else if message.contains("schema_version") {
        Some("schema_version")
    } else {
        None
    }
}
