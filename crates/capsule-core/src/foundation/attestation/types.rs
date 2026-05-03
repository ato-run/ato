//! Frozen wire types for A2 attestations.
//!
//! These structures define the bytes that go on disk and through the
//! signature. Every field that participates in the canonical statement is
//! enumerated below; changing a name or order invalidates every existing
//! envelope and is therefore a breaking change that requires bumping the
//! `ATTESTATION_ALGORITHM` prefix.

use serde::{Deserialize, Serialize};

use crate::common::store::BlobAddress;

/// Schema version embedded in every envelope and statement.
pub const ATTESTATION_SCHEMA_VERSION: u32 = 1;

/// Algorithm tag locked down by `docs/rfcs/accepted/A2_ATTESTATION.md`.
pub const ATTESTATION_ALGORITHM: &str = "ato-attestation-v1";

/// Byte prefix mixed into every signed statement so cross-format
/// confusion attacks (replaying an attestation as a different artifact)
/// are impossible without colliding the prefix.
pub const ATTESTATION_SIGNING_PREFIX: &[u8] = b"ato-attestation-v1\0";

/// What an attestation talks about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationSubject {
    /// `"blob"` for cached dependency blobs, `"payload"` for capsule
    /// payload artifacts. Future kinds (e.g. `"runtime"`) extend this.
    pub kind: String,
    /// Canonical `<algorithm>:<hex>` digest of the subject.
    pub hash: String,
}

impl AttestationSubject {
    pub fn for_blob(blob_hash: &str) -> Self {
        Self {
            kind: "blob".to_string(),
            hash: blob_hash.to_string(),
        }
    }

    pub fn for_payload(payload_hash: &str) -> Self {
        Self {
            kind: "payload".to_string(),
            hash: payload_hash.to_string(),
        }
    }

    /// Returns the parsed [`BlobAddress`] when the subject is a blob.
    pub fn blob_address(&self) -> Option<BlobAddress> {
        if self.kind != "blob" {
            return None;
        }
        BlobAddress::parse(&self.hash).ok()
    }
}

/// Where a subject came from.
///
/// `requested_ref` (mutable input) is preserved for audit but is **not**
/// part of the identity; the resolved commit is the load-bearing field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SourceRef {
    pub authority: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub resolved_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_signature_verdict: Option<String>,
}

/// Snapshot of the policy values that influenced the produced subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PolicySnapshot {
    pub lifecycle_script_policy: String,
    pub registry_policy: String,
    pub network_policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_allowlist_digest: Option<String>,
}

/// Optional summary of the freeze that produced a blob subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FreezeMetadata {
    pub file_count: usize,
    pub symlink_count: usize,
    pub dir_count: usize,
    pub total_bytes: u64,
}

/// What the issuer is asserting about the subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationPredicate {
    /// Human-readable identifier for the issuing builder, e.g.
    /// `"ato-cli@0.4.114"` or a DID. Always required so unsigned/anonymous
    /// envelopes are not produced by accident.
    pub builder_id: String,
    /// Source provenance recovered from the resolver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRef>,
    /// Tree hash of the source materialized into the build, after LFS
    /// expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_tree_hash: Option<String>,
    /// Derivation hash that keys the subject in the A1 cache index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_hash: Option<String>,
    /// Policy state at issuance.
    pub policy: PolicySnapshot,
    /// Optional freeze summary (file/byte counts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze: Option<FreezeMetadata>,
}

/// What goes through the signature.
///
/// The statement is canonicalized via JCS (RFC 8785) and prefixed with
/// [`ATTESTATION_SIGNING_PREFIX`] before signing. The envelope below
/// stores the canonical bytes and the signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationStatement {
    pub schema_version: u32,
    pub algorithm: String,
    pub subject: AttestationSubject,
    pub predicate: AttestationPredicate,
    /// RFC3339 issuance timestamp.
    pub issued_at: String,
}

impl AttestationStatement {
    pub fn new(
        subject: AttestationSubject,
        predicate: AttestationPredicate,
        issued_at: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: ATTESTATION_SCHEMA_VERSION,
            algorithm: ATTESTATION_ALGORITHM.to_string(),
            subject,
            predicate,
            issued_at: issued_at.into(),
        }
    }

    /// Returns the JCS-canonical bytes of the statement.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_jcs::to_vec(self)
    }

    /// Returns the bytes that go into Ed25519 sign / verify
    /// (`ATTESTATION_SIGNING_PREFIX || canonical_bytes`).
    pub fn signing_input(&self) -> Result<Vec<u8>, serde_json::Error> {
        let canonical = self.canonical_bytes()?;
        let mut out = Vec::with_capacity(ATTESTATION_SIGNING_PREFIX.len() + canonical.len());
        out.extend_from_slice(ATTESTATION_SIGNING_PREFIX);
        out.extend_from_slice(&canonical);
        Ok(out)
    }
}

/// Envelope persisted on disk: statement + signature + key id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestationEnvelope {
    pub schema_version: u32,
    pub statement: AttestationStatement,
    pub signature: Signature,
}

/// Single Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// Always `"ed25519"` in v1.
    pub algorithm: String,
    /// SHA-256 of the public key bytes, lower-case hex with `key:` prefix.
    pub key_id: String,
    /// Base64-encoded raw signature bytes (64 bytes for Ed25519).
    pub signature_b64: String,
    /// Base64-encoded raw 32-byte public key, mirroring the key_id so
    /// envelopes can be checked offline by anyone holding a trust root.
    pub public_key_b64: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_statement() -> AttestationStatement {
        AttestationStatement::new(
            AttestationSubject::for_blob(
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
            AttestationPredicate {
                builder_id: "ato-cli@0.4.114".to_string(),
                source: Some(SourceRef {
                    authority: "github.com".to_string(),
                    repository: Some("acme/app".to_string()),
                    resolved_commit: "deadbeef".to_string(),
                    requested_ref: Some("main".to_string()),
                    commit_signature_verdict: None,
                }),
                source_tree_hash: Some("sha256:abc".to_string()),
                derivation_hash: Some("sha256:def".to_string()),
                policy: PolicySnapshot {
                    lifecycle_script_policy: "sandbox".to_string(),
                    registry_policy: "default".to_string(),
                    network_policy: "default".to_string(),
                    env_allowlist_digest: None,
                },
                freeze: Some(FreezeMetadata {
                    file_count: 12,
                    symlink_count: 0,
                    dir_count: 3,
                    total_bytes: 4096,
                }),
            },
            "2026-05-03T00:00:00Z",
        )
    }

    #[test]
    fn statement_canonicalizes_deterministically() {
        let statement = fixture_statement();
        let first = statement.canonical_bytes().unwrap();
        let second = statement.canonical_bytes().unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn signing_input_includes_versioned_prefix() {
        let statement = fixture_statement();
        let bytes = statement.signing_input().unwrap();
        assert!(bytes.starts_with(ATTESTATION_SIGNING_PREFIX));
        assert!(bytes.len() > ATTESTATION_SIGNING_PREFIX.len());
    }

    #[test]
    fn changing_subject_changes_signing_input() {
        let mut statement = fixture_statement();
        let original = statement.signing_input().unwrap();
        statement.subject.hash =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string();
        let modified = statement.signing_input().unwrap();
        assert_ne!(original, modified);
    }

    #[test]
    fn blob_subject_can_be_parsed_back_into_blob_address() {
        let subject = AttestationSubject::for_blob(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );
        let address = subject.blob_address().expect("blob subject parses");
        assert_eq!(address.algorithm(), "sha256");
    }

    #[test]
    fn payload_subject_does_not_yield_blob_address() {
        let subject = AttestationSubject::for_payload("sha256:abc");
        assert!(subject.blob_address().is_none());
    }
}
