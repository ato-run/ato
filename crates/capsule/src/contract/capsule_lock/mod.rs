mod canonicalize;
mod closure;
mod execution;
mod hash;
pub mod oci;
mod schema;
mod validate;

use std::fs;
use std::path::Path;

/// capsule.lock v1 foundation module.
///
/// v1 intentionally uses one Rust model for both serde and in-memory draft
/// handling, while keeping canonical lock identity in a separate projection.
/// Load, validation, lock_id computation, and serialization are split so later
/// input-resolver and import flows can work with draft locks without being
/// forced through persisted artifact validation too early.
pub use canonicalize::{
    CANONICAL_IDENTITY_EXCLUDED_SECTIONS, CANONICAL_IDENTITY_INCLUDED_SECTIONS,
    CanonicalLockProjection, CanonicalSignatureProjection, canonical_identity_projection,
    canonical_projection, canonical_signature_projection, is_canonical_identity_section,
};
pub use closure::{
    ClosureInfo, closure_info, compute_closure_digest, normalize_closure_value,
    normalize_lock_closure, normalize_resolution_closure_entries, validate_closure_value,
};
pub use execution::{
    LockExecutionError, verify_environment_values, verify_execution_envelope,
    verify_lock_execution, verify_lock_program_identity,
};
pub use hash::{
    canonical_document_bytes, canonical_projection_bytes, canonical_signature_payload_bytes,
    compute_lock_id, recompute_lock_id,
};
pub use oci::{
    OciImageLockEntry, OciImportEntry, OciLockReadResult, OciLockReadWarning, OciLockSource,
    OciMainLockError, construct_resolved_ref_from_sidecar, oci_images_from_main_lock,
    oci_imports_from_main_lock, parse_platform_str as parse_oci_platform_str, read_oci_lock,
    upsert_oci_lock_facts, write_oci_facts_to_main_lock,
};
pub use schema::{
    AttestationsSection, BindingSection, CAPSULE_LOCK_SCHEMA_VERSION, CapsuleLock, ContractSection,
    DeliveryBootstrap, DeliveryEnvironment, DeliveryHealthcheck, DeliveryRepair, DeliveryService,
    FeatureName, KnownFeature, LockEnvironmentValue, LockFeatures, LockId, LockLaunchSection,
    LockSignature, PolicySection, ResolutionSection, UnresolvedReason, UnresolvedValue,
    delivery_environment, parse_delivery_environment_value,
};
pub use validate::{
    CapsuleLockValidationError, ValidationMode, validate_persisted, validate_structural,
};

use crate::contract::lockfile::lockfile_support::write_atomic_bytes_with_os_lock;
use crate::error::{CapsuleError, Result};

/// Parses capsule.lock JSON without applying any validation.
pub fn load_unvalidated_from_str(raw: &str) -> Result<CapsuleLock> {
    serde_json::from_str(raw)
        .map_err(|err| CapsuleError::Config(format!("Failed to parse capsule.lock: {err}")))
}

/// Reads capsule.lock JSON from disk without applying any validation.
pub fn load_unvalidated_from_path(path: &Path) -> Result<CapsuleLock> {
    let raw = fs::read_to_string(path)
        .map_err(|err| CapsuleError::Config(format!("Failed to read {}: {err}", path.display())))?;
    load_unvalidated_from_str(&raw)
}

/// Fail-closed re-derivation of every trusted section carried by a persisted
/// lock: the embedded execution section (D2 `execution_contract` envelope + D5
/// `launch.environment`) and the ADR-014 Capsule Program identity states
/// (`program_identity` envelope + the execution envelope's parent claim, the
/// §5 four-state matrix).
///
/// This is the standard-path chokepoint the read/write boundary runs so a
/// tampered `execution_id`, a bad D5 value payload, a tampered
/// `capsule_program_id`, or an inconsistent/orphan parent claim can never pass
/// strict validation or be persisted. Launch-time re-read / OCI wiring is
/// deferred to a later PR; only the persisted read/write boundary is enforced
/// here.
fn verify_lock_trust_boundary(lock: &CapsuleLock) -> Result<()> {
    verify_lock_execution(lock).map_err(|err| {
        CapsuleError::Config(format!("capsule.lock execution verification failed: {err}"))
    })?;
    verify_lock_program_identity(lock).map_err(|err| {
        CapsuleError::Config(format!(
            "capsule.lock program identity verification failed: {err}"
        ))
    })
}

/// Loads a persisted lock from JSON and verifies it fully: strict persisted
/// validation PLUS re-derivation of any embedded execution section. This is the
/// sanctioned entry for a persisted lock carrying an execution section.
pub fn load_verified_from_str(raw: &str) -> Result<CapsuleLock> {
    let lock = load_unvalidated_from_str(raw)?;
    validate_persisted_strict(&lock).map_err(validation_errors_to_capsule_error)?;
    verify_lock_trust_boundary(&lock)?;
    Ok(lock)
}

/// Reads a persisted lock from disk and verifies it fully (see
/// [`load_verified_from_str`]).
pub fn load_verified_from_path(path: &Path) -> Result<CapsuleLock> {
    let raw = fs::read_to_string(path)
        .map_err(|err| CapsuleError::Config(format!("Failed to read {}: {err}", path.display())))?;
    load_verified_from_str(&raw)
}

/// Validates a persisted lock's IDENTITY under strict mode: schema version,
/// structural shape, feature encoding, and `lock_id` (presence, format, and
/// match against the canonical projection).
///
/// IDENTITY-ONLY: this does NOT verify the lock's embedded execution section. A
/// lock carrying a tampered `execution_id`, or a tampered/secret D5
/// `launch.environment` value payload, still returns `Ok(())` from here — those
/// fields are excluded from lock identity by design (see `CanonicalLockProjection`).
/// Any lock that may carry an `execution_contract` MUST be read through the
/// trusted entrypoints [`load_verified_from_str`] / [`load_verified_from_path`],
/// which run this strict identity validation AND then re-derive the execution
/// section fail-closed (`verify_lock_trust_boundary`). Call this directly only
/// for identity-only checks where execution trust is irrelevant.
pub fn validate_persisted_strict(
    lock: &CapsuleLock,
) -> std::result::Result<(), Vec<CapsuleLockValidationError>> {
    validate_persisted(lock, ValidationMode::Strict)
}

/// Validates a persisted lock under non-strict mode.
pub fn validate_persisted_non_strict(
    lock: &CapsuleLock,
) -> std::result::Result<(), Vec<CapsuleLockValidationError>> {
    validate_persisted(lock, ValidationMode::NonStrict)
}

/// Validates a draft or persisted lock structurally under strict mode.
pub fn validate_structural_strict(
    lock: &CapsuleLock,
) -> std::result::Result<(), Vec<CapsuleLockValidationError>> {
    validate_structural(lock, ValidationMode::Strict)
}

/// Validates a draft or persisted lock structurally under non-strict mode.
pub fn validate_structural_non_strict(
    lock: &CapsuleLock,
) -> std::result::Result<(), Vec<CapsuleLockValidationError>> {
    validate_structural(lock, ValidationMode::NonStrict)
}

/// Pretty-serializes a durable capsule.lock artifact.
///
/// This preserves generated_at as stored on the model and does not normalize
/// its textual representation beyond RFC3339 validation. lock_id is recomputed
/// before serialization and persisted validation must pass.
pub fn to_pretty_json(lock: &CapsuleLock) -> Result<String> {
    let mut persisted = lock.clone();
    normalize_lock_closure(&mut persisted)?;
    recompute_lock_id(&mut persisted)?;
    validate_persisted_strict(&persisted).map_err(validation_errors_to_capsule_error)?;
    verify_lock_trust_boundary(&persisted)?;
    serde_json::to_string_pretty(&persisted)
        .map_err(|err| CapsuleError::Config(format!("Failed to serialize capsule.lock: {err}")))
}

/// Writes a durable pretty capsule.lock artifact after recomputing lock_id.
///
/// The write is atomic and OS-locked, through the same
/// [`write_atomic_bytes_with_os_lock`] the legacy lockfile writer already uses:
/// temp file in the destination directory, `fsync`, `rename`, then `fsync` of
/// the parent. A plain `write` here would be a real hazard rather than a
/// theoretical one — every reader of this file goes through
/// [`load_verified_from_path`], which validates `lock_id` against the canonical
/// projection, so a torn write does not read back as a slightly-stale lock: it
/// reads back as a lock that fails verification, and the workspace loses its
/// identity until someone regenerates it. Renaming a fully-written temp file
/// means a concurrent reader sees either the whole old lock or the whole new
/// one, and a crash mid-write leaves the old one intact.
pub fn write_pretty_to_path(lock: &CapsuleLock, path: &Path) -> Result<()> {
    let raw = to_pretty_json(lock)?;
    write_atomic_bytes_with_os_lock(path, raw.as_bytes(), "capsule.lock", CapsuleError::Config)
}

/// Returns canonical persisted bytes for a durable capsule.lock artifact.
pub fn write_canonical_to_vec(lock: &CapsuleLock) -> Result<Vec<u8>> {
    let mut persisted = lock.clone();
    normalize_lock_closure(&mut persisted)?;
    recompute_lock_id(&mut persisted)?;
    validate_persisted_strict(&persisted).map_err(validation_errors_to_capsule_error)?;
    verify_lock_trust_boundary(&persisted)?;
    serde_jcs::to_vec(&persisted).map_err(|err| {
        CapsuleError::Config(format!("Failed to canonicalize capsule.lock JSON: {err}"))
    })
}

/// Verifies that an existing persisted lock_id matches the canonical projection.
pub fn verify_lock_id(lock: &CapsuleLock) -> Result<()> {
    validate_persisted_strict(lock).map_err(validation_errors_to_capsule_error)?;
    Ok(())
}

fn validation_errors_to_capsule_error(errors: Vec<CapsuleLockValidationError>) -> CapsuleError {
    let message = errors
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    CapsuleError::Config(format!("capsule.lock validation failed: {message}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::validate::CapsuleLockValidationError;
    use super::{
        CANONICAL_IDENTITY_EXCLUDED_SECTIONS, CAPSULE_LOCK_SCHEMA_VERSION, CapsuleLock,
        FeatureName, KnownFeature, LockId, LockSignature, UnresolvedReason, UnresolvedValue,
        canonical_projection_bytes, canonical_signature_payload_bytes, compute_lock_id,
        delivery_environment, is_canonical_identity_section, load_unvalidated_from_path,
        load_unvalidated_from_str, recompute_lock_id, to_pretty_json, validate_persisted_strict,
        validate_structural_non_strict, validate_structural_strict, write_pretty_to_path,
    };

    fn sample_lock() -> CapsuleLock {
        let mut lock = CapsuleLock {
            generated_at: Some("2026-03-25T00:00:00Z".to_string()),
            ..CapsuleLock::default()
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

    fn persisted_sample_lock() -> CapsuleLock {
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
        assert_eq!(parsed.schema_version, CAPSULE_LOCK_SCHEMA_VERSION);
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
        assert!(
            missing_errors
                .iter()
                .any(|error| matches!(error, CapsuleLockValidationError::MissingLockId))
        );

        let mut malformed = sample_lock();
        malformed.lock_id = Some(LockId::new("sha256:abcd"));
        let malformed_errors =
            validate_persisted_strict(&malformed).expect_err("malformed lock_id must fail");
        assert!(malformed_errors.iter().any(|error| {
            matches!(error, CapsuleLockValidationError::MalformedLockId(_))
                || matches!(error, CapsuleLockValidationError::LockIdMismatch { .. })
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
            matches!(error, CapsuleLockValidationError::UnknownRequiredFeature(value) if value == "future_gate")
        }));

        let mut unknown_declared = persisted_sample_lock();
        unknown_declared.features.declared = vec![FeatureName::Unknown("preview_only".to_string())];
        let strict_errors = validate_structural_strict(&unknown_declared)
            .expect_err("strict declared unknown feature must fail");
        assert!(strict_errors.iter().any(|error| {
            matches!(error, CapsuleLockValidationError::UnknownDeclaredFeature(value) if value == "preview_only")
        }));
        assert!(validate_structural_non_strict(&unknown_declared).is_ok());

        let mut recognized_but_unimplemented = persisted_sample_lock();
        recognized_but_unimplemented.features.required_for_execution =
            vec![FeatureName::Known(KnownFeature::Identity)];
        let unsupported_errors = validate_persisted_strict(&recognized_but_unimplemented)
            .expect_err("recognized but unsupported required feature must fail");
        assert!(unsupported_errors.iter().any(|error| {
            matches!(error, CapsuleLockValidationError::UnsupportedRequiredFeature(value) if value == "identity")
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
            matches!(error, CapsuleLockValidationError::UnknownUnresolvedReason(value) if value == "future_reason")
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
        assert!(ambiguity_errors.iter().any(|error| matches!(
            error,
            CapsuleLockValidationError::AmbiguityRequiresCandidates
        )));

        let non_strict_unknown = validate_structural_non_strict(&lock)
            .expect_err("unknown unresolved reason remains structurally invalid");
        assert!(non_strict_unknown.iter().any(|error| {
            matches!(error, CapsuleLockValidationError::UnknownUnresolvedReason(value) if value == "future_reason")
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

    /// The write must not be observable half-done. A reader that catches a
    /// partial lock does not get a stale-but-valid one — `lock_id` is a hash of
    /// the document, so a truncated file fails verification and the workspace
    /// has no usable identity until someone regenerates it.
    ///
    /// This asserts the mechanism rather than racing a real reader: an atomic
    /// write publishes by `rename`, so the destination inode is REPLACED, while
    /// a plain `write` truncates the existing one in place. Holding the old
    /// file open across the write distinguishes the two — through the old
    /// handle the previous bytes are still whole.
    #[cfg(unix)]
    #[test]
    fn writing_a_lock_replaces_the_file_rather_than_truncating_it_in_place() {
        use std::fs;
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;

        use super::load_verified_from_path;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("capsule.lock");

        let first = persisted_sample_lock();
        write_pretty_to_path(&first, &path).expect("write first lock");
        let before = fs::read_to_string(&path).expect("read first lock");
        let before_inode = fs::metadata(&path).expect("stat first lock").ino();

        // An open handle to the ORIGINAL file, held across the second write.
        let mut held = fs::File::open(&path).expect("hold the first lock open");

        let mut second = sample_lock();
        second
            .resolution
            .entries
            .insert("runtime".to_string(), json!({"kind": "node"}));
        recompute_lock_id(&mut second).expect("compute lock_id");
        write_pretty_to_path(&second, &path).expect("write second lock");

        let mut through_old_handle = String::new();
        held.read_to_string(&mut through_old_handle)
            .expect("read through the held handle");
        assert_eq!(
            through_old_handle, before,
            "the previous lock must survive intact behind an open handle — an \
             in-place truncating write would have destroyed it"
        );
        assert_ne!(
            before_inode,
            fs::metadata(&path).expect("stat second lock").ino(),
            "an atomic publish renames a new file over the old one, so the \
             destination inode must change"
        );

        // And the published bytes are the NEW lock, fully verified.
        let reloaded = load_verified_from_path(&path).expect("load the published lock");
        assert_eq!(
            reloaded.resolution.entries["runtime"],
            json!({"kind": "node"})
        );
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
        let mut legacy = CapsuleLock::default();
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
