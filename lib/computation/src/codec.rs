//! Canonical identity codec and verified resolution for sealed computations.
//!
//! JCS bytes are the sole preimage for a [`ComputationRef`]. A
//! [`ResolvedComputation`] can only be created by decoding canonical bytes and
//! verifying their BLAKE3 digest.

#![forbid(unsafe_code)]

use crate::{
    Boundary, ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId, RoleId,
    SemanticsId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// Maximum canonical byte length accepted for one computation object.
pub const MAX_COMPUTATION_OBJECT_BYTES: u64 = 1024 * 1024;

/// A computation whose canonical encoding and identity have been verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedComputation {
    reference: ComputationRef,
    object: ComputationObject,
}

impl ResolvedComputation {
    /// Decodes canonical bytes and verifies that their digest equals `reference`.
    pub fn verify(reference: ComputationRef, canonical_bytes: &[u8]) -> Result<Self, CodecError> {
        let object = decode_computation_object(canonical_bytes)?;
        let actual = reference_for_bytes(canonical_bytes)?;
        if actual != reference {
            return Err(CodecError::IdentityMismatch {
                expected: reference,
                actual,
            });
        }
        Ok(Self { reference, object })
    }

    pub fn reference(&self) -> &ComputationRef {
        &self.reference
    }

    pub fn object(&self) -> &ComputationObject {
        &self.object
    }
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("computation object is {actual} bytes; maximum is {maximum}")]
    ObjectTooLarge { actual: u64, maximum: u64 },
    #[error("computation object JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("computation object identifier failed: {0}")]
    Identifier(#[from] crate::IdentifierError),
    #[error("computation object is not in its canonical JCS representation")]
    NonCanonical,
    #[error("computation object identity mismatch: expected {expected}, got {actual}")]
    IdentityMismatch {
        expected: ComputationRef,
        actual: ComputationRef,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputationObjectWire {
    semantics: String,
    boundary: BTreeMap<String, PortWire>,
    residual: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortWire {
    protocol: String,
    role: String,
}

pub fn encode_computation_object(object: &ComputationObject) -> Result<Vec<u8>, CodecError> {
    let boundary = object
        .boundary
        .iter()
        .map(|(port, definition)| {
            (
                port.as_str().to_owned(),
                PortWire {
                    protocol: definition.protocol.as_str().to_owned(),
                    role: definition.role.as_str().to_owned(),
                },
            )
        })
        .collect();
    let bytes = serde_jcs::to_vec(&ComputationObjectWire {
        semantics: object.semantics.as_str().to_owned(),
        boundary,
        residual: object.residual.as_str().to_owned(),
    })?;
    ensure_object_size(&bytes)?;
    Ok(bytes)
}

pub fn decode_computation_object(bytes: &[u8]) -> Result<ComputationObject, CodecError> {
    ensure_object_size(bytes)?;
    let wire: ComputationObjectWire = serde_json::from_slice(bytes)?;
    let object = ComputationObject {
        semantics: SemanticsId::parse(wire.semantics)?,
        boundary: decode_boundary(wire.boundary)?,
        residual: ContentRef::parse(wire.residual)?,
    };
    if encode_computation_object(&object)? != bytes {
        return Err(CodecError::NonCanonical);
    }
    Ok(object)
}

fn ensure_object_size(bytes: &[u8]) -> Result<(), CodecError> {
    let actual = bytes.len() as u64;
    if actual > MAX_COMPUTATION_OBJECT_BYTES {
        return Err(CodecError::ObjectTooLarge {
            actual,
            maximum: MAX_COMPUTATION_OBJECT_BYTES,
        });
    }
    Ok(())
}

pub fn computation_ref(object: &ComputationObject) -> Result<ComputationRef, CodecError> {
    reference_for_bytes(&encode_computation_object(object)?)
}

fn decode_boundary(boundary: BTreeMap<String, PortWire>) -> Result<Boundary, CodecError> {
    boundary
        .into_iter()
        .map(|(port, definition)| {
            Ok((
                PortId::parse(port)?,
                PortDef {
                    protocol: ProtocolId::parse(definition.protocol)?,
                    role: RoleId::parse(definition.role)?,
                },
            ))
        })
        .collect()
}

fn reference_for_bytes(bytes: &[u8]) -> Result<ComputationRef, CodecError> {
    Ok(ComputationRef::parse(format!(
        "blake3:{}",
        blake3::hash(bytes).to_hex()
    ))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ComputationObject {
        ComputationObject {
            semantics: SemanticsId::parse("capsule.legacy-v1@1").unwrap(),
            boundary: BTreeMap::from([(
                PortId::parse("terminal.main").unwrap(),
                PortDef {
                    protocol: ProtocolId::parse("ato.io.pty@1").unwrap(),
                    role: RoleId::parse("legacy-peer").unwrap(),
                },
            )]),
            residual: ContentRef::parse(format!("blake3:{}", "ab".repeat(32))).unwrap(),
        }
    }

    #[test]
    fn canonical_encoding_and_identity_match_golden_vector() {
        let bytes = encode_computation_object(&fixture()).unwrap();
        let expected_bytes =
            hex::decode(include_str!("../tests/vectors/computation_object_v1.jcs.hex").trim())
                .unwrap();
        let reference = computation_ref(&fixture()).unwrap();

        assert_eq!(bytes, expected_bytes);
        assert_eq!(
            reference.as_str(),
            include_str!("../tests/vectors/computation_object_v1.ref").trim()
        );
    }

    #[test]
    fn decode_rejects_valid_json_that_is_not_exact_jcs() {
        let bytes = br#"{ "semantics":"capsule.legacy-v1@1", "boundary":{}, "residual":"blake3:abababababababababababababababababababababababababababababababab" }"#;

        assert!(matches!(
            decode_computation_object(bytes),
            Err(CodecError::NonCanonical)
        ));
    }

    #[test]
    fn resolved_computation_rejects_unrelated_object() {
        let bytes = encode_computation_object(&fixture()).unwrap();
        let unrelated = ComputationRef::parse(format!("blake3:{}", "cd".repeat(32))).unwrap();

        assert!(matches!(
            ResolvedComputation::verify(unrelated, &bytes),
            Err(CodecError::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn decode_rejects_bytes_above_the_codec_limit_before_parsing() {
        let bytes = vec![b' '; MAX_COMPUTATION_OBJECT_BYTES as usize + 1];

        assert!(matches!(
            decode_computation_object(&bytes),
            Err(CodecError::ObjectTooLarge { .. })
        ));
    }

    #[test]
    fn resolved_computation_rejects_bytes_above_the_codec_limit() {
        let reference = ComputationRef::parse(format!("blake3:{}", "cd".repeat(32))).unwrap();
        let bytes = vec![b' '; MAX_COMPUTATION_OBJECT_BYTES as usize + 1];

        assert!(matches!(
            ResolvedComputation::verify(reference, &bytes),
            Err(CodecError::ObjectTooLarge { .. })
        ));
    }
}
