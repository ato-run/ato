use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::ato_lock::AtoLock;
use crate::error::{CapsuleError, Result};

const KIND_METADATA_ONLY: &str = "metadata_only";
const KIND_RUNTIME_CLOSURE: &str = "runtime_closure";
const KIND_BUILD_CLOSURE: &str = "build_closure";
const KIND_IMPORTED_ARTIFACT_CLOSURE: &str = "imported_artifact_closure";

const STATUS_INCOMPLETE: &str = "incomplete";
const STATUS_COMPLETE: &str = "complete";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClosureInfo {
    pub kind: String,
    pub status: String,
    pub digestable: bool,
    pub provenance_limited: bool,
}

pub fn normalize_lock_closure(lock: &mut AtoLock) -> Result<()> {
    normalize_resolution_closure_entries(&mut lock.resolution.entries)
}

pub fn normalize_resolution_closure_entries(entries: &mut BTreeMap<String, Value>) -> Result<()> {
    if let Some(closure) = entries.get_mut("closure") {
        *closure = normalize_closure_value(closure)?;
    }
    Ok(())
}

pub fn normalize_closure_value(value: &Value) -> Result<Value> {
    let mut closure = value.as_object().cloned().ok_or_else(|| {
        CapsuleError::Config("resolution.closure must be a JSON object".to_string())
    })?;

    if closure.get("kind").is_none()
        && closure
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status == STATUS_COMPLETE)
        && closure.get("inputs").and_then(Value::as_array).is_some()
    {
        closure.insert(
            "kind".to_string(),
            Value::String(KIND_RUNTIME_CLOSURE.to_string()),
        );
    }

    if closure.get("status").is_none()
        && closure
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == KIND_METADATA_ONLY)
    {
        closure.insert(
            "status".to_string(),
            Value::String(STATUS_INCOMPLETE.to_string()),
        );
    }

    if closure
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == KIND_METADATA_ONLY)
        && !closure.contains_key("observed_lockfiles")
    {
        closure.insert("observed_lockfiles".to_string(), Value::Array(Vec::new()));
    }

    if closure
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == KIND_BUILD_CLOSURE)
    {
        normalize_build_environment_shape(&mut closure)?;
    }

    sort_closure_arrays(&mut closure);

    Ok(Value::Object(closure))
}

pub fn validate_closure_value(value: &Value) -> std::result::Result<(), Vec<String>> {
    let normalized = normalize_closure_value(value).map_err(|err| vec![err.to_string()])?;
    let closure = normalized
        .as_object()
        .expect("normalized resolution.closure must remain an object");
    let kind = closure
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| vec!["resolution.closure.kind is required".to_string()])?;
    let status = closure
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| vec!["resolution.closure.status is required".to_string()])?;

    let mut errors = Vec::new();

    if !matches!(
        kind,
        KIND_METADATA_ONLY
            | KIND_RUNTIME_CLOSURE
            | KIND_BUILD_CLOSURE
            | KIND_IMPORTED_ARTIFACT_CLOSURE
    ) {
        errors.push(format!(
            "resolution.closure.kind must be one of metadata_only, runtime_closure, build_closure, imported_artifact_closure; got '{kind}'"
        ));
    }

    if !matches!(status, STATUS_INCOMPLETE | STATUS_COMPLETE) {
        errors.push(format!(
            "resolution.closure.status must be one of incomplete or complete; got '{status}'"
        ));
    }

    match kind {
        KIND_METADATA_ONLY => validate_metadata_only_closure(closure, status, &mut errors),
        KIND_RUNTIME_CLOSURE => validate_runtime_closure(closure, &mut errors),
        KIND_BUILD_CLOSURE => validate_build_closure(closure, &mut errors),
        KIND_IMPORTED_ARTIFACT_CLOSURE => validate_imported_artifact_closure(closure, &mut errors),
        _ => {}
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn closure_info(value: &Value) -> Result<ClosureInfo> {
    validate_closure_value(value).map_err(|errors| {
        CapsuleError::Config(format!(
            "resolution.closure is invalid: {}",
            errors.join("; ")
        ))
    })?;
    let normalized = normalize_closure_value(value)?;
    let closure = normalized
        .as_object()
        .expect("normalized resolution.closure must remain an object");
    let artifact = closure.get("artifact").and_then(Value::as_object);
    let kind = closure
        .get("kind")
        .and_then(Value::as_str)
        .expect("validated closure kind missing");
    let status = closure
        .get("status")
        .and_then(Value::as_str)
        .expect("validated closure status missing");

    Ok(ClosureInfo {
        kind: kind.to_string(),
        status: status.to_string(),
        digestable: kind != KIND_METADATA_ONLY && status == STATUS_COMPLETE,
        provenance_limited: artifact
            .and_then(|value| value.get("provenance_limited"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub fn compute_closure_digest(value: &Value) -> Result<Option<String>> {
    let normalized = normalize_closure_value(value)?;
    let info = closure_info(&normalized)?;
    if !info.digestable {
        return Ok(None);
    }

    let canonical = serde_jcs::to_vec(&normalized).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize resolution.closure for closure_digest: {err}"
        ))
    })?;
    Ok(Some(format!(
        "blake3:{}",
        blake3::hash(&canonical).to_hex()
    )))
}

fn validate_metadata_only_closure(
    closure: &Map<String, Value>,
    status: &str,
    errors: &mut Vec<String>,
) {
    if status != STATUS_INCOMPLETE {
        errors.push(
            "resolution.closure metadata_only entries must use status='incomplete'".to_string(),
        );
    }

    match closure.get("observed_lockfiles") {
        Some(Value::Array(values)) => {
            if values
                .iter()
                .any(|value| value.as_str().is_none_or(|item| item.trim().is_empty()))
            {
                errors.push(
                    "resolution.closure.observed_lockfiles must contain only non-empty strings"
                        .to_string(),
                );
            }
        }
        Some(_) => errors
            .push("resolution.closure.observed_lockfiles must be an array of strings".to_string()),
        None => errors.push(
            "resolution.closure metadata_only entries must include observed_lockfiles".to_string(),
        ),
    }

    for forbidden in ["inputs", "build_environment", "artifact"] {
        if closure.contains_key(forbidden) {
            errors.push(format!(
                "resolution.closure metadata_only entries must not contain {forbidden}"
            ));
        }
    }

    for key in closure.keys() {
        if !matches!(key.as_str(), "kind" | "status" | "observed_lockfiles") {
            errors.push(format!(
                "resolution.closure metadata_only entries must not contain {key}"
            ));
        }
    }
}

fn validate_runtime_closure(closure: &Map<String, Value>, errors: &mut Vec<String>) {
    validate_inputs(closure, errors);
    for forbidden in ["observed_lockfiles", "build_environment", "artifact"] {
        if closure.contains_key(forbidden) {
            errors.push(format!(
                "resolution.closure runtime_closure entries must not contain {forbidden}"
            ));
        }
    }
}

fn validate_build_closure(closure: &Map<String, Value>, errors: &mut Vec<String>) {
    validate_inputs(closure, errors);
    validate_build_environment(closure.get("build_environment"), errors);
    for forbidden in ["observed_lockfiles", "artifact"] {
        if closure.contains_key(forbidden) {
            errors.push(format!(
                "resolution.closure build_closure entries must not contain {forbidden}"
            ));
        }
    }
}

fn validate_imported_artifact_closure(closure: &Map<String, Value>, errors: &mut Vec<String>) {
    validate_artifact(closure.get("artifact"), errors);
    for forbidden in ["observed_lockfiles", "inputs", "build_environment"] {
        if closure.contains_key(forbidden) {
            errors.push(format!(
                "resolution.closure imported_artifact_closure entries must not contain {forbidden}"
            ));
        }
    }
}

fn validate_inputs(closure: &Map<String, Value>, errors: &mut Vec<String>) {
    let Some(inputs) = closure.get("inputs") else {
        errors.push("resolution.closure.inputs is required".to_string());
        return;
    };

    let Some(inputs) = inputs.as_array() else {
        errors.push("resolution.closure.inputs must be an array".to_string());
        return;
    };

    for (index, input) in inputs.iter().enumerate() {
        let Some(input) = input.as_object() else {
            errors.push(format!(
                "resolution.closure.inputs[{index}] must be an object"
            ));
            continue;
        };
        for field in ["kind", "name", "digest"] {
            if input
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .is_none_or(str::is_empty)
            {
                errors.push(format!(
                    "resolution.closure.inputs[{index}].{field} must be a non-empty string"
                ));
            }
        }
    }
}

fn validate_build_environment(value: Option<&Value>, errors: &mut Vec<String>) {
    let Some(value) = value else {
        errors.push("resolution.closure.build_environment is required".to_string());
        return;
    };
    let Some(environment) = value.as_object() else {
        errors.push("resolution.closure.build_environment must be an object".to_string());
        return;
    };

    for field in ["toolchains", "package_managers", "sdks", "helper_tools"] {
        let Some(values) = environment.get(field) else {
            errors.push(format!(
                "resolution.closure.build_environment.{field} is required"
            ));
            continue;
        };
        let Some(values) = values.as_array() else {
            errors.push(format!(
                "resolution.closure.build_environment.{field} must be an array of strings"
            ));
            continue;
        };
        if values
            .iter()
            .any(|value| value.as_str().is_none_or(|entry| entry.trim().is_empty()))
        {
            errors.push(format!(
                "resolution.closure.build_environment.{field} must contain only non-empty strings"
            ));
        }
    }
}

fn normalize_build_environment_shape(closure: &mut Map<String, Value>) -> Result<()> {
    let Some(value) = closure.get_mut("build_environment") else {
        return Ok(());
    };
    let Some(environment) = value.as_object_mut() else {
        return Err(CapsuleError::Config(
            "resolution.closure.build_environment must be an object".to_string(),
        ));
    };

    normalize_build_environment_field(environment, "toolchain", "toolchains");
    normalize_build_environment_field(environment, "package_manager", "package_managers");
    normalize_build_environment_field(environment, "sdk", "sdks");

    Ok(())
}

fn normalize_build_environment_field(
    environment: &mut Map<String, Value>,
    singular: &str,
    plural: &str,
) {
    if environment.contains_key(plural) {
        return;
    }

    let Some(value) = environment.remove(singular) else {
        return;
    };

    match value {
        Value::Null => {
            environment.insert(plural.to_string(), Value::Array(Vec::new()));
        }
        Value::Array(values) => {
            environment.insert(plural.to_string(), Value::Array(values));
        }
        Value::String(value) => {
            environment.insert(plural.to_string(), Value::Array(vec![Value::String(value)]));
        }
        other => {
            environment.insert(plural.to_string(), other);
        }
    }
}

fn sort_closure_arrays(closure: &mut Map<String, Value>) {
    sort_string_array_field(closure, "observed_lockfiles");
    sort_inputs_array_field(closure, "inputs");

    if let Some(environment) = closure
        .get_mut("build_environment")
        .and_then(Value::as_object_mut)
    {
        for field in ["toolchains", "package_managers", "sdks", "helper_tools"] {
            sort_string_array_field(environment, field);
        }
    }
}

fn sort_string_array_field(object: &mut Map<String, Value>, field: &str) {
    let Some(values) = object.get_mut(field).and_then(Value::as_array_mut) else {
        return;
    };

    values.sort_by(|left, right| {
        left.as_str()
            .unwrap_or_default()
            .cmp(right.as_str().unwrap_or_default())
    });
}

fn sort_inputs_array_field(object: &mut Map<String, Value>, field: &str) {
    let Some(values) = object.get_mut(field).and_then(Value::as_array_mut) else {
        return;
    };

    values.sort_by_key(closure_input_sort_key);
}

fn closure_input_sort_key(value: &Value) -> (String, String, String, String) {
    let Some(object) = value.as_object() else {
        return (String::new(), String::new(), String::new(), value.to_string());
    };

    (
        object
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        object
            .get("digest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        value.to_string(),
    )
}

fn validate_artifact(value: Option<&Value>, errors: &mut Vec<String>) {
    let Some(value) = value else {
        errors.push("resolution.closure.artifact is required".to_string());
        return;
    };
    let Some(artifact) = value.as_object() else {
        errors.push("resolution.closure.artifact must be an object".to_string());
        return;
    };

    for field in ["artifact_type", "digest"] {
        if artifact
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            errors.push(format!(
                "resolution.closure.artifact.{field} must be a non-empty string"
            ));
        }
    }

    if artifact
        .get("provenance_limited")
        .and_then(Value::as_bool)
        .is_none()
    {
        errors.push("resolution.closure.artifact.provenance_limited must be a boolean".to_string());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_metadata_only_legacy_shape() {
        let normalized = normalize_closure_value(&json!({
            "kind": "metadata_only",
        }))
        .expect("normalize closure");

        assert_eq!(
            normalized,
            json!({
                "kind": "metadata_only",
                "status": "incomplete",
                "observed_lockfiles": [],
            })
        );
    }

    #[test]
    fn normalizes_complete_inputs_legacy_shape_to_runtime_closure() {
        let normalized = normalize_closure_value(&json!({
            "status": "complete",
            "inputs": [],
        }))
        .expect("normalize closure");

        assert_eq!(
            normalized,
            json!({
                "kind": "runtime_closure",
                "status": "complete",
                "inputs": [],
            })
        );
    }

    #[test]
    fn validates_build_closure_shape() {
        validate_closure_value(&json!({
            "kind": "build_closure",
            "status": "complete",
            "inputs": [{"kind": "lockfile", "name": "package-lock.json", "digest": "sha256:abc"}],
            "build_environment": {
                "toolchains": ["node:20"],
                "package_managers": ["npm"],
                "sdks": ["xcode"],
                "helper_tools": ["codesign"]
            }
        }))
        .expect("build closure should validate");
    }

    #[test]
    fn normalizes_legacy_build_environment_shape() {
        let normalized = normalize_closure_value(&json!({
            "kind": "build_closure",
            "status": "complete",
            "inputs": [],
            "build_environment": {
                "toolchain": "rust",
                "package_manager": "pnpm",
                "sdk": "apple-sdk",
                "helper_tools": ["tauri-cli", "codesign"]
            }
        }))
        .expect("normalize closure");

        assert_eq!(
            normalized,
            json!({
                "kind": "build_closure",
                "status": "complete",
                "inputs": [],
                "build_environment": {
                    "toolchains": ["rust"],
                    "package_managers": ["pnpm"],
                    "sdks": ["apple-sdk"],
                    "helper_tools": ["codesign", "tauri-cli"]
                }
            })
        );
    }

    #[test]
    fn rejects_complete_metadata_only_closure() {
        let errors = validate_closure_value(&json!({
            "kind": "metadata_only",
            "status": "complete",
            "observed_lockfiles": []
        }))
        .expect_err("metadata_only complete should fail");

        assert!(errors
            .iter()
            .any(|error| error.contains("status='incomplete'")));
    }

    #[test]
    fn computes_digest_from_normalized_closure_shape() {
        let legacy = json!({
            "status": "complete",
            "inputs": [],
        });
        let normalized = json!({
            "kind": "runtime_closure",
            "status": "complete",
            "inputs": [],
        });

        assert_eq!(
            compute_closure_digest(&legacy).expect("legacy digest"),
            compute_closure_digest(&normalized).expect("normalized digest")
        );
    }

    #[test]
    fn returns_none_for_incomplete_closure_digest() {
        assert_eq!(
            compute_closure_digest(&json!({
                "kind": "metadata_only",
                "status": "incomplete",
                "observed_lockfiles": ["package-lock.json"],
            }))
            .expect("compute digest"),
            None
        );
    }

    #[test]
    fn sorts_digest_relevant_closure_arrays_deterministically() {
        let normalized = normalize_closure_value(&json!({
            "kind": "build_closure",
            "status": "complete",
            "inputs": [
                {"kind": "package", "name": "z", "digest": "sha256:z"},
                {"kind": "lockfile", "name": "a", "digest": "sha256:a"}
            ],
            "build_environment": {
                "toolchains": ["z", "a"],
                "package_managers": ["pnpm", "npm"],
                "sdks": ["xcode", "apple-sdk"],
                "helper_tools": ["codesign", "actool"]
            }
        }))
        .expect("normalize closure");

        assert_eq!(
            normalized,
            json!({
                "kind": "build_closure",
                "status": "complete",
                "inputs": [
                    {"kind": "lockfile", "name": "a", "digest": "sha256:a"},
                    {"kind": "package", "name": "z", "digest": "sha256:z"}
                ],
                "build_environment": {
                    "toolchains": ["a", "z"],
                    "package_managers": ["npm", "pnpm"],
                    "sdks": ["apple-sdk", "xcode"],
                    "helper_tools": ["actool", "codesign"]
                }
            })
        );
    }
}
