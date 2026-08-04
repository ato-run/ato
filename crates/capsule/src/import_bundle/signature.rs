//! `signature.json` — the `ato.capsule-index-signature/v1` detached signature
//! over `index.json`'s JCS bytes.
//!
//! A lenient parser here is exactly as exploitable as a lenient `index.json`
//! parser: a duplicate `key_id` or `signature` key lets an attacker make the
//! verified value differ from the displayed one. So this module rejects instead
//! of normalizing at every turn — padded base64, standard-alphabet base64,
//! uppercase digest hex, a non-canonical `did:key`, an unknown field, a repeated
//! key, or an `algorithm` that differs from `"ed25519"` by so much as a capital
//! letter.
//!
//! `claimed_issuer` is parsed and carried for display and **never** consulted by
//! [`super::trust`]; see [`ClaimedIssuer`].

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};

use super::CapsuleImportError;
use super::index::{Sha256Digest, reject_duplicate_json_keys};
use crate::types::identity::{did_to_public_key, public_key_to_did};

/// The only `schema` value a `signature.json` may carry in v1.
pub const SIGNATURE_SCHEMA: &str = "ato.capsule-index-signature/v1";

/// The domain-separation tag prefixed to the signed bytes.
///
/// Signed message = `UTF8(SIGNATURE_DOMAIN_TAG) || 0x00 || <JCS bytes of index.json>`.
pub const SIGNATURE_DOMAIN_TAG: &str = SIGNATURE_SCHEMA;

/// The signer's own claim about who they are.
///
/// **Display-only.** RFC §`signature.json`: an attacker can write
/// `"claimed_issuer": "ato-store"` on any bundle they sign with their own key,
/// so nothing in [`super::trust`] reads this value. It is modelled as a closed
/// enum rather than a free string because the RFC's schema enumerates exactly
/// three spellings, and an unrecognized fourth is more likely a malformed bundle
/// than a forward-compatible one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClaimedIssuer {
    /// A locally exported bundle.
    LocalAuthor,
    /// A publisher-signed bundle (no such producer exists in Slice 1).
    Publisher,
    /// A bundle claiming to come from the Ato Store.
    AtoStore,
}

impl ClaimedIssuer {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalAuthor => "local-author",
            Self::Publisher => "publisher",
            Self::AtoStore => "ato-store",
        }
    }
}

impl fmt::Display for ClaimedIssuer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ClaimedIssuer {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ClaimedIssuer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "local-author" => Ok(Self::LocalAuthor),
            "publisher" => Ok(Self::Publisher),
            "ato-store" => Ok(Self::AtoStore),
            other => Err(de::Error::custom(format!(
                "unknown claimed_issuer {other:?}; v1 defines only \"local-author\", \
                 \"publisher\", and \"ato-store\""
            ))),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// did:key
// ─────────────────────────────────────────────────────────────────────────────

/// A `did:key` identifier proven to decode to a 32-byte Ed25519 public key
/// under multicodec `0xed 0x01`, per `SIGNATURE_SPEC.md` §"Public Key Format".
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DidKey {
    canonical: String,
    key: [u8; 32],
}

impl DidKey {
    /// Parse and canonicality-check a `did:key`.
    ///
    /// Decoding alone is not enough: base58btc admits non-canonical spellings
    /// (a leading `1` is a zero byte, and some alphabets tolerate ambiguity), so
    /// the decoded key is re-encoded and compared against the input. A `did:key`
    /// that decodes correctly but is not the encoder's own output is rejected —
    /// two spellings of one key would be two identities in a pin comparison.
    ///
    /// # Errors
    ///
    /// A reason string on prefix, multicodec, length, or canonicality failure.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let key = did_to_public_key(raw).map_err(|source| source.to_string())?;
        let canonical = public_key_to_did(&key);
        if canonical != raw {
            return Err(format!(
                "did:key {raw:?} is not the canonical encoding of the key it decodes to \
                 (canonical form is {canonical:?})"
            ));
        }
        Ok(Self { canonical, key })
    }

    /// Build the canonical `did:key` for a raw Ed25519 public key.
    #[must_use]
    pub fn from_public_key(key: &[u8; 32]) -> Self {
        Self {
            canonical: public_key_to_did(key),
            key: *key,
        }
    }

    /// The canonical `did:key:z…` string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// The raw 32-byte Ed25519 public key.
    #[must_use]
    pub fn public_key(&self) -> &[u8; 32] {
        &self.key
    }

    fn verifying_key(&self) -> Result<VerifyingKey, CapsuleImportError> {
        VerifyingKey::from_bytes(&self.key).map_err(|source| {
            CapsuleImportError::signature(format!(
                "key_id {} is not a usable Ed25519 public key: {source}",
                self.canonical
            ))
        })
    }
}

impl fmt::Display for DidKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.canonical)
    }
}

impl Serialize for DidKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.canonical)
    }
}

impl<'de> Deserialize<'de> for DidKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        DidKey::parse(&raw).map_err(de::Error::custom)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Ed25519SignatureBytes
// ─────────────────────────────────────────────────────────────────────────────

/// Exactly 64 signature bytes, spelled as canonical **unpadded base64url**.
///
/// A padded or standard-alphabet value is rejected rather than normalized: the
/// signature string is inside a strict-schema document, and accepting two
/// spellings of one signature would mean the document a verifier reads is not
/// the document a producer wrote. A decode-valid-but-wrong-length value is
/// rejected here too, so it never surfaces as a confusing failure inside the
/// Ed25519 check itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Ed25519SignatureBytes([u8; 64]);

impl Ed25519SignatureBytes {
    /// Wrap raw signature bytes.
    #[must_use]
    pub fn from_raw(raw: [u8; 64]) -> Self {
        Self(raw)
    }

    /// Parse the canonical unpadded-base64url spelling.
    ///
    /// # Errors
    ///
    /// A reason string on alphabet, padding, trailing-bit, or length failure.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let decoded = URL_SAFE_NO_PAD.decode(raw).map_err(|source| {
            format!(
                "signature must be canonical unpadded base64url (no `+`, `/`, or `=`): {source}"
            )
        })?;
        let bytes: [u8; 64] = decoded.as_slice().try_into().map_err(|_| {
            format!(
                "signature must decode to exactly 64 bytes (Ed25519), got {}",
                decoded.len()
            )
        })?;
        Ok(Self(bytes))
    }

    /// The canonical unpadded-base64url spelling.
    #[must_use]
    pub fn to_encoded(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }
}

impl fmt::Debug for Ed25519SignatureBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Ed25519SignatureBytes({})", self.to_encoded())
    }
}

impl Serialize for Ed25519SignatureBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_encoded())
    }
}

impl<'de> Deserialize<'de> for Ed25519SignatureBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ed25519SignatureBytes::parse(&raw).map_err(de::Error::custom)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The document
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed, structurally valid `ato.capsule-index-signature/v1` document.
///
/// "Structurally valid" is not "verified": [`Self::verify_over_index`] is what
/// checks the digest and the Ed25519 signature, and trust in *who* signed is a
/// separate axis entirely ([`super::SignerTrust`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleIndexSignatureV1 {
    /// Must be [`SIGNATURE_SCHEMA`].
    pub schema: String,
    /// Must be the exact literal `"ed25519"`.
    pub algorithm: String,
    /// The signer's canonical `did:key`.
    pub key_id: DidKey,
    /// Self-declared, display-only. Never a trust input.
    pub claimed_issuer: ClaimedIssuer,
    /// SHA-256 of `index.json`'s JCS bytes.
    pub index_digest: Sha256Digest,
    /// Ed25519 signature over the domain-separated message.
    pub signature: Ed25519SignatureBytes,
}

impl CapsuleIndexSignatureV1 {
    /// The JCS canonical encoding of this document — what the writer emits.
    ///
    /// # Errors
    ///
    /// [`CapsuleImportError::SignatureInvalid`] if canonicalization fails.
    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, CapsuleImportError> {
        serde_jcs::to_vec(self).map_err(|source| {
            CapsuleImportError::signature(format!(
                "failed to canonicalize signature.json: {source}"
            ))
        })
    }

    /// Recompute the index digest and verify the Ed25519 signature over the
    /// domain-separated message.
    ///
    /// The digest check runs first, exactly as the RFC requires: a reader
    /// "recomputes it and rejects on mismatch before even attempting signature
    /// verification", so a bundle whose `signature.json` points at a different
    /// `index.json` fails with that specific reason rather than as a generic bad
    /// signature.
    ///
    /// # Errors
    ///
    /// [`CapsuleImportError::SignatureInvalid`] on digest or signature failure.
    pub fn verify_over_index(&self, index_jcs_bytes: &[u8]) -> Result<(), CapsuleImportError> {
        let recomputed = Sha256Digest::of_bytes(index_jcs_bytes);
        if recomputed != self.index_digest {
            return Err(CapsuleImportError::signature(format!(
                "signature.json index_digest is {}, but index.json hashes to {recomputed}",
                self.index_digest
            )));
        }

        let verifying_key = self.key_id.verifying_key()?;
        let signature = Signature::from_bytes(&self.signature.0);
        verifying_key
            .verify(&signing_message(index_jcs_bytes), &signature)
            .map_err(|source| {
                CapsuleImportError::signature(format!(
                    "Ed25519 verification failed for key {}: {source}",
                    self.key_id
                ))
            })
    }
}

/// Build the domain-separated bytes a v3 index signature covers:
/// `UTF8("ato.capsule-index-signature/v1") || 0x00 || <JCS bytes of index.json>`.
#[must_use]
pub fn signing_message(index_jcs_bytes: &[u8]) -> Vec<u8> {
    let tag = SIGNATURE_DOMAIN_TAG.as_bytes();
    let mut message = Vec::with_capacity(tag.len() + 1 + index_jcs_bytes.len());
    message.extend_from_slice(tag);
    message.push(0x00);
    message.extend_from_slice(index_jcs_bytes);
    message
}

/// Parse and structurally validate `signature.json` bytes.
///
/// Duplicate-key rejection runs over the raw bytes first, for the same reason it
/// does in [`super::index::parse_index_json`].
///
/// # Errors
///
/// [`CapsuleImportError::SignatureInvalid`] with the specific reason.
pub fn parse_signature_json(bytes: &[u8]) -> Result<CapsuleIndexSignatureV1, CapsuleImportError> {
    reject_duplicate_json_keys(bytes).map_err(|reason| {
        CapsuleImportError::signature(format!(
            "signature.json is not strictly parseable: {reason}"
        ))
    })?;

    let signature: CapsuleIndexSignatureV1 = serde_json::from_slice(bytes).map_err(|source| {
        CapsuleImportError::signature(format!(
            "signature.json does not match {SIGNATURE_SCHEMA}: {source}"
        ))
    })?;

    if signature.schema != SIGNATURE_SCHEMA {
        return Err(CapsuleImportError::signature(format!(
            "signature.json schema is {:?}; expected {SIGNATURE_SCHEMA:?}",
            signature.schema
        )));
    }
    if signature.algorithm != "ed25519" {
        return Err(CapsuleImportError::signature(format!(
            "signature.json algorithm is {:?}; v1 fixes it to the exact literal \"ed25519\"",
            signature.algorithm
        )));
    }
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_bytes_reject_padded_and_standard_alphabets() {
        let raw = [7u8; 64];
        let canonical = URL_SAFE_NO_PAD.encode(raw);
        assert!(Ed25519SignatureBytes::parse(&canonical).is_ok());

        let padded = base64::engine::general_purpose::URL_SAFE.encode(raw);
        assert!(
            Ed25519SignatureBytes::parse(&padded).is_err(),
            "padded base64url must be rejected, not normalized"
        );

        // A byte pattern whose standard-alphabet encoding actually uses `+`/`/`.
        let mut tricky = [0u8; 64];
        for (index, slot) in tricky.iter_mut().enumerate() {
            *slot = (index as u8).wrapping_mul(37).wrapping_add(251);
        }
        let standard = base64::engine::general_purpose::STANDARD_NO_PAD.encode(tricky);
        assert!(standard.contains('+') || standard.contains('/'));
        assert!(Ed25519SignatureBytes::parse(&standard).is_err());
    }

    #[test]
    fn signature_bytes_reject_wrong_length() {
        let short = URL_SAFE_NO_PAD.encode([1u8; 63]);
        let long = URL_SAFE_NO_PAD.encode([1u8; 65]);
        assert!(Ed25519SignatureBytes::parse(&short).is_err());
        assert!(Ed25519SignatureBytes::parse(&long).is_err());
    }

    #[test]
    fn did_key_round_trips_and_rejects_garbage() {
        let did = DidKey::from_public_key(&[3u8; 32]);
        assert_eq!(DidKey::parse(did.as_str()).expect("canonical"), did);
        assert!(DidKey::parse("did:key:z").is_err());
        assert!(DidKey::parse("not-a-did").is_err());
        assert!(DidKey::parse("did:key:zZZZZ0OIl").is_err());
    }

    #[test]
    fn signing_message_is_domain_separated() {
        let message = signing_message(b"{}");
        assert!(message.starts_with(SIGNATURE_DOMAIN_TAG.as_bytes()));
        assert_eq!(message[SIGNATURE_DOMAIN_TAG.len()], 0x00);
        assert_eq!(&message[SIGNATURE_DOMAIN_TAG.len() + 1..], b"{}");
    }
}
