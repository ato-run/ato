use crate::ato_lock::closure::{normalize_closure_value, validate_closure_value};
use chrono::DateTime;
use serde_json::Value;
use thiserror::Error;

use crate::ato_lock::hash::compute_lock_id;
use crate::ato_lock::schema::{
    parse_delivery_environment_value, AtoLock, DeliveryEnvironment, FeatureName, KnownFeature,
    LockSignature, UnresolvedReason, UnresolvedValue, ATO_LOCK_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Strict,
    NonStrict,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum AtoLockValidationError {
    #[error("schema_version must be {expected}, got {actual}")]
    InvalidSchemaVersion { expected: u32, actual: u32 },
    #[error("generated_at must be RFC3339, got '{0}'")]
    InvalidGeneratedAt(String),
    #[error("lock_id is required for persisted ato.lock artifacts")]
    MissingLockId,
    #[error("{0}")]
    MalformedLockId(String),
    #[error("lock_id mismatch: expected {expected}, got {actual}")]
    LockIdMismatch { expected: String, actual: String },
    #[error("declared feature '{0}' is unknown")]
    UnknownDeclaredFeature(String),
    #[error("declared feature '{0}' is recognized by schema but not implemented by this runtime")]
    UnsupportedDeclaredFeature(String),
    #[error("required feature '{0}' is unknown")]
    UnknownRequiredFeature(String),
    #[error("required feature '{0}' is recognized by schema but not implemented by this runtime")]
    UnsupportedRequiredFeature(String),
    #[error("unresolved reason '{0}' is unknown")]
    UnknownUnresolvedReason(String),
    #[error("unresolved ambiguity markers must include candidates")]
    AmbiguityRequiresCandidates,
    #[error("unresolved candidates must not contain empty values")]
    InvalidUnresolvedCandidates,
    #[error("signature kind must not be empty")]
    EmptySignatureKind,
    #[error("invalid resolution.closure: {0}")]
    InvalidClosure(String),
    #[error("invalid contract.delivery: {0}")]
    InvalidDelivery(String),
}

/// Structural validation accepts draft locks without requiring lock_id.
///
/// This validates schema version, generated_at formatting, feature encoding,
/// unresolved marker shape, and signature placeholders. It does not require a
/// persisted artifact boundary and therefore does not require lock_id to exist
/// or match the canonical projection.
pub fn validate_structural(
    lock: &AtoLock,
    mode: ValidationMode,
) -> std::result::Result<(), Vec<AtoLockValidationError>> {
    let mut errors = Vec::new();

    if lock.schema_version != ATO_LOCK_SCHEMA_VERSION {
        errors.push(AtoLockValidationError::InvalidSchemaVersion {
            expected: ATO_LOCK_SCHEMA_VERSION,
            actual: lock.schema_version,
        });
    }

    if let Some(generated_at) = &lock.generated_at {
        if DateTime::parse_from_rfc3339(generated_at).is_err() {
            errors.push(AtoLockValidationError::InvalidGeneratedAt(
                generated_at.clone(),
            ));
        }
    }

    validate_declared_features(&lock.features.declared, mode, &mut errors);
    validate_required_features(&lock.features.required_for_execution, mode, &mut errors);
    validate_resolution_closure(lock, &mut errors);
    validate_contract_delivery(lock, &mut errors);

    for unresolved in lock
        .resolution
        .unresolved
        .iter()
        .chain(lock.contract.unresolved.iter())
        .chain(lock.binding.unresolved.iter())
        .chain(lock.policy.unresolved.iter())
        .chain(lock.attestations.unresolved.iter())
    {
        validate_unresolved(unresolved, &mut errors);
    }

    for signature in &lock.signatures {
        validate_signature(signature, &mut errors);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Persisted validation applies structural validation and then enforces lock_id.
///
/// Call this only when validating a durable ato.lock artifact or when preparing
/// to serialize one. Draft lock values produced by later resolver/importer
/// stages should use structural validation until lock_id has been recomputed.
pub fn validate_persisted(
    lock: &AtoLock,
    mode: ValidationMode,
) -> std::result::Result<(), Vec<AtoLockValidationError>> {
    let mut errors = match validate_structural(lock, mode) {
        Ok(()) => Vec::new(),
        Err(errors) => errors,
    };

    match &lock.lock_id {
        None => errors.push(AtoLockValidationError::MissingLockId),
        Some(lock_id) => {
            if let Err(message) = lock_id.validate_format() {
                errors.push(AtoLockValidationError::MalformedLockId(message));
            }
        }
    }

    if let Some(lock_id) = &lock.lock_id {
        match compute_lock_id(lock) {
            Ok(expected) if expected != *lock_id => {
                errors.push(AtoLockValidationError::LockIdMismatch {
                    expected: expected.as_str().to_string(),
                    actual: lock_id.as_str().to_string(),
                });
            }
            Ok(_) => {}
            Err(err) => errors.push(AtoLockValidationError::MalformedLockId(err.to_string())),
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_declared_features(
    features: &[FeatureName],
    mode: ValidationMode,
    errors: &mut Vec<AtoLockValidationError>,
) {
    for feature in features {
        match feature {
            FeatureName::Unknown(value) if matches!(mode, ValidationMode::Strict) => {
                errors.push(AtoLockValidationError::UnknownDeclaredFeature(
                    value.clone(),
                ));
            }
            _ => {}
        }
    }
}

fn validate_required_features(
    features: &[FeatureName],
    mode: ValidationMode,
    errors: &mut Vec<AtoLockValidationError>,
) {
    for feature in features {
        match feature {
            FeatureName::Unknown(value) => {
                let _ = mode;
                errors.push(AtoLockValidationError::UnknownRequiredFeature(
                    value.clone(),
                ));
            }
            FeatureName::Known(feature) if !is_supported_feature(*feature) => {
                errors.push(AtoLockValidationError::UnsupportedRequiredFeature(
                    feature.as_str().to_string(),
                ));
            }
            _ => {}
        }
    }
}

fn validate_unresolved(unresolved: &UnresolvedValue, errors: &mut Vec<AtoLockValidationError>) {
    // Unknown unresolved reasons and malformed ambiguity markers are treated as
    // structural invalidity even in non-strict mode. non-strict is intended to
    // relax forward-compatible feature handling, not to accept malformed state.
    if let UnresolvedReason::Unknown(value) = &unresolved.reason {
        errors.push(AtoLockValidationError::UnknownUnresolvedReason(
            value.clone(),
        ));
    }

    if matches!(unresolved.reason, UnresolvedReason::Ambiguity) && unresolved.candidates.is_empty()
    {
        errors.push(AtoLockValidationError::AmbiguityRequiresCandidates);
    }

    if unresolved
        .candidates
        .iter()
        .any(|candidate| candidate.trim().is_empty())
    {
        errors.push(AtoLockValidationError::InvalidUnresolvedCandidates);
    }
}

fn validate_signature(signature: &LockSignature, errors: &mut Vec<AtoLockValidationError>) {
    if signature.kind.trim().is_empty() {
        errors.push(AtoLockValidationError::EmptySignatureKind);
    }
}

fn validate_resolution_closure(lock: &AtoLock, errors: &mut Vec<AtoLockValidationError>) {
    let Some(closure) = lock.resolution.entries.get("closure") else {
        return;
    };

    if let Err(closure_errors) = validate_closure_value(closure) {
        errors.extend(
            closure_errors
                .into_iter()
                .map(AtoLockValidationError::InvalidClosure),
        );
    }
}

fn validate_contract_delivery(lock: &AtoLock, errors: &mut Vec<AtoLockValidationError>) {
    let Some(delivery) = lock.contract.entries.get("delivery") else {
        return;
    };

    if let Err(delivery_errors) = validate_delivery_value(lock, delivery) {
        errors.extend(
            delivery_errors
                .into_iter()
                .map(AtoLockValidationError::InvalidDelivery),
        );
    }
}

fn validate_delivery_value(lock: &AtoLock, value: &Value) -> std::result::Result<(), Vec<String>> {
    let mut errors = Vec::new();
    let Some(object) = value.as_object() else {
        return Err(vec!["contract.delivery must be an object".to_string()]);
    };

    let normalized_closure = lock
        .resolution
        .entries
        .get("closure")
        .map(normalize_closure_value)
        .transpose()
        .map_err(|err| vec![err.to_string()])?;
    let closure = normalized_closure.as_ref().and_then(Value::as_object);

    let mode = match object.get("mode").and_then(Value::as_str) {
        Some(mode @ ("source-draft" | "source-derivation" | "artifact-import")) => mode,
        Some(other) => {
            errors.push(format!("contract.delivery.mode '{}' is unsupported", other));
            ""
        }
        None => {
            errors.push("contract.delivery.mode is required".to_string());
            ""
        }
    };

    validate_delivery_section(object, "artifact", &mut errors);
    validate_delivery_section(object, "install", &mut errors);
    validate_delivery_section(object, "projection", &mut errors);

    if let Some(install) = object.get("install").and_then(Value::as_object) {
        if let Some(environment) = install.get("environment") {
            match parse_delivery_environment_value(environment) {
                Ok(environment) => validate_delivery_environment(&environment, &mut errors),
                Err(err) => errors.push(err),
            }
        }
    }

    if let Some(artifact) = object.get("artifact").and_then(Value::as_object) {
        if artifact.get("kind").and_then(Value::as_str) != Some("desktop-native") {
            errors.push("contract.delivery.artifact.kind must be 'desktop-native'".to_string());
        }
        if artifact
            .get("canonical_build_input")
            .and_then(Value::as_bool)
            .is_none()
        {
            errors.push(
                "contract.delivery.artifact.canonical_build_input must be a boolean".to_string(),
            );
        }
        if artifact
            .get("provenance_limited")
            .and_then(Value::as_bool)
            .is_none()
        {
            errors.push(
                "contract.delivery.artifact.provenance_limited must be a boolean".to_string(),
            );
        }
    }

    match mode {
        "source-draft" | "source-derivation" => {
            validate_delivery_section(object, "build", &mut errors);
            validate_delivery_section(object, "finalize", &mut errors);
            if let Some(build) = object.get("build").and_then(Value::as_object) {
                if build.get("kind").and_then(Value::as_str) != Some("native-delivery") {
                    errors
                        .push("contract.delivery.build.kind must be 'native-delivery'".to_string());
                }
                let expected_status = if mode == "source-derivation" {
                    "complete"
                } else {
                    "incomplete"
                };
                if build.get("closure_status").and_then(Value::as_str) != Some(expected_status) {
                    errors.push(format!(
                        "contract.delivery.build.closure_status must be '{}' for mode '{}'",
                        expected_status, mode
                    ));
                }
                if build.get("requires_build_closure").and_then(Value::as_bool) != Some(true) {
                    errors.push(
                        "contract.delivery.build.requires_build_closure must be true for source delivery"
                            .to_string(),
                    );
                }
            }

            if mode == "source-draft" {
                if let Some(closure) = closure {
                    if closure.get("status").and_then(Value::as_str) != Some("incomplete") {
                        errors.push(
                            "contract.delivery.mode 'source-draft' requires resolution.closure.status = 'incomplete'"
                                .to_string(),
                        );
                    }
                    if closure.get("kind").and_then(Value::as_str)
                        == Some("imported_artifact_closure")
                    {
                        errors.push(
                            "contract.delivery.mode 'source-draft' must not use resolution.closure.kind = 'imported_artifact_closure'"
                                .to_string(),
                        );
                    }
                }
            } else {
                validate_delivery_closure_contract(
                    closure,
                    "build_closure",
                    "complete",
                    mode,
                    &mut errors,
                );
            }
        }
        "artifact-import" => {
            if let Some(artifact) = object.get("artifact").and_then(Value::as_object) {
                if artifact.get("provenance_limited").and_then(Value::as_bool) != Some(true) {
                    errors.push(
                        "contract.delivery.artifact.provenance_limited must be true for artifact-import"
                            .to_string(),
                    );
                }
                if artifact
                    .get("canonical_build_input")
                    .and_then(Value::as_bool)
                    != Some(false)
                {
                    errors.push(
                        "contract.delivery.artifact.canonical_build_input must be false for artifact-import"
                            .to_string(),
                    );
                }
            }
            for forbidden in ["build", "finalize"] {
                if object.contains_key(forbidden) {
                    errors.push(format!(
                        "contract.delivery.{} must be omitted for mode 'artifact-import'",
                        forbidden
                    ));
                }
            }

            validate_delivery_closure_contract(
                closure,
                "imported_artifact_closure",
                "complete",
                mode,
                &mut errors,
            );
        }
        _ => {}
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_delivery_section(
    object: &serde_json::Map<String, Value>,
    key: &str,
    errors: &mut Vec<String>,
) {
    if !object.get(key).is_some_and(Value::is_object) {
        errors.push(format!("contract.delivery.{} must be an object", key));
    }
}

fn validate_delivery_closure_contract(
    closure: Option<&serde_json::Map<String, Value>>,
    expected_kind: &str,
    expected_status: &str,
    mode: &str,
    errors: &mut Vec<String>,
) {
    let Some(closure) = closure else {
        errors.push(format!(
            "contract.delivery.mode '{}' requires resolution.closure to be present",
            mode
        ));
        return;
    };

    if closure.get("kind").and_then(Value::as_str) != Some(expected_kind) {
        errors.push(format!(
            "contract.delivery.mode '{}' requires resolution.closure.kind = '{}'",
            mode, expected_kind
        ));
    }
    if closure.get("status").and_then(Value::as_str) != Some(expected_status) {
        errors.push(format!(
            "contract.delivery.mode '{}' requires resolution.closure.status = '{}'",
            mode, expected_status
        ));
    }
}

fn validate_delivery_environment(environment: &DeliveryEnvironment, errors: &mut Vec<String>) {
    if environment.strategy.trim().is_empty() {
        errors.push(
            "contract.delivery.install.environment.strategy must be a non-empty string".to_string(),
        );
    }

    for service in &environment.services {
        if service.name.trim().is_empty() {
            errors.push(
                "contract.delivery.install.environment.services[].name must be non-empty"
                    .to_string(),
            );
        }
        if service.from.trim().is_empty() {
            errors.push(format!(
                "contract.delivery.install.environment.services[{}].from must be non-empty",
                service.name
            ));
        }
        if service.lifecycle.trim().is_empty() {
            errors.push(format!(
                "contract.delivery.install.environment.services[{}].lifecycle must be non-empty",
                service.name
            ));
        }
        if let Some(healthcheck) = &service.healthcheck {
            if healthcheck.kind.trim().is_empty() {
                errors.push(format!(
                    "contract.delivery.install.environment.services[{}].healthcheck.kind must be non-empty",
                    service.name
                ));
            }
        }
    }
}

fn is_supported_feature(_feature: KnownFeature) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{validate_structural, AtoLock, ValidationMode};

    fn lock_with_delivery(delivery: Value, closure: Option<Value>) -> AtoLock {
        let mut lock = AtoLock::default();
        lock.contract
            .entries
            .insert("delivery".to_string(), delivery);
        if let Some(closure) = closure {
            lock.resolution
                .entries
                .insert("closure".to_string(), closure);
        }
        lock
    }

    #[test]
    fn artifact_import_rejects_build_and_finalize_sections() {
        let lock = lock_with_delivery(
            json!({
                "mode": "artifact-import",
                "artifact": {
                    "kind": "desktop-native",
                    "artifact_type": "app-bundle",
                    "digest": "sha256:abc",
                    "canonical_build_input": false,
                    "provenance_limited": true
                },
                "build": {},
                "finalize": {},
                "install": {},
                "projection": {}
            }),
            Some(json!({
                "kind": "imported_artifact_closure",
                "status": "complete",
                "artifact": {
                    "artifact_type": "app-bundle",
                    "digest": "sha256:abc",
                    "provenance_limited": true
                }
            })),
        );

        let errors = validate_structural(&lock, ValidationMode::Strict)
            .expect_err("artifact-import with build/finalize should fail");

        assert!(errors.iter().any(|error| error
            .to_string()
            .contains("contract.delivery.build must be omitted for mode 'artifact-import'")));
        assert!(errors.iter().any(|error| error
            .to_string()
            .contains("contract.delivery.finalize must be omitted for mode 'artifact-import'")));
    }

    #[test]
    fn source_derivation_requires_complete_build_closure() {
        let lock = lock_with_delivery(
            json!({
                "mode": "source-derivation",
                "artifact": {
                    "kind": "desktop-native",
                    "canonical_build_input": false,
                    "provenance_limited": false
                },
                "build": {
                    "kind": "native-delivery",
                    "requires_build_closure": true,
                    "closure_status": "complete"
                },
                "finalize": {},
                "install": {},
                "projection": {}
            }),
            Some(json!({
                "kind": "metadata_only",
                "status": "incomplete",
                "observed_lockfiles": []
            })),
        );

        let errors = validate_structural(&lock, ValidationMode::Strict)
            .expect_err("source-derivation without build closure should fail");

        assert!(errors.iter().any(|error| error
            .to_string()
            .contains("contract.delivery.mode 'source-derivation' requires resolution.closure.kind = 'build_closure'")));
    }

    #[test]
    fn delivery_environment_rejects_empty_service_fields() {
        let lock = lock_with_delivery(
            json!({
                "mode": "artifact-import",
                "artifact": {
                    "kind": "desktop-native",
                    "artifact_type": "app-bundle",
                    "digest": "sha256:abc",
                    "canonical_build_input": false,
                    "provenance_limited": true
                },
                "install": {
                    "environment": {
                        "strategy": "ato-managed",
                        "services": [
                            {
                                "name": "",
                                "from": "",
                                "lifecycle": ""
                            }
                        ]
                    }
                },
                "projection": {}
            }),
            Some(json!({
                "kind": "imported_artifact_closure",
                "status": "complete",
                "artifact": {
                    "artifact_type": "app-bundle",
                    "digest": "sha256:abc",
                    "provenance_limited": true
                }
            })),
        );

        let errors = validate_structural(&lock, ValidationMode::Strict)
            .expect_err("invalid environment should fail");

        assert!(errors.iter().any(|error| error
            .to_string()
            .contains("contract.delivery.install.environment.services[].name must be non-empty")));
    }
}
