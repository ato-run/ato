use crate::capsule_lock::closure::{normalize_closure_value, validate_closure_value};
use chrono::DateTime;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::capsule_lock::hash::compute_lock_id;
use crate::capsule_lock::schema::{
    CAPSULE_LOCK_SCHEMA_VERSION, CapsuleLock, DeliveryEnvironment, FeatureName, KnownFeature,
    LockSignature, UnresolvedReason, UnresolvedValue, parse_delivery_environment_value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Strict,
    NonStrict,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CapsuleLockValidationError {
    #[error("schema_version must be {expected}, got {actual}")]
    InvalidSchemaVersion { expected: u32, actual: u32 },
    #[error("generated_at must be RFC3339, got '{0}'")]
    InvalidGeneratedAt(String),
    #[error("lock_id is required for persisted capsule.lock artifacts")]
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
    #[error("invalid authoring identity: {0}")]
    InvalidAuthoringIdentity(String),
}

/// Structural validation accepts draft locks without requiring lock_id.
///
/// This validates schema version, generated_at formatting, feature encoding,
/// unresolved marker shape, and signature placeholders. It does not require a
/// persisted artifact boundary and therefore does not require lock_id to exist
/// or match the canonical projection.
pub fn validate_structural(
    lock: &CapsuleLock,
    mode: ValidationMode,
) -> std::result::Result<(), Vec<CapsuleLockValidationError>> {
    let mut errors = Vec::new();

    if lock.schema_version != CAPSULE_LOCK_SCHEMA_VERSION {
        errors.push(CapsuleLockValidationError::InvalidSchemaVersion {
            expected: CAPSULE_LOCK_SCHEMA_VERSION,
            actual: lock.schema_version,
        });
    }

    if let Some(generated_at) = &lock.generated_at
        && DateTime::parse_from_rfc3339(generated_at).is_err()
    {
        errors.push(CapsuleLockValidationError::InvalidGeneratedAt(
            generated_at.clone(),
        ));
    }

    validate_declared_features(&lock.features.declared, mode, &mut errors);
    validate_required_features(&lock.features.required_for_execution, mode, &mut errors);
    validate_resolution_closure(lock, &mut errors);
    validate_contract_delivery(lock, &mut errors);
    validate_authoring_identity(lock, &mut errors);

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

fn sha256_jcs<T: serde::Serialize>(value: &T) -> Result<String, String> {
    let bytes = serde_jcs::to_vec(value).map_err(|error| error.to_string())?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn validate_authoring_identity(lock: &CapsuleLock, errors: &mut Vec<CapsuleLockValidationError>) {
    let mut invalid = Vec::new();
    if let Some(manifest) = &lock.manifest {
        if manifest.schema_version != crate::types::manifest_v1::MANIFEST_SCHEMA_V1 {
            invalid.push("manifest.schema_version must be '1'".to_string());
        }
        if !valid_sha256(&manifest.normalized_digest) {
            invalid.push("manifest.normalized_digest is malformed".to_string());
        }
    }
    if let Some(selection) = &lock.source_selection {
        if selection.policy_version != crate::types::manifest_v1::SOURCE_FILTER_POLICY_VERSION_V1 {
            invalid.push("source_selection.policy_version is unsupported".to_string());
        }
        for (field, digest) in [
            ("system_ignore_digest", &selection.system_ignore_digest),
            ("manifest_ignore_digest", &selection.manifest_ignore_digest),
            (
                "effective_ignore_digest",
                &selection.effective_ignore_digest,
            ),
        ] {
            if !valid_sha256(digest) {
                invalid.push(format!("source_selection.{field} is malformed"));
            }
        }
        let system = crate::types::manifest_v1::SYSTEM_SOURCE_IGNORE_V1
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect::<Vec<_>>();
        match sha256_jcs(&system) {
            Ok(expected) if expected != selection.system_ignore_digest => {
                invalid.push("source_selection.system_ignore_digest mismatch".to_string());
            }
            Err(error) => invalid.push(format!("canonicalize system ignore policy: {error}")),
            _ => {}
        }
        if !selection.effective_ignore.starts_with(&system) {
            invalid.push(
                "source_selection.effective_ignore must begin with the system policy".to_string(),
            );
        }
        let authored = selection
            .effective_ignore
            .strip_prefix(system.as_slice())
            .unwrap_or_default();
        match sha256_jcs(&authored) {
            Ok(expected) if expected != selection.manifest_ignore_digest => {
                invalid.push("source_selection.manifest_ignore_digest mismatch".to_string());
            }
            Err(error) => invalid.push(format!("canonicalize manifest ignore policy: {error}")),
            _ => {}
        }
        match sha256_jcs(&selection.effective_ignore) {
            Ok(expected) if expected != selection.effective_ignore_digest => {
                invalid.push("source_selection.effective_ignore_digest mismatch".to_string());
            }
            Err(error) => invalid.push(format!("canonicalize effective ignore policy: {error}")),
            _ => {}
        }
    }
    if let Some(assets) = &lock.metadata_assets {
        for (field, asset) in [
            ("icon", assets.icon.as_ref()),
            ("banner", assets.banner.as_ref()),
        ] {
            let Some(asset) = asset else { continue };
            if !matches!(asset.origin.kind.as_str(), "path" | "url")
                || asset.origin.value.trim().is_empty()
            {
                invalid.push(format!("metadata_assets.{field}.origin is invalid"));
            } else if asset.origin.kind == "path"
                && (asset.origin.value.starts_with('/')
                    || asset.origin.value.contains('\\')
                    || asset
                        .origin
                        .value
                        .split('/')
                        .any(|segment| segment.is_empty() || matches!(segment, "." | "..")))
            {
                invalid.push(format!(
                    "metadata_assets.{field}.origin path is not normalized"
                ));
            } else if asset.origin.kind == "url"
                && url::Url::parse(&asset.origin.value).map_or(true, |url| {
                    url.scheme() != "https"
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.fragment().is_some()
                        || url.host_str().is_none()
                })
            {
                invalid.push(format!(
                    "metadata_assets.{field}.origin URL is not a credential-free HTTPS URL"
                ));
            }
            if !valid_sha256(&asset.content_digest) {
                invalid.push(format!(
                    "metadata_assets.{field}.content_digest is malformed"
                ));
            }
            let expected_ref = asset
                .content_digest
                .strip_prefix("sha256:")
                .map(|digest| format!("ato-asset://sha256/{digest}"));
            if asset.artifact_ref.as_ref() != expected_ref.as_ref() {
                invalid.push(format!(
                    "metadata_assets.{field}.artifact_ref does not match content_digest"
                ));
            }
            if !matches!(
                asset.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            ) {
                invalid.push(format!("metadata_assets.{field}.media_type is unsupported"));
            }
        }
    }
    errors.extend(
        invalid
            .into_iter()
            .map(CapsuleLockValidationError::InvalidAuthoringIdentity),
    );
}

/// Persisted validation applies structural validation and then enforces lock_id.
///
/// IDENTITY-ONLY: this validates schema version, `generated_at` formatting,
/// feature/unresolved/signature shape, and `lock_id` (presence, format, and
/// match against the canonical projection). It does NOT verify the lock's
/// embedded execution section — neither the D2 `execution_contract` envelope nor
/// the D5 `launch.environment` value payloads, which are excluded from lock
/// identity by design. A lock with a tampered `execution_id` or a
/// tampered/secret D5 payload still passes here. Any lock that may carry an
/// execution section MUST instead be read through the trusted entrypoints
/// `load_verified_from_str` / `load_verified_from_path` (in the parent
/// `capsule_lock` module), which run this identity validation AND then re-derive the
/// execution section fail-closed.
///
/// Call this only when validating a durable capsule.lock artifact's identity or when
/// preparing to serialize one. Draft lock values produced by later
/// resolver/importer stages should use structural validation until lock_id has
/// been recomputed.
pub fn validate_persisted(
    lock: &CapsuleLock,
    mode: ValidationMode,
) -> std::result::Result<(), Vec<CapsuleLockValidationError>> {
    let mut errors = match validate_structural(lock, mode) {
        Ok(()) => Vec::new(),
        Err(errors) => errors,
    };

    match &lock.lock_id {
        None => errors.push(CapsuleLockValidationError::MissingLockId),
        Some(lock_id) => {
            if let Err(message) = lock_id.validate_format() {
                errors.push(CapsuleLockValidationError::MalformedLockId(message));
            }
        }
    }

    if let Some(lock_id) = &lock.lock_id {
        match compute_lock_id(lock) {
            Ok(expected) if expected != *lock_id => {
                errors.push(CapsuleLockValidationError::LockIdMismatch {
                    expected: expected.as_str().to_string(),
                    actual: lock_id.as_str().to_string(),
                });
            }
            Ok(_) => {}
            Err(err) => errors.push(CapsuleLockValidationError::MalformedLockId(err.to_string())),
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
    errors: &mut Vec<CapsuleLockValidationError>,
) {
    for feature in features {
        match feature {
            FeatureName::Unknown(value) if matches!(mode, ValidationMode::Strict) => {
                errors.push(CapsuleLockValidationError::UnknownDeclaredFeature(
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
    errors: &mut Vec<CapsuleLockValidationError>,
) {
    for feature in features {
        match feature {
            FeatureName::Unknown(value) => {
                let _ = mode;
                errors.push(CapsuleLockValidationError::UnknownRequiredFeature(
                    value.clone(),
                ));
            }
            FeatureName::Known(feature) if !is_supported_feature(*feature) => {
                errors.push(CapsuleLockValidationError::UnsupportedRequiredFeature(
                    feature.as_str().to_string(),
                ));
            }
            _ => {}
        }
    }
}

fn validate_unresolved(unresolved: &UnresolvedValue, errors: &mut Vec<CapsuleLockValidationError>) {
    // Unknown unresolved reasons and malformed ambiguity markers are treated as
    // structural invalidity even in non-strict mode. non-strict is intended to
    // relax forward-compatible feature handling, not to accept malformed state.
    if let UnresolvedReason::Unknown(value) = &unresolved.reason {
        errors.push(CapsuleLockValidationError::UnknownUnresolvedReason(
            value.clone(),
        ));
    }

    if matches!(unresolved.reason, UnresolvedReason::Ambiguity) && unresolved.candidates.is_empty()
    {
        errors.push(CapsuleLockValidationError::AmbiguityRequiresCandidates);
    }

    if unresolved
        .candidates
        .iter()
        .any(|candidate| candidate.trim().is_empty())
    {
        errors.push(CapsuleLockValidationError::InvalidUnresolvedCandidates);
    }
}

fn validate_signature(signature: &LockSignature, errors: &mut Vec<CapsuleLockValidationError>) {
    if signature.kind.trim().is_empty() {
        errors.push(CapsuleLockValidationError::EmptySignatureKind);
    }
}

fn validate_resolution_closure(lock: &CapsuleLock, errors: &mut Vec<CapsuleLockValidationError>) {
    let Some(closure) = lock.resolution.entries.get("closure") else {
        return;
    };

    if let Err(closure_errors) = validate_closure_value(closure) {
        errors.extend(
            closure_errors
                .into_iter()
                .map(CapsuleLockValidationError::InvalidClosure),
        );
    }
}

fn validate_contract_delivery(lock: &CapsuleLock, errors: &mut Vec<CapsuleLockValidationError>) {
    let Some(delivery) = lock.contract.entries.get("delivery") else {
        return;
    };

    if let Err(delivery_errors) = validate_delivery_value(lock, delivery) {
        errors.extend(
            delivery_errors
                .into_iter()
                .map(CapsuleLockValidationError::InvalidDelivery),
        );
    }
}

fn validate_delivery_value(
    lock: &CapsuleLock,
    value: &Value,
) -> std::result::Result<(), Vec<String>> {
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

    if let Some(install) = object.get("install").and_then(Value::as_object)
        && let Some(environment) = install.get("environment")
    {
        match parse_delivery_environment_value(environment) {
            Ok(environment) => validate_delivery_environment(&environment, &mut errors),
            Err(err) => errors.push(err),
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
        if let Some(healthcheck) = &service.healthcheck
            && healthcheck.kind.trim().is_empty()
        {
            errors.push(format!(
                    "contract.delivery.install.environment.services[{}].healthcheck.kind must be non-empty",
                    service.name
                ));
        }
    }
}

fn is_supported_feature(_feature: KnownFeature) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{CapsuleLock, ValidationMode, validate_structural};
    use crate::capsule_lock::{
        LockManifestSection, LockMetadataAsset, LockMetadataAssetOrigin, LockMetadataAssetsSection,
        LockSourceSelectionSection,
    };

    fn lock_with_delivery(delivery: Value, closure: Option<Value>) -> CapsuleLock {
        let mut lock = CapsuleLock::default();
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

        assert!(errors.iter().any(|error| {
            error
                .to_string()
                .contains("contract.delivery.build must be omitted for mode 'artifact-import'")
        }));
        assert!(errors.iter().any(|error| {
            error
                .to_string()
                .contains("contract.delivery.finalize must be omitted for mode 'artifact-import'")
        }));
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

        assert!(errors.iter().any(|error| {
            error
                .to_string()
                .contains("contract.delivery.install.environment.services[].name must be non-empty")
        }));
    }

    fn validation_messages(lock: &CapsuleLock) -> String {
        validate_structural(lock, ValidationMode::Strict)
            .expect_err("invalid authoring identity")
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn malformed_manifest_and_source_policy_are_rejected() {
        let lock = CapsuleLock {
            manifest: Some(LockManifestSection {
                schema_version: "0.3".to_string(),
                normalized_digest: "sha256:ABC".to_string(),
            }),
            source_selection: Some(LockSourceSelectionSection {
                root: ".".to_string(),
                policy_version: "future-policy".to_string(),
                effective_ignore: vec!["not-the-system-policy".to_string()],
                system_ignore_digest: format!("sha256:{}", "0".repeat(64)),
                manifest_ignore_digest: format!("sha256:{}", "0".repeat(64)),
                effective_ignore_digest: format!("sha256:{}", "0".repeat(64)),
            }),
            ..CapsuleLock::default()
        };
        let errors = validation_messages(&lock);
        assert!(errors.contains("manifest.schema_version"));
        assert!(errors.contains("manifest.normalized_digest"));
        assert!(errors.contains("policy_version"));
        assert!(errors.contains("system_ignore_digest mismatch"));
        assert!(errors.contains("must begin with the system policy"));
    }

    #[test]
    fn asset_origin_ref_and_media_type_are_rejected_fail_closed() {
        let lock = CapsuleLock {
            metadata_assets: Some(LockMetadataAssetsSection {
                icon: Some(LockMetadataAsset {
                    origin: LockMetadataAssetOrigin {
                        kind: "url".to_string(),
                        value: "http://user:secret@127.0.0.1/icon.png#fragment".to_string(),
                    },
                    content_digest: format!("sha256:{}", "a".repeat(64)),
                    artifact_ref: Some(format!("ato-asset://sha256/{}", "b".repeat(64))),
                    media_type: "application/octet-stream".to_string(),
                }),
                banner: None,
            }),
            ..CapsuleLock::default()
        };
        let errors = validation_messages(&lock);
        assert!(errors.contains("credential-free HTTPS"));
        assert!(errors.contains("artifact_ref does not match"));
        assert!(errors.contains("media_type is unsupported"));
    }
}
