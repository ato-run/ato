//! Ed25519 signing primitives for A2 attestations.
//!
//! Keeps key handling narrow and deterministic: keys are 32-byte secret +
//! 32-byte public, addressed by `key:<sha256-hex>` of the public key. JSON
//! serialization uses the same `key_type` / `public_key` / `secret_key`
//! shape as `security::signing::sign::StoredKeyRef` so existing keys can
//! be reused.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::types::{
    AttestationEnvelope, AttestationStatement, Signature, ATTESTATION_SCHEMA_VERSION,
};

#[derive(Debug, Error)]
pub enum AttestationKeyError {
    #[error("ed25519 secret key must be 32 bytes, got {0}")]
    InvalidSecretKeyLength(usize),
    #[error("ed25519 public key must be 32 bytes, got {0}")]
    InvalidPublicKeyLength(usize),
    #[error("base64 decode failed: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("attestation statement canonicalization failed: {0}")]
    Canonicalize(#[from] serde_json::Error),
}

/// On-disk persisted form of an attestation key.
///
/// Format mirrors `security::signing::sign::StoredKeyRef` so a single key
/// file can drive both legacy artifact signing and A2 attestation signing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAttestationKey {
    pub key_type: String,
    pub public_key: String,
    pub secret_key: String,
}

/// In-memory Ed25519 keypair plus its derived `key_id`.
pub struct AttestationKey {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    key_id: String,
}

impl AttestationKey {
    /// Builds a key from a 32-byte secret. The verifying half and key_id
    /// are derived deterministically.
    pub fn from_secret_bytes(secret: &[u8]) -> Result<Self, AttestationKeyError> {
        let array: [u8; 32] = secret
            .try_into()
            .map_err(|_| AttestationKeyError::InvalidSecretKeyLength(secret.len()))?;
        let signing_key = SigningKey::from_bytes(&array);
        let verifying_key = signing_key.verifying_key();
        let key_id = derive_key_id(verifying_key.as_bytes());
        Ok(Self {
            signing_key,
            verifying_key,
            key_id,
        })
    }

    /// Decodes a base64 secret into an [`AttestationKey`].
    pub fn from_secret_b64(secret_b64: &str) -> Result<Self, AttestationKeyError> {
        let bytes = BASE64.decode(secret_b64)?;
        Self::from_secret_bytes(&bytes)
    }

    /// Loads from the [`StoredAttestationKey`] JSON shape.
    pub fn from_stored(stored: &StoredAttestationKey) -> Result<Self, AttestationKeyError> {
        if stored.key_type != "ed25519" {
            return Err(AttestationKeyError::InvalidSecretKeyLength(0));
        }
        Self::from_secret_b64(&stored.secret_key)
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        *self.verifying_key.as_bytes()
    }

    pub fn public_key_b64(&self) -> String {
        BASE64.encode(self.verifying_key.as_bytes())
    }

    pub fn secret_key_b64(&self) -> String {
        BASE64.encode(self.signing_key.to_bytes())
    }

    /// Returns the JSON-serializable representation of the keypair.
    pub fn to_stored(&self) -> StoredAttestationKey {
        StoredAttestationKey {
            key_type: "ed25519".to_string(),
            public_key: self.public_key_b64(),
            secret_key: self.secret_key_b64(),
        }
    }
}

/// Generates a fresh Ed25519 keypair using the OS CSPRNG.
pub fn generate_keypair() -> AttestationKey {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let key_id = derive_key_id(verifying_key.as_bytes());
    AttestationKey {
        signing_key,
        verifying_key,
        key_id,
    }
}

/// Produces a signed [`AttestationEnvelope`] over `statement` using `key`.
///
/// The signature covers `ATTESTATION_SIGNING_PREFIX || canonical_bytes`
/// where `canonical_bytes` are the JCS encoding of the statement. Repeated
/// calls with the same key and statement produce identical signatures.
pub fn sign_envelope(
    statement: AttestationStatement,
    key: &AttestationKey,
) -> Result<AttestationEnvelope, AttestationKeyError> {
    let signing_input = statement.signing_input()?;
    let signature = key.signing_key.sign(&signing_input);
    Ok(AttestationEnvelope {
        schema_version: ATTESTATION_SCHEMA_VERSION,
        statement,
        signature: Signature {
            algorithm: "ed25519".to_string(),
            key_id: key.key_id.clone(),
            signature_b64: BASE64.encode(signature.to_bytes()),
            public_key_b64: key.public_key_b64(),
        },
    })
}

pub(crate) fn derive_key_id(public_key_bytes: &[u8]) -> String {
    let digest = Sha256::digest(public_key_bytes);
    format!("key:{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    use super::super::types::*;
    use super::*;

    fn statement() -> AttestationStatement {
        AttestationStatement::new(
            AttestationSubject::for_blob(
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
            AttestationPredicate {
                builder_id: "ato-cli@test".to_string(),
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
    fn key_id_is_deterministic() {
        let secret = [7u8; 32];
        let a = AttestationKey::from_secret_bytes(&secret).unwrap();
        let b = AttestationKey::from_secret_bytes(&secret).unwrap();
        assert_eq!(a.key_id(), b.key_id());
        assert!(a.key_id().starts_with("key:"));
    }

    #[test]
    fn sign_envelope_is_deterministic_for_same_inputs() {
        let key = AttestationKey::from_secret_bytes(&[1u8; 32]).unwrap();
        let first = sign_envelope(statement(), &key).unwrap();
        let second = sign_envelope(statement(), &key).unwrap();
        assert_eq!(
            first.signature.signature_b64,
            second.signature.signature_b64
        );
        assert_eq!(first.signature.key_id, key.key_id());
    }

    #[test]
    fn distinct_keys_produce_distinct_signatures() {
        let key_a = AttestationKey::from_secret_bytes(&[1u8; 32]).unwrap();
        let key_b = AttestationKey::from_secret_bytes(&[2u8; 32]).unwrap();
        let envelope_a = sign_envelope(statement(), &key_a).unwrap();
        let envelope_b = sign_envelope(statement(), &key_b).unwrap();
        assert_ne!(envelope_a.signature.key_id, envelope_b.signature.key_id);
        assert_ne!(
            envelope_a.signature.signature_b64,
            envelope_b.signature.signature_b64
        );
    }

    #[test]
    fn stored_key_round_trips() {
        let original = generate_keypair();
        let stored = original.to_stored();
        let parsed = AttestationKey::from_stored(&stored).unwrap();
        assert_eq!(original.key_id(), parsed.key_id());
        assert_eq!(original.public_key_b64(), parsed.public_key_b64());
    }
}
