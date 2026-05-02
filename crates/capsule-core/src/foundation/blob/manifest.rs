//! Self-describing manifest written next to every frozen blob payload.
//!
//! The manifest lives at `<blob>/manifest.json` and lets later code verify a
//! blob in isolation from the ref index. It also gives `verify()` enough
//! metadata to know how aggressively to re-walk the payload.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::tree_hash::TreeHash;

/// Wire format / schema version for the manifest.
pub const BLOB_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Tree hashing prefix locked down by `docs/rfcs/accepted/A1_BLOB_HASH.md`.
pub const BLOB_TREE_ALGORITHM: &str = "ato-blob-v1";

/// Self-describing record persisted with every frozen blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobManifest {
    pub schema_version: u32,
    /// Tree-hashing algorithm tag (e.g. `"ato-blob-v1"`).
    pub algorithm: String,
    /// Canonical `<digest-alg>:<hex>` blob hash. Identical to the directory
    /// name under `~/.ato/store/blobs/`.
    pub blob_hash: String,
    /// `DepDerivationKeyV1` hash that produced this blob.
    pub derivation_hash: String,
    /// RFC3339 timestamp recording when the blob was frozen.
    pub created_at: String,
    /// Regular file count inside the payload.
    pub file_count: usize,
    /// Symlink count inside the payload.
    pub symlink_count: usize,
    /// Directory count (excluding recursively empty dirs).
    pub dir_count: usize,
    /// Sum of regular file sizes in bytes.
    pub total_bytes: u64,
}

impl BlobManifest {
    /// Builds a manifest from a tree hash result and the producing derivation.
    pub fn from_tree_hash(
        tree: &TreeHash,
        derivation_hash: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: BLOB_MANIFEST_SCHEMA_VERSION,
            algorithm: BLOB_TREE_ALGORITHM.to_string(),
            blob_hash: tree.blob_hash.clone(),
            derivation_hash: derivation_hash.into(),
            created_at: created_at.into(),
            file_count: tree.file_count,
            symlink_count: tree.symlink_count,
            dir_count: tree.dir_count,
            total_bytes: tree.total_bytes,
        }
    }

    /// Writes the manifest as pretty-printed JSON, creating parent dirs.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let bytes =
            serde_json::to_vec_pretty(self).context("failed to serialize blob manifest")?;
        fs::write(path, [bytes, b"\n".to_vec()].concat())
            .with_context(|| format!("failed to write blob manifest at {}", path.display()))
    }

    /// Reads a manifest from `path`. Returns the parsed value.
    pub fn read_from(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read blob manifest at {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse blob manifest at {}", path.display()))
    }

    /// Returns true if the manifest's `blob_hash` matches the supplied value.
    ///
    /// This is the fast presence check used by `plan()`: we only confirm the
    /// manifest claims the right identifier; we do not re-walk the payload
    /// tree. Callers wanting cryptographic re-verification go through
    /// `verify()`.
    pub fn matches_blob_hash(&self, expected: &str) -> bool {
        self.blob_hash == expected
    }
}
