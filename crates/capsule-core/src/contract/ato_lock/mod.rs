mod canonicalize;
mod closure;
mod hash;
mod schema;
mod validate;

use std::fs;
use std::path::Path;

/// ato.lock v1 foundation module.
///
/// v1 intentionally uses one Rust model for both serde and in-memory draft
/// handling, while keeping canonical lock identity in a separate projection.
/// Load, validation, lock_id computation, and serialization are split so later
/// input-resolver and import flows can work with draft locks without being
/// forced through persisted artifact validation too early.
pub use canonicalize::{
    canonical_identity_projection, canonical_projection, is_canonical_identity_section,
    CanonicalLockProjection, CANONICAL_IDENTITY_EXCLUDED_SECTIONS,
    CANONICAL_IDENTITY_INCLUDED_SECTIONS,
};
pub use closure::{
    closure_info, compute_closure_digest, normalize_closure_value, normalize_lock_closure,
    normalize_resolution_closure_entries, validate_closure_value, ClosureInfo,
};
pub use hash::{
    canonical_document_bytes, canonical_projection_bytes, canonical_signature_payload_bytes,
    compute_lock_id, recompute_lock_id,
};
pub use schema::{
    delivery_environment, parse_delivery_environment_value, AtoLock, AttestationsSection,
    BindingSection, ContractSection, DeliveryBootstrap, DeliveryEnvironment, DeliveryHealthcheck,
    DeliveryRepair, DeliveryService, FeatureName, KnownFeature, LockFeatures, LockId,
    LockSignature, PolicySection, ResolutionSection, UnresolvedReason, UnresolvedValue,
    ATO_LOCK_SCHEMA_VERSION,
};
pub use validate::{
    validate_persisted, validate_structural, AtoLockValidationError, ValidationMode,
};

use crate::error::{CapsuleError, Result};

/// Parses ato.lock JSON without applying any validation.
pub fn load_unvalidated_from_str(raw: &str) -> Result<AtoLock> {
    serde_json::from_str(raw)
        .map_err(|err| CapsuleError::Config(format!("Failed to parse ato.lock.json: {err}")))
}

/// Reads ato.lock JSON from disk without applying any validation.
pub fn load_unvalidated_from_path(path: &Path) -> Result<AtoLock> {
    let raw = fs::read_to_string(path)
        .map_err(|err| CapsuleError::Config(format!("Failed to read {}: {err}", path.display())))?;
    load_unvalidated_from_str(&raw)
}

/// Validates a persisted lock under strict mode.
pub fn validate_persisted_strict(
    lock: &AtoLock,
) -> std::result::Result<(), Vec<AtoLockValidationError>> {
    validate_persisted(lock, ValidationMode::Strict)
}

/// Validates a persisted lock under non-strict mode.
pub fn validate_persisted_non_strict(
    lock: &AtoLock,
) -> std::result::Result<(), Vec<AtoLockValidationError>> {
    validate_persisted(lock, ValidationMode::NonStrict)
}

/// Validates a draft or persisted lock structurally under strict mode.
pub fn validate_structural_strict(
    lock: &AtoLock,
) -> std::result::Result<(), Vec<AtoLockValidationError>> {
    validate_structural(lock, ValidationMode::Strict)
}

/// Validates a draft or persisted lock structurally under non-strict mode.
pub fn validate_structural_non_strict(
    lock: &AtoLock,
) -> std::result::Result<(), Vec<AtoLockValidationError>> {
    validate_structural(lock, ValidationMode::NonStrict)
}

/// Pretty-serializes a durable ato.lock artifact.
///
/// This preserves generated_at as stored on the model and does not normalize
/// its textual representation beyond RFC3339 validation. lock_id is recomputed
/// before serialization and persisted validation must pass.
pub fn to_pretty_json(lock: &AtoLock) -> Result<String> {
    let mut persisted = lock.clone();
    normalize_lock_closure(&mut persisted)?;
    recompute_lock_id(&mut persisted)?;
    validate_persisted_strict(&persisted).map_err(validation_errors_to_capsule_error)?;
    serde_json::to_string_pretty(&persisted)
        .map_err(|err| CapsuleError::Config(format!("Failed to serialize ato.lock.json: {err}")))
}

/// Writes a durable pretty ato.lock artifact after recomputing lock_id.
pub fn write_pretty_to_path(lock: &AtoLock, path: &Path) -> Result<()> {
    let raw = to_pretty_json(lock)?;
    fs::write(path, raw)
        .map_err(|err| CapsuleError::Config(format!("Failed to write {}: {err}", path.display())))
}

/// Returns canonical persisted bytes for a durable ato.lock artifact.
pub fn write_canonical_to_vec(lock: &AtoLock) -> Result<Vec<u8>> {
    let mut persisted = lock.clone();
    normalize_lock_closure(&mut persisted)?;
    recompute_lock_id(&mut persisted)?;
    validate_persisted_strict(&persisted).map_err(validation_errors_to_capsule_error)?;
    serde_jcs::to_vec(&persisted)
        .map_err(|err| CapsuleError::Config(format!("Failed to canonicalize ato.lock JSON: {err}")))
}

/// Verifies that an existing persisted lock_id matches the canonical projection.
pub fn verify_lock_id(lock: &AtoLock) -> Result<()> {
    validate_persisted_strict(lock).map_err(validation_errors_to_capsule_error)?;
    Ok(())
}

fn validation_errors_to_capsule_error(errors: Vec<AtoLockValidationError>) -> CapsuleError {
    let message = errors
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    CapsuleError::Config(format!("ato.lock validation failed: {message}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::validate::AtoLockValidationError;
    use super::{
        canonical_projection_bytes, canonical_signature_payload_bytes, compute_lock_id,
        delivery_environment, is_canonical_identity_section, load_unvalidated_from_path,
        load_unvalidated_from_str, recompute_lock_id, to_pretty_json, validate_persisted_strict,
        validate_structural_non_strict, validate_structural_strict, write_pretty_to_path, AtoLock,
        FeatureName, KnownFeature, LockId, LockSignature, UnresolvedReason, UnresolvedValue,
        ATO_LOCK_SCHEMA_VERSION, CANONICAL_IDENTITY_EXCLUDED_SECTIONS,
    };

    fn sample_lock() -> AtoLock {
        let mut lock = AtoLock {
            generated_at: Some("2026-03-25T00:00:00Z".to_string()),
            ..AtoLock::default()
        };
        lock.features.declared = vec![FeatureName::Known(KnownFeature::Identity)];
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.3"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "driver": "deno"}),
        );
        lock.binding
            .entries
            .insert("host_port".to_string(), json!(3000));
        lock.policy
            .entries
            .insert("network".to_string(), json!({"mode": "deny"}));
        lock.attestations
            .entries
            .insert("last_run".to_string(), json!({"status": "ok"}));
        lock.signatures.push(LockSignature {
            kind: "opaque".to_string(),
            payload: BTreeMap::from([("blob".to_string(), json!("abc"))]),
        });
        lock
    }

    fn persisted_sample_lock() -> AtoLock {
        let mut lock = sample_lock();
        recompute_lock_id(&mut lock).expect("compute lock_id");
        lock
    }

    #[test]
    fn parses_delivery_environment_from_contract_install() {
        let mut lock = sample_lock();
        lock.contract.entries.insert(
            "delivery".to_string(),
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
                        "target": "desktop",
                        "services": [
                            {
                                "name": "ollama",
                                "from": "dependency:ollama",
                                "lifecycle": "managed",
                                "healthcheck": {
                                    "kind": "http",
                                    "url": "http://127.0.0.1:11434/api/tags"
                                }
                            },
                            {
                                "name": "opencode",
                                "from": "dependency:opencode",
                                "lifecycle": "on-demand",
                                "depends_on": ["ollama"]
                            }
                        ],
                        "bootstrap": {
                            "requires_personalization": true,
                            "model_tiers": ["fast", "balanced", "fallback"]
                        },
                        "repair": {
                            "actions": ["restart-services", "rewrite-config"]
                        }
                    }
                },
                "projection": {}
            }),
        );

        let environment = delivery_environment(&lock)
            .expect("parse delivery environment")
            .expect("environment present");

        assert_eq!(environment.strategy, "ato-managed");
        assert_eq!(environment.target.as_deref(), Some("desktop"));
        assert_eq!(environment.services.len(), 2);
        assert_eq!(environment.services[0].name, "ollama");
        assert_eq!(environment.services[1].depends_on, vec!["ollama"]);
        assert_eq!(
            environment.bootstrap.expect("bootstrap").model_tiers,
            vec!["fast", "balanced", "fallback"]
        );
    }

    #[test]
    fn round_trip_parse_and_serialize_schema_v1() {
        let lock = persisted_sample_lock();
        let pretty = to_pretty_json(&lock).expect("pretty json");
        let parsed = load_unvalidated_from_str(&pretty).expect("parse lock");
        assert_eq!(parsed.schema_version, ATO_LOCK_SCHEMA_VERSION);
        assert!(validate_persisted_strict(&parsed).is_ok());
    }

    #[test]
    fn canonical_projection_is_deterministic_across_field_order_and_whitespace() {
        let left = r#"{
            "schema_version": 1,
            "resolution": {"runtime": {"kind": "deno", "version": "2.1.3"}},
            "contract": {"process": {"driver": "deno", "entrypoint": "main.ts"}}
        }"#;
        let right = r#"{"contract":{"process":{"entrypoint":"main.ts","driver":"deno"}},"resolution":{"runtime":{"version":"2.1.3","kind":"deno"}},"schema_version":1}"#;

        let left_lock = load_unvalidated_from_str(left).expect("left parse");
        let right_lock = load_unvalidated_from_str(right).expect("right parse");

        assert_eq!(
            canonical_projection_bytes(&left_lock).expect("left bytes"),
            canonical_projection_bytes(&right_lock).expect("right bytes")
        );
        assert_eq!(
            compute_lock_id(&left_lock).expect("left lock_id"),
            compute_lock_id(&right_lock).expect("right lock_id")
        );
    }

    #[test]
    fn mutable_fields_do_not_change_lock_id() {
        let lock = persisted_sample_lock();
        let baseline = compute_lock_id(&lock).expect("baseline lock_id");

        let mut mutated = lock.clone();
        mutated.generated_at = Some("2026-03-26T00:00:00Z".to_string());
        mutated.features.required_for_execution =
            vec![FeatureName::Unknown("future_gate".to_string())];
        mutated
            .binding
            .entries
            .insert("host_port".to_string(), json!(4321));
        mutated
            .policy
            .entries
            .insert("mode".to_string(), json!("allow"));
        mutated
            .attestations
            .entries
            .insert("approval".to_string(), json!(true));
        mutated.signatures.push(LockSignature {
            kind: "second".to_string(),
            payload: BTreeMap::new(),
        });

        assert_eq!(
            baseline,
            compute_lock_id(&mutated).expect("mutated lock_id")
        );
    }

    #[test]
    fn resolution_or_contract_changes_lock_id() {
        let lock = persisted_sample_lock();
        let baseline = compute_lock_id(&lock).expect("baseline lock_id");

        let mut resolution_mutated = lock.clone();
        resolution_mutated.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "version": "2.1.4"}),
        );
        assert_ne!(
            baseline,
            compute_lock_id(&resolution_mutated).expect("resolution lock_id")
        );

        let mut contract_mutated = lock.clone();
        contract_mutated.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "server.ts", "driver": "deno"}),
        );
        assert_ne!(
            baseline,
            compute_lock_id(&contract_mutated).expect("contract lock_id")
        );
    }

    #[test]
    fn lock_id_field_itself_does_not_affect_recompute() {
        let mut lock = persisted_sample_lock();
        let baseline = compute_lock_id(&lock).expect("baseline lock_id");
        lock.lock_id = Some(LockId::new(
            "blake3:0000000000000000000000000000000000000000000000000000000000000000",
        ));
        assert_eq!(
            baseline,
            compute_lock_id(&lock).expect("recomputed lock_id")
        );
    }

    #[test]
    fn persisted_validation_rejects_missing_or_malformed_lock_id() {
        let missing = sample_lock();
        let missing_errors =
            validate_persisted_strict(&missing).expect_err("missing lock_id must fail");
        assert!(missing_errors
            .iter()
            .any(|error| matches!(error, AtoLockValidationError::MissingLockId)));

        let mut malformed = sample_lock();
        malformed.lock_id = Some(LockId::new("sha256:abcd"));
        let malformed_errors =
            validate_persisted_strict(&malformed).expect_err("malformed lock_id must fail");
        assert!(malformed_errors.iter().any(|error| {
            matches!(error, AtoLockValidationError::MalformedLockId(_))
                || matches!(error, AtoLockValidationError::LockIdMismatch { .. })
        }));
    }

    #[test]
    fn strict_validation_handles_unknown_and_required_features() {
        let mut unknown_required = persisted_sample_lock();
        unknown_required.features.required_for_execution =
            vec![FeatureName::Unknown("future_gate".to_string())];
        let errors = validate_persisted_strict(&unknown_required)
            .expect_err("unknown required feature must fail");
        assert!(errors.iter().any(|error| {
            matches!(error, AtoLockValidationError::UnknownRequiredFeature(value) if value == "future_gate")
        }));

        let mut unknown_declared = persisted_sample_lock();
        unknown_declared.features.declared = vec![FeatureName::Unknown("preview_only".to_string())];
        let strict_errors = validate_structural_strict(&unknown_declared)
            .expect_err("strict declared unknown feature must fail");
        assert!(strict_errors.iter().any(|error| {
            matches!(error, AtoLockValidationError::UnknownDeclaredFeature(value) if value == "preview_only")
        }));
        assert!(validate_structural_non_strict(&unknown_declared).is_ok());

        let mut recognized_but_unimplemented = persisted_sample_lock();
        recognized_but_unimplemented.features.required_for_execution =
            vec![FeatureName::Known(KnownFeature::Identity)];
        let unsupported_errors = validate_persisted_strict(&recognized_but_unimplemented)
            .expect_err("recognized but unsupported required feature must fail");
        assert!(unsupported_errors.iter().any(|error| {
            matches!(error, AtoLockValidationError::UnsupportedRequiredFeature(value) if value == "identity")
        }));
    }

    #[test]
    fn unresolved_marker_validation_is_fail_closed() {
        let mut lock = persisted_sample_lock();
        lock.contract.unresolved = vec![UnresolvedValue {
            field: Some("contract.process".to_string()),
            reason: UnresolvedReason::Unknown("future_reason".to_string()),
            detail: None,
            candidates: Vec::new(),
        }];
        let errors =
            validate_structural_strict(&lock).expect_err("unknown unresolved reason must fail");
        assert!(errors.iter().any(|error| {
            matches!(error, AtoLockValidationError::UnknownUnresolvedReason(value) if value == "future_reason")
        }));

        let mut ambiguity = persisted_sample_lock();
        ambiguity.resolution.unresolved = vec![UnresolvedValue {
            field: Some("resolution.runtime".to_string()),
            reason: UnresolvedReason::Ambiguity,
            detail: Some("multiple candidates".to_string()),
            candidates: Vec::new(),
        }];
        let ambiguity_errors = validate_structural_strict(&ambiguity)
            .expect_err("ambiguity without candidates must fail");
        assert!(ambiguity_errors
            .iter()
            .any(|error| matches!(error, AtoLockValidationError::AmbiguityRequiresCandidates)));

        let non_strict_unknown = validate_structural_non_strict(&lock)
            .expect_err("unknown unresolved reason remains structurally invalid");
        assert!(non_strict_unknown.iter().any(|error| {
            matches!(error, AtoLockValidationError::UnknownUnresolvedReason(value) if value == "future_reason")
        }));
    }

    #[test]
    fn write_and_load_path_round_trip() {
        let lock = persisted_sample_lock();
        let file = NamedTempFile::new().expect("temp file");
        write_pretty_to_path(&lock, file.path()).expect("write pretty lock");
        let parsed = load_unvalidated_from_path(file.path()).expect("read pretty lock");
        assert!(validate_persisted_strict(&parsed).is_ok());
    }

    #[test]
    fn recompute_then_persisted_validation_is_the_intended_draft_path() {
        let mut draft = sample_lock();
        assert!(validate_structural_strict(&draft).is_ok());
        assert!(validate_persisted_strict(&draft).is_err());

        recompute_lock_id(&mut draft).expect("recompute lock_id");

        assert!(validate_persisted_strict(&draft).is_ok());
    }

    #[test]
    fn closure_normalization_keeps_lock_id_stable_across_legacy_and_normalized_shapes() {
        let mut legacy = AtoLock::default();
        legacy.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "dist", "driver": "static"}),
        );
        legacy.resolution.entries.insert(
            "closure".to_string(),
            json!({"status": "complete", "inputs": []}),
        );

        let mut normalized = legacy.clone();
        normalized.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "runtime_closure", "status": "complete", "inputs": []}),
        );

        assert_eq!(
            compute_lock_id(&legacy).expect("legacy lock_id"),
            compute_lock_id(&normalized).expect("normalized lock_id")
        );
    }

    #[test]
    fn standard_signature_payload_matches_canonical_projection_bytes() {
        let lock = persisted_sample_lock();
        assert_eq!(
            canonical_signature_payload_bytes(&lock).expect("signature payload"),
            canonical_projection_bytes(&lock).expect("canonical bytes")
        );
    }

    #[test]
    fn canonical_identity_helpers_report_expected_sections() {
        assert!(is_canonical_identity_section("schema_version"));
        assert!(is_canonical_identity_section("resolution"));
        assert!(is_canonical_identity_section("contract"));
        assert!(!is_canonical_identity_section("binding"));
        assert!(!is_canonical_identity_section("policy"));
        assert!(!is_canonical_identity_section("attestations"));
        assert!(!is_canonical_identity_section("signatures"));
        assert!(CANONICAL_IDENTITY_EXCLUDED_SECTIONS.contains(&"binding"));
        assert!(CANONICAL_IDENTITY_EXCLUDED_SECTIONS.contains(&"policy"));
        assert!(CANONICAL_IDENTITY_EXCLUDED_SECTIONS.contains(&"attestations"));
        assert!(CANONICAL_IDENTITY_EXCLUDED_SECTIONS.contains(&"signatures"));
    }

    #[test]
    fn structural_validation_accepts_native_delivery_contract() {
        let mut lock = sample_lock();
        // source-derivation delivery requires the resolution.closure block
        // (kind=build_closure, status=complete) plus inputs and a fully
        // populated build_environment to be present.
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({
                "kind": "build_closure",
                "status": "complete",
                "inputs": [],
                "build_environment": {
                    "host_target": "darwin/arm64",
                    "toolchains": [],
                    "package_managers": [],
                    "sdks": [],
                    "helper_tools": []
                }
            }),
        );
        lock.contract.entries.insert(
            "delivery".to_string(),
            json!({
                "mode": "source-derivation",
                "artifact": {
                    "kind": "desktop-native",
                    "framework": "tauri",
                    "target": "darwin/arm64",
                    "path": "dist/MyApp.app",
                    "canonical_build_input": false,
                    "provenance_limited": false,
                    "reproducibility": "closure-tracked-build"
                },
                "build": {
                    "kind": "native-delivery",
                    "requires_build_closure": true,
                    "closure_status": "complete"
                },
                "finalize": {
                    "tool": "codesign",
                    "args": ["--deep", "--force"],
                    "host_local": true
                },
                "install": {
                    "kind": "local-derivation",
                    "host_local": true,
                    "requires_local_derivation": true
                },
                "projection": {
                    "kind": "launcher-surface",
                    "host_local": true
                }
            }),
        );

        assert!(validate_structural_strict(&lock).is_ok());
    }

    #[test]
    fn structural_validation_rejects_invalid_native_delivery_contract() {
        let mut lock = sample_lock();
        lock.contract.entries.insert(
            "delivery".to_string(),
            json!({
                "mode": "artifact-import",
                "artifact": {
                    "kind": "desktop-native",
                    "canonical_build_input": false,
                    "provenance_limited": false
                },
                "install": {
                    "kind": "local-derivation",
                    "host_local": true,
                    "requires_local_derivation": true
                },
                "projection": {
                    "kind": "launcher-surface",
                    "host_local": true
                }
            }),
        );

        let errors = validate_structural_strict(&lock).expect_err("delivery should be invalid");
        assert!(errors.iter().any(|error| {
            error
                .to_string()
                .contains("provenance_limited must be true for artifact-import")
        }));
    }
}
