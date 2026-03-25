use chrono::DateTime;
use thiserror::Error;

use crate::ato_lock::hash::compute_lock_id;
use crate::ato_lock::schema::{
    AtoLock, FeatureName, KnownFeature, LockSignature, UnresolvedReason, UnresolvedValue,
    ATO_LOCK_SCHEMA_VERSION,
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

fn is_supported_feature(_feature: KnownFeature) -> bool {
    false
}
