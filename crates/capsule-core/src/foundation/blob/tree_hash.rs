//! Deterministic content-addressable hashing for directory trees.
//!
//! ## Frozen specification
//!
//! This file implements the algorithm spelled out in
//! `docs/rfcs/accepted/A1_BLOB_HASH.md`. The wire format is **frozen**:
//! every byte of input that feeds into the digest is enumerated below and
//! must never change without bumping the `ato-blob-v?` prefix.
//!
//! ### Per-node digests (recursive children)
//!
//! For each entry inside a directory, we compute a 32-byte SHA-256:
//!
//! ```text
//! file:    sha256(b"file\0" || basename || b"\0" || mode_byte || b"\0" || content_sha256)
//! dir:     sha256(b"dir\0"  || basename || b"\0" || concat(sorted_child_hashes))
//! symlink: sha256(b"link\0" || basename || b"\0" || link_target_bytes)
//! ```
//!
//! - `mode_byte` is `1u8` if the executable bit is set on the regular file
//!   and `0u8` otherwise. No other permission bits influence the hash.
//! - mtime/atime/ctime, owner uid/gid, xattrs, and ACLs are deliberately
//!   ignored.
//! - Children are sorted by raw `OsStr::as_bytes()` (lexicographic). No
//!   Unicode normalization is performed.
//! - A directory whose recursive content is empty is **omitted** from its
//!   parent's child list (POSIX-tar convention).
//! - Hidden entries (e.g. `.git`) are **not** filtered. The caller is
//!   responsible for excluding paths *before* hashing if it wants them gone.
//!
//! ### Top-level digest
//!
//! The blob hash is then derived from the concatenated child hashes of the
//! root directory:
//!
//! ```text
//! root_concat = concat(sorted_root_child_hashes)
//! blob_hash   = sha256(b"ato-blob-v1\0" || root_concat)
//! ```
//!
//! The basename of the root directory is not part of the input, so the same
//! tree hashes identically regardless of where it is materialized on disk.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Error type for [`hash_tree`].
#[derive(Debug, Error)]
pub enum TreeHashError {
    #[error("root path does not exist: {0}")]
    RootMissing(PathBuf),
    #[error("root path is not a directory: {0}")]
    RootNotDirectory(PathBuf),
    #[error(
        "unsupported file type at {path}: only regular files, directories, and symlinks are hashed"
    )]
    UnsupportedFileType { path: PathBuf },
    #[error("non-UTF-8 file name on a non-Unix platform at {path}")]
    NonUtf8Name { path: PathBuf },
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Result of hashing a directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeHash {
    /// Canonical `<algorithm>:<hex>` representation of the blob hash.
    pub blob_hash: String,
    /// Number of regular files included in the hash.
    pub file_count: usize,
    /// Number of symlinks included in the hash.
    pub symlink_count: usize,
    /// Number of directories included in the hash (excludes recursively empty dirs).
    pub dir_count: usize,
    /// Sum of regular file sizes in bytes.
    pub total_bytes: u64,
}

#[derive(Default)]
struct TreeStats {
    files: usize,
    symlinks: usize,
    dirs: usize,
    total_bytes: u64,
}

/// Computes the deterministic content-addressable hash of `root`.
///
/// `root` must point to an existing directory. The function walks the tree
/// recursively, hashing files, symlinks, and non-empty directories per the
/// specification at the top of this module.
pub fn hash_tree(root: &Path) -> Result<TreeHash, TreeHashError> {
    let metadata = fs::symlink_metadata(root).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => TreeHashError::RootMissing(root.to_path_buf()),
        _ => TreeHashError::Io {
            path: root.to_path_buf(),
            source: err,
        },
    })?;
    if !metadata.file_type().is_dir() {
        return Err(TreeHashError::RootNotDirectory(root.to_path_buf()));
    }

    let mut stats = TreeStats::default();
    let root_concat = hash_dir_children(root, &mut stats)?;

    let mut hasher = Sha256::new();
    hasher.update(b"ato-blob-v1\0");
    hasher.update(&root_concat);
    let digest = hasher.finalize();

    Ok(TreeHash {
        blob_hash: format!("sha256:{}", hex::encode(digest)),
        file_count: stats.files,
        symlink_count: stats.symlinks,
        dir_count: stats.dirs,
        total_bytes: stats.total_bytes,
    })
}

/// Walks `dir`'s children in sorted byte order and returns the concatenated
/// child digests (32 bytes per included entry).
fn hash_dir_children(dir: &Path, stats: &mut TreeStats) -> Result<Vec<u8>, TreeHashError> {
    let entries = read_dir_sorted(dir)?;
    let mut concat: Vec<u8> = Vec::with_capacity(entries.len() * 32);
    for (name_bytes, entry_path) in entries {
        if let Some(node_hash) = hash_node(&entry_path, &name_bytes, stats)? {
            concat.extend_from_slice(&node_hash);
        }
    }
    Ok(concat)
}

/// Hashes a single entry. Returns `None` if the entry is a recursively empty
/// directory (excluded from its parent's child list).
fn hash_node(
    path: &Path,
    name_bytes: &[u8],
    stats: &mut TreeStats,
) -> Result<Option<[u8; 32]>, TreeHashError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| TreeHashError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(path).map_err(|source| TreeHashError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let target_bytes = os_str_bytes(target.as_os_str(), path)?;
        let mut hasher = Sha256::new();
        hasher.update(b"link\0");
        hasher.update(name_bytes);
        hasher.update(b"\0");
        hasher.update(&target_bytes);
        stats.symlinks += 1;
        Ok(Some(hasher.finalize().into()))
    } else if file_type.is_file() {
        let content = fs::read(path).map_err(|source| TreeHashError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mode_byte = if is_executable(&metadata) { 1u8 } else { 0u8 };
        let mut content_hasher = Sha256::new();
        content_hasher.update(&content);
        let content_hash = content_hasher.finalize();

        let mut hasher = Sha256::new();
        hasher.update(b"file\0");
        hasher.update(name_bytes);
        hasher.update(b"\0");
        hasher.update([mode_byte]);
        hasher.update(b"\0");
        hasher.update(&content_hash);
        stats.files += 1;
        stats.total_bytes = stats.total_bytes.saturating_add(content.len() as u64);
        Ok(Some(hasher.finalize().into()))
    } else if file_type.is_dir() {
        let child_concat = hash_dir_children(path, stats)?;
        if child_concat.is_empty() {
            // Recursively empty directories are excluded from their parent's
            // child list so adding/removing empty scaffolding does not change
            // the blob hash.
            return Ok(None);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"dir\0");
        hasher.update(name_bytes);
        hasher.update(b"\0");
        hasher.update(&child_concat);
        stats.dirs += 1;
        Ok(Some(hasher.finalize().into()))
    } else {
        Err(TreeHashError::UnsupportedFileType {
            path: path.to_path_buf(),
        })
    }
}

/// Reads `dir`'s entries and sorts them by raw byte order of their names.
fn read_dir_sorted(dir: &Path) -> Result<Vec<(Vec<u8>, PathBuf)>, TreeHashError> {
    let read = fs::read_dir(dir).map_err(|source| TreeHashError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| TreeHashError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name_bytes = os_str_bytes(entry.file_name().as_os_str(), &path)?;
        entries.push((name_bytes, path));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

#[cfg(unix)]
fn os_str_bytes(value: &std::ffi::OsStr, _path: &Path) -> Result<Vec<u8>, TreeHashError> {
    Ok(value.as_bytes().to_vec())
}

#[cfg(not(unix))]
fn os_str_bytes(value: &std::ffi::OsStr, path: &Path) -> Result<Vec<u8>, TreeHashError> {
    value
        .to_str()
        .map(|s| s.as_bytes().to_vec())
        .ok_or_else(|| TreeHashError::NonUtf8Name {
            path: path.to_path_buf(),
        })
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    // Owner executable bit; we ignore group/other bits intentionally so the
    // hash is stable across umasks.
    (metadata.permissions().mode() & 0o100) != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    // Without POSIX permissions, treat all files as non-executable. The
    // Windows projection path is out of scope for A1 anyway.
    false
}
