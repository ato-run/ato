//! Content hashing for CapsuleFS.
//!
//! One family on the hot path: `blake3:<hex>` over raw bytes for chunk/blob
//! content (see the crate docs for the full reconcile rationale).

use serde::{Deserialize, Serialize};

/// A content address: `blake3:<hex>`.
///
/// `#[serde(transparent)]` so it serializes as the bare string — a manifest's
/// chunk list is an array of plain `"blake3:…"` strings, not wrapper objects.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    /// Wrap a pre-formatted `blake3:<hex>` string.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The hex digest without the `blake3:` prefix (the CAS on-disk filename).
    pub fn hex(&self) -> &str {
        self.0.strip_prefix("blake3:").unwrap_or(&self.0)
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
    ContentHash(format!("blake3:{}", blake3::hash(bytes).to_hex()))
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
}
