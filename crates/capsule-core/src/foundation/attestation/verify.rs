//! Verification of A2 attestation envelopes.
//!
//! `verify_envelope` re-canonicalizes the statement, prepends the frozen
//! signing prefix, and runs Ed25519 verification with the trust root's
//! public key. The trust root format is intentionally minimal: a single
//! base64-encoded 32-byte public key and a key id matching the envelope's
//! claim.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature as DalekSignature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::sign::derive_key_id;
use super::types::{AttestationEnvelope, AttestationStatement};

#[derive(Debug, Error)]
pub enum AttestationVerifyError {
    #[error("envelope claims algorithm `{0}`; only `ato-attestation-v1` is supported")]
    AlgorithmMismatch(String),
    #[error("envelope schema version {0} not supported")]
    SchemaVersionMismatch(u32),
    #[error("signature algorithm `{0}` not supported (only ed25519)")]
    SignatureAlgorithmMismatch(String),
    #[error("envelope public key does not match the trust root")]
    PublicKeyMismatch,
    #[error("derived key id `{derived}` does not match envelope claim `{claimed}`")]
    KeyIdMismatch { derived: String, claimed: String },
    #[error("base64 decode failed: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("public key must be 32 bytes")]
    InvalidPublicKey,
    #[error("signature must be 64 bytes")]
    InvalidSignatureLength,
    #[error("ed25519 verification failed")]
    BadSignature,
    #[error("statement canonicalization failed: {0}")]
    Canonicalize(#[from] serde_json::Error),
}

/// Local trust root: one Ed25519 public key the operator has accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRoot {
    pub schema_version: u32,
    pub key_id: String,
    pub public_key_b64: String,
    /// Optional human-readable label (operator-assigned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl TrustRoot {
    pub fn new(public_key_bytes: &[u8; 32], label: Option<String>) -> Self {
        Self {
            schema_version: 1,
            key_id: derive_key_id(public_key_bytes),
            public_key_b64: BASE64.encode(public_key_bytes),
            label,
        }
    }

    /// Returns the raw public-key bytes.
    pub fn public_key_bytes(&self) -> Result<[u8; 32], AttestationVerifyError> {
        let bytes = BASE64.decode(&self.public_key_b64)?;
        bytes
            .try_into()
            .map_err(|_| AttestationVerifyError::InvalidPublicKey)
    }
}

/// Outcome of a successful verification.
#[derive(Debug, Clone)]
pub struct VerifiedAttestation<'a> {
    pub statement: &'a AttestationStatement,
    pub key_id: &'a str,
}

/// Verifies `envelope` against `trust_root`. Returns `Ok` only when the
/// envelope is well-formed, the trust root matches, and the Ed25519
/// signature checks out.
pub fn verify_envelope<'a>(
    envelope: &'a AttestationEnvelope,
    trust_root: &TrustRoot,
) -> Result<VerifiedAttestation<'a>, AttestationVerifyError> {
    if envelope.statement.algorithm != super::types::ATTESTATION_ALGORITHM {
        return Err(AttestationVerifyError::AlgorithmMismatch(
            envelope.statement.algorithm.clone(),
        ));
    }
    if envelope.schema_version != super::types::ATTESTATION_SCHEMA_VERSION {
        return Err(AttestationVerifyError::SchemaVersionMismatch(
            envelope.schema_version,
        ));
    }
    if envelope.signature.algorithm != "ed25519" {
        return Err(AttestationVerifyError::SignatureAlgorithmMismatch(
            envelope.signature.algorithm.clone(),
        ));
    }

    // 1. Trust root must own the same public key the envelope claims.
    if envelope.signature.public_key_b64 != trust_root.public_key_b64 {
        return Err(AttestationVerifyError::PublicKeyMismatch);
    }

    // 2. The claimed key_id must derive from the embedded public key,
    //    so a tampered envelope cannot point at a trusted key while
    //    carrying a different actual public key.
    let pub_bytes = trust_root.public_key_bytes()?;
    let derived = derive_key_id(&pub_bytes);
    if derived != envelope.signature.key_id || derived != trust_root.key_id {
        return Err(AttestationVerifyError::KeyIdMismatch {
            derived,
            claimed: envelope.signature.key_id.clone(),
        });
    }

    // 3. Run Ed25519 verification on the canonical signing input.
    let signature_bytes = BASE64.decode(&envelope.signature.signature_b64)?;
    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| AttestationVerifyError::InvalidSignatureLength)?;
    let signature = DalekSignature::from_bytes(&signature_array);

    let verifying = VerifyingKey::from_bytes(&pub_bytes)
        .map_err(|_| AttestationVerifyError::InvalidPublicKey)?;

    let signing_input = envelope.statement.signing_input()?;
    verifying
        .verify(&signing_input, &signature)
        .map_err(|_| AttestationVerifyError::BadSignature)?;

    Ok(VerifiedAttestation {
        statement: &envelope.statement,
        key_id: &envelope.signature.key_id,
    })
}

/// Convenience: returns the SHA-256 of a public key.
#[allow(dead_code)]
fn fingerprint(public_key: &[u8]) -> String {
    hex::encode(Sha256::digest(public_key))
}

#[cfg(test)]
mod tests {
    use super::super::sign::{generate_keypair, sign_envelope, AttestationKey};
    use super::super::types::*;
    use super::*;

    fn statement() -> AttestationStatement {
        AttestationStatement::new(
            AttestationSubject::for_blob(
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
            AttestationPredicate {
                builder_id: "ato-cli@verify-test".to_string(),
                source: None,
                source_tree_hash: None,
                derivation_hash: None,
                policy: PolicySnapshot::default(),
                freeze: None,
            },
            "2026-05-03T00:00:00Z",
        )
    }

    #[test]
    fn verify_round_trip_succeeds() {
        let key = generate_keypair();
        let envelope = sign_envelope(statement(), &key).unwrap();
        let trust_root = TrustRoot::new(&key.public_key_bytes(), Some("self".to_string()));
        let verified = verify_envelope(&envelope, &trust_root).unwrap();
        assert_eq!(verified.key_id, key.key_id());
    }

    #[test]
    fn verify_rejects_unknown_trust_root() {
        let key_a = AttestationKey::from_secret_bytes(&[1u8; 32]).unwrap();
        let key_b = AttestationKey::from_secret_bytes(&[2u8; 32]).unwrap();
        let envelope = sign_envelope(statement(), &key_a).unwrap();
        let unrelated_trust = TrustRoot::new(&key_b.public_key_bytes(), None);
        let err = verify_envelope(&envelope, &unrelated_trust).unwrap_err();
        assert!(matches!(err, AttestationVerifyError::PublicKeyMismatch));
    }

    #[test]
    fn verify_rejects_tampered_subject() {
        let key = generate_keypair();
        let mut envelope = sign_envelope(statement(), &key).unwrap();
        envelope.statement.subject.hash =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string();
        let trust_root = TrustRoot::new(&key.public_key_bytes(), None);
        let err = verify_envelope(&envelope, &trust_root).unwrap_err();
        assert!(matches!(err, AttestationVerifyError::BadSignature));
    }

    #[test]
    fn verify_rejects_swapped_key_id() {
        let key = generate_keypair();
        let mut envelope = sign_envelope(statement(), &key).unwrap();
        envelope.signature.key_id = "key:0000000000000000".to_string();
        let trust_root = TrustRoot::new(&key.public_key_bytes(), None);
        let err = verify_envelope(&envelope, &trust_root).unwrap_err();
        assert!(matches!(err, AttestationVerifyError::KeyIdMismatch { .. }));
    }

    #[test]
    fn verify_rejects_wrong_algorithm() {
        let key = generate_keypair();
        let mut envelope = sign_envelope(statement(), &key).unwrap();
        envelope.statement.algorithm = "blake3-attestation-v9".to_string();
        let trust_root = TrustRoot::new(&key.public_key_bytes(), None);
        let err = verify_envelope(&envelope, &trust_root).unwrap_err();
        assert!(matches!(err, AttestationVerifyError::AlgorithmMismatch(_)));
    }
}
