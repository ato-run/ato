//! Content hashing for CapsuleFS.
//!
//! One family on the hot path: `blake3:<hex>` over raw bytes for chunk/blob
//! content (see the crate docs for the full reconcile rationale).
//!
//! ## Security: [`ContentHash`] is a validated type
//!
//! A content hash is also a CAS *filename* — [`crate::CasStore`] joins its
//! `hex()` onto the store root to locate a chunk. A Ready-State manifest is
//! untrusted input (it can arrive from another host), so a malformed hash like
//! `"blake3:../../etc/passwd"` must never be able to escape the CAS root. We
//! make that impossible at the type level: a `ContentHash` can only be
//! constructed from raw bytes ([`hash_bytes`]) or by [`ContentHash::parse`],
//! which enforces `blake3:` + exactly 64 lowercase-hex characters. No slash, no
//! dot, no other path separator can survive parsing, so every `ContentHash` the
//! store ever sees is a safe single path component. Deserialization runs the
//! same gate, so a hostile manifest is rejected at the serde boundary.

use serde::{Deserialize, Serialize};

/// Length of a blake3 digest in lowercase-hex characters.
const BLAKE3_HEX_LEN: usize = 64;
/// The only hash prefix CapsuleFS accepts.
const HASH_PREFIX: &str = "blake3:";

/// A validated content address: `blake3:<64 lowercase-hex>`.
///
/// Serializes as the bare string (a manifest's chunk list is an array of plain
/// `"blake3:…"` strings, not wrapper objects). **Deserialization validates** —
/// an out-of-shape string is rejected rather than silently trusted, so the CAS
/// never path-joins attacker-controlled bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ContentHash(String);

/// Why a string is not a valid [`ContentHash`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid content hash {input:?}: {reason}")]
pub struct InvalidContentHash {
    /// The offending input.
    pub input: String,
    /// What was wrong with it.
    pub reason: &'static str,
}

impl ContentHash {
    /// Construct without validation. Private: callers either hash raw bytes
    /// (always valid) or go through [`ContentHash::parse`].
    pub(crate) fn new_unchecked(s: String) -> Self {
        debug_assert!(
            Self::parse(&s).is_ok(),
            "new_unchecked given an invalid content hash: {s:?}"
        );
        Self(s)
    }

    /// Parse and validate a `blake3:<hex>` string. The single trusted entry
    /// point for turning an arbitrary (possibly hostile) string into a
    /// `ContentHash`. Enforces:
    ///
    /// * the exact `blake3:` prefix (no other family),
    /// * exactly 64 hex digits after it,
    /// * digits drawn only from `[0-9a-f]` (lowercase) — which rules out `/`,
    ///   `.`, and every other path separator.
    pub fn parse(s: &str) -> Result<Self, InvalidContentHash> {
        let invalid = |reason| InvalidContentHash {
            input: s.to_string(),
            reason,
        };
        let hex = s
            .strip_prefix(HASH_PREFIX)
            .ok_or_else(|| invalid("missing required \"blake3:\" prefix"))?;
        if hex.len() != BLAKE3_HEX_LEN {
            return Err(invalid("digest must be exactly 64 hex characters"));
        }
        if !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(invalid(
                "digest must contain only lowercase hex [0-9a-f]",
            ));
        }
        Ok(Self(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The 64-hex digest without the `blake3:` prefix (the CAS on-disk
    /// filename). By construction this is a single safe path component — no
    /// separators, no `..`.
    pub fn hex(&self) -> &str {
        // Safe: every `ContentHash` carries the validated prefix.
        self.0.strip_prefix(HASH_PREFIX).unwrap_or(&self.0)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        ContentHash::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Hash raw bytes to a `blake3:<hex>` content address.
///
/// Identical to the existing CAS chunker's per-chunk hashing
/// (`format!("blake3:{}", blake3::hash(chunk).to_hex())`), so chunks are
/// interchangeable across the two stores.
pub fn hash_bytes(bytes: &[u8]) -> ContentHash {
    ContentHash::new_unchecked(format!("blake3:{}", blake3::hash(bytes).to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_blake3_prefixed_and_stable() {
        let h = hash_bytes(b"hello");
        assert!(h.as_str().starts_with("blake3:"));
        assert_eq!(h, hash_bytes(b"hello"));
        assert_ne!(h, hash_bytes(b"world"));
    }

    #[test]
    fn hex_strips_prefix() {
        let h = hash_bytes(b"x");
        assert!(!h.hex().contains(':'));
        assert_eq!(format!("blake3:{}", h.hex()), h.as_str());
    }

    #[test]
    fn matches_known_blake3_convention() {
        // Mirror the exact expression the capsule CAS chunker uses.
        let chunk = b"deterministic chunk bytes";
        let expected = format!("blake3:{}", blake3::hash(chunk).to_hex());
        assert_eq!(hash_bytes(chunk).as_str(), expected);
    }

    #[test]
    fn content_hash_serializes_transparently() {
        let h = hash_bytes(b"data");
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.starts_with("\"blake3:"));
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn parse_accepts_a_well_formed_hash() {
        let good = format!("blake3:{}", "a".repeat(64));
        let h = ContentHash::parse(&good).expect("valid hash parses");
        assert_eq!(h.as_str(), good);
        assert_eq!(h.hex(), "a".repeat(64));
    }

    #[test]
    fn parse_rejects_path_traversal_and_malformed_hashes() {
        // Every one of these must fail closed — none may become a ContentHash.
        let bad = [
            "blake3:../../x",                     // path traversal
            "blake3:/tmp/x",                      // absolute path escape
            "blake3:..",                          // parent dir
            "sha256:0000000000000000000000000000000000000000000000000000000000000000", // wrong family
            "blake3:not-hex-not-hex-not-hex-not-hex-not-hex-not-hex-not-hex-xx",        // non-hex
            "blake3:ABCDEF0000000000000000000000000000000000000000000000000000000000", // uppercase
            "deadbeef",                           // no prefix
            "",                                   // empty
        ];
        for s in bad {
            assert!(
                ContentHash::parse(s).is_err(),
                "expected {s:?} to be rejected"
            );
        }
        // Length boundaries: 63 and 65 hex chars both fail.
        assert!(ContentHash::parse(&format!("blake3:{}", "a".repeat(63))).is_err());
        assert!(ContentHash::parse(&format!("blake3:{}", "a".repeat(65))).is_err());
    }

    #[test]
    fn deserialize_rejects_a_hostile_hash() {
        // A manifest carrying a traversal string must not deserialize into a
        // usable ContentHash.
        let json = "\"blake3:../../../etc/passwd\"";
        let err = serde_json::from_str::<ContentHash>(json).unwrap_err();
        assert!(err.to_string().contains("invalid content hash"), "{err}");
    }

    #[test]
    fn hex_is_always_a_single_safe_path_component() {
        let h = hash_bytes(b"anything");
        let hex = h.hex();
        assert_eq!(hex.len(), 64);
        assert!(!hex.contains('/') && !hex.contains('.') && !hex.contains('\\'));
    }
}
