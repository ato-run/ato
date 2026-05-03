//! Physical layout helpers for the immutable artifact store (`~/.ato/store`).
//!
//! See `docs/rfcs/draft/A1_PROJECTION_STRATEGY.md` for the projection model
//! that consumes these paths and `docs/rfcs/accepted/A1_BLOB_HASH.md` for the
//! tree-hash that fills `BlobAddress`.
//!
//! ## Layout
//!
//! ```text
//! ~/.ato/store/
//! ├── blobs/<alg>/<hex[0:2]>/<hex[2:]>/
//! │   ├── payload/        # immutable payload tree
//! │   └── manifest.json   # self-describing blob manifest
//! ├── refs/
//! │   └── deps/<ecosystem>/<derivation-hash>.json   # weak ref
//! ├── meta/
//! │   └── blobs/<alg>/<hex[0:2]>/<hex[2:]>.json     # mutable observation
//! └── locks/
//!     └── derivation-<alg>-<hex>.lock               # advisory flock
//! ```
//!
//! `BlobAddress` is the canonical entry point; raw string juggling should be
//! confined to this module so the rest of the codebase only sees structured
//! values.

use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

use super::paths::{ato_store_blobs_dir, ato_store_dir, ato_store_meta_dir, ato_store_refs_dir};

/// Hex-encoded digest length for SHA-256.
const SHA256_HEX_LEN: usize = 64;

/// Errors raised when parsing a [`BlobAddress`] from a string.
#[derive(Debug, Error)]
pub enum BlobAddressError {
    #[error("blob hash is empty")]
    Empty,
    #[error("blob hash is missing the `<algorithm>:<hex>` separator")]
    MissingSeparator,
    #[error("blob hash algorithm `{0}` is not supported (only `sha256` is accepted today)")]
    UnsupportedAlgorithm(String),
    #[error(
        "blob hash hex length {found} does not match expected {expected} for algorithm `{alg}`"
    )]
    InvalidHexLength {
        alg: String,
        expected: usize,
        found: usize,
    },
    #[error("blob hash hex contains non-hexadecimal characters")]
    NonHexCharacters,
}

/// Strongly typed wrapper around a content-addressable blob identifier.
///
/// The on-wire representation is `<algorithm>:<lowercase-hex>` (for example
/// `sha256:abc...`). Path construction uses two-level sharding on the hex
/// prefix so a single store can scale to millions of blobs without exhausting
/// inode tables in any one directory.
///
/// ## Determinism
///
/// `BlobAddress` is intentionally a thin parsed view. It does **not** compute
/// the hash itself; that is `tree_hash`'s responsibility. Constructing a
/// `BlobAddress` is therefore zero-cost beyond validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobAddress {
    algorithm: String,
    hex: String,
}

impl BlobAddress {
    /// Parses a `<algorithm>:<hex>` blob hash.
    pub fn parse(value: &str) -> Result<Self, BlobAddressError> {
        if value.is_empty() {
            return Err(BlobAddressError::Empty);
        }
        let (alg, hex) = value
            .split_once(':')
            .ok_or(BlobAddressError::MissingSeparator)?;
        if alg != "sha256" {
            return Err(BlobAddressError::UnsupportedAlgorithm(alg.to_string()));
        }
        if hex.len() != SHA256_HEX_LEN {
            return Err(BlobAddressError::InvalidHexLength {
                alg: alg.to_string(),
                expected: SHA256_HEX_LEN,
                found: hex.len(),
            });
        }
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(BlobAddressError::NonHexCharacters);
        }
        // Normalize hex casing so paths are stable across callers.
        let hex = hex.to_ascii_lowercase();
        Ok(Self {
            algorithm: alg.to_string(),
            hex,
        })
    }

    /// Returns the canonical `<algorithm>:<hex>` representation.
    pub fn as_str(&self) -> String {
        format!("{}:{}", self.algorithm, self.hex)
    }

    /// Returns the algorithm tag (e.g. `"sha256"`).
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    /// Returns the lowercase hex digest.
    pub fn hex(&self) -> &str {
        &self.hex
    }

    fn shard(&self) -> (&str, &str) {
        let (head, tail) = self.hex.split_at(2);
        (head, tail)
    }

    /// Returns `~/.ato/store/blobs/<alg>/<hex[0:2]>/<hex[2:]>/`.
    pub fn dir(&self) -> PathBuf {
        let (head, tail) = self.shard();
        ato_store_blobs_dir()
            .join(&self.algorithm)
            .join(head)
            .join(tail)
    }

    /// Returns the immutable payload directory inside the blob.
    pub fn payload_dir(&self) -> PathBuf {
        self.dir().join("payload")
    }

    /// Returns the path to the self-describing `manifest.json`.
    pub fn manifest_path(&self) -> PathBuf {
        self.dir().join("manifest.json")
    }

    /// Returns the mutable per-blob observation file.
    ///
    /// Layout: `~/.ato/store/meta/blobs/<alg>/<hex[0:2]>/<hex[2:]>.json`.
    pub fn meta_path(&self) -> PathBuf {
        let (head, tail) = self.shard();
        ato_store_meta_dir()
            .join("blobs")
            .join(&self.algorithm)
            .join(head)
            .join(format!("{tail}.json"))
    }

    /// Returns a sibling temp directory for atomic move operations.
    ///
    /// Callers should write the staged blob into `staging_dir(suffix)` and
    /// `rename(2)` it into `dir()` once writes are durable. The suffix is
    /// expected to be a process-unique token (uuid, pid, etc.) so concurrent
    /// freezes do not stomp on each other.
    pub fn staging_dir(&self, suffix: &str) -> PathBuf {
        let (head, _) = self.shard();
        ato_store_blobs_dir()
            .join(&self.algorithm)
            .join(head)
            .join(format!("{}.tmp-{}", &self.hex[2..], suffix))
    }
}

impl fmt::Display for BlobAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

/// Returns the weak-ref path that maps a derivation key to a blob hash.
///
/// Layout: `~/.ato/store/refs/deps/<ecosystem>/<sanitized-derivation>.json`.
/// The colon in `sha256:...` is replaced with `-` so the path is portable
/// across filesystems that disallow colons (notably Windows).
pub fn ato_store_dep_ref_path(ecosystem: &str, derivation_hash: &str) -> PathBuf {
    ato_store_refs_dir()
        .join("deps")
        .join(sanitize_path_segment(ecosystem))
        .join(sanitize_hash_for_path(derivation_hash))
        .with_extension("json")
}

/// Returns `~/.ato/store/locks/`.
pub fn ato_store_locks_dir() -> PathBuf {
    ato_store_dir().join("locks")
}

/// Returns the advisory lock path for a derivation hash.
///
/// Concurrent `materialize()` calls for the same derivation key contend on
/// this file via `flock(2)` so only one process performs the freeze while
/// the others either wait or observe the cache hit on retry.
pub fn ato_store_derivation_lock_path(derivation_hash: &str) -> PathBuf {
    ato_store_locks_dir().join(format!(
        "derivation-{}.lock",
        sanitize_hash_for_path(derivation_hash)
    ))
}

/// Sanitize a hash identifier (`sha256:abc...`) into a single path segment.
fn sanitize_hash_for_path(hash: &str) -> String {
    hash.replace(':', "-")
}

/// Sanitize a free-form path segment (ecosystem name etc.) so it stays inside
/// a single directory level.
fn sanitize_path_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn sample() -> BlobAddress {
        BlobAddress::parse(&format!("sha256:{SAMPLE_HEX}")).expect("valid blob hash")
    }

    #[test]
    fn parse_accepts_canonical_sha256_hash() {
        let addr = sample();
        assert_eq!(addr.algorithm(), "sha256");
        assert_eq!(addr.hex(), SAMPLE_HEX);
        assert_eq!(addr.as_str(), format!("sha256:{SAMPLE_HEX}"));
    }

    #[test]
    fn parse_rejects_missing_separator() {
        assert!(matches!(
            BlobAddress::parse("not-a-hash"),
            Err(BlobAddressError::MissingSeparator)
        ));
    }

    #[test]
    fn parse_rejects_unsupported_algorithm() {
        assert!(matches!(
            BlobAddress::parse("blake3:deadbeef"),
            Err(BlobAddressError::UnsupportedAlgorithm(alg)) if alg == "blake3"
        ));
    }

    #[test]
    fn parse_rejects_wrong_hex_length() {
        assert!(matches!(
            BlobAddress::parse("sha256:deadbeef"),
            Err(BlobAddressError::InvalidHexLength { .. })
        ));
    }

    #[test]
    fn parse_rejects_non_hex() {
        let payload = format!("sha256:{}{}", &SAMPLE_HEX[..63], "Z");
        assert!(matches!(
            BlobAddress::parse(&payload),
            Err(BlobAddressError::NonHexCharacters)
        ));
    }

    #[test]
    fn parse_normalizes_hex_case() {
        let upper = SAMPLE_HEX.to_ascii_uppercase();
        let addr = BlobAddress::parse(&format!("sha256:{upper}")).unwrap();
        assert_eq!(addr.hex(), SAMPLE_HEX);
    }

    #[test]
    fn dir_paths_use_two_level_sharding() {
        let addr = sample();
        let blobs_root = ato_store_blobs_dir();

        let expected_dir = blobs_root.join("sha256").join("01").join(&SAMPLE_HEX[2..]);
        assert_eq!(addr.dir(), expected_dir);
        assert_eq!(addr.payload_dir(), expected_dir.join("payload"));
        assert_eq!(addr.manifest_path(), expected_dir.join("manifest.json"));
    }

    #[test]
    fn meta_path_mirrors_blob_sharding() {
        let addr = sample();
        let expected = ato_store_meta_dir()
            .join("blobs")
            .join("sha256")
            .join("01")
            .join(format!("{}.json", &SAMPLE_HEX[2..]));
        assert_eq!(addr.meta_path(), expected);
    }

    #[test]
    fn staging_dir_lives_next_to_target() {
        let addr = sample();
        let staging = addr.staging_dir("worker-7");
        assert_eq!(staging.parent(), addr.dir().parent());
        let staging_name = staging.file_name().unwrap().to_string_lossy().into_owned();
        assert!(staging_name.starts_with(&SAMPLE_HEX[2..]));
        assert!(staging_name.contains(".tmp-worker-7"));
    }

    #[test]
    fn dep_ref_path_replaces_colon_in_hash() {
        let path = ato_store_dep_ref_path("npm", "sha256:abc123");
        let last = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(last, "sha256-abc123.json");
        assert!(path
            .components()
            .any(|c| c.as_os_str().to_string_lossy() == "deps"));
    }

    #[test]
    fn dep_ref_path_sanitizes_ecosystem_segment() {
        let path = ato_store_dep_ref_path("npm/private", "sha256:abc");
        let segments: Vec<String> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        assert!(segments.contains(&"npm-private".to_string()));
        assert!(!segments.iter().any(|s| s.contains('/')));
    }

    #[test]
    fn derivation_lock_path_lives_under_locks_dir() {
        let path = ato_store_derivation_lock_path("sha256:dead");
        assert_eq!(path.parent(), Some(ato_store_locks_dir().as_path()));
        let last = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(last, "derivation-sha256-dead.lock");
    }
}
