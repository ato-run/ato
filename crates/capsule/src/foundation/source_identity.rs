//! Materialized source identity helpers.
//!
//! Git commit OIDs remain provenance. These helpers hash the archive bytes and
//! the final source tree that Ato will expose, while refusing to call an
//! unresolved Git LFS pointer or submodule checkout a complete materialization.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;
use walkdir::WalkDir;

use crate::blob::{TreeHashError, hash_tree};

const LFS_POINTER_HEADER: &[u8] = b"version https://git-lfs.github.com/spec/v1";
const LFS_POINTER_SCAN_LIMIT: u64 = 1024;

#[derive(Debug, Error)]
pub enum SourceIdentityError {
    #[error("source root contains VCS metadata and is not a clean materialized projection: {0:?}")]
    VcsMetadataPresent(PathBuf),
    #[error("source contains a submodule declaration that was not expanded: {0:?}")]
    UnexpandedSubmodule(PathBuf),
    #[error("source contains an unresolved Git LFS pointer: {0:?}")]
    UnresolvedLfsPointer(PathBuf),
    #[error("failed to read materialized source at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Walk(#[from] walkdir::Error),
    #[error(transparent)]
    Tree(#[from] TreeHashError),
}

/// SHA-256 of the exact source archive bytes received from the authority.
pub fn source_archive_hash(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Verify that the filesystem does not retain unresolved Git indirections.
///
/// `.git` metadata is skipped because it is provenance, not runnable source.
/// A `.gitmodules` marker is rejected conservatively: an archive does not carry
/// enough Git object information to prove the referenced gitlinks were expanded.
pub fn verify_fully_materialized(root: &Path) -> Result<(), SourceIdentityError> {
    let entries = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".git");

    for entry in entries {
        let entry = entry?;
        if entry.file_name() == ".gitmodules" {
            return Err(SourceIdentityError::UnexpandedSubmodule(
                entry.path().to_path_buf(),
            ));
        }
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let file = File::open(path).map_err(|source| SourceIdentityError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut prefix = Vec::with_capacity(LFS_POINTER_SCAN_LIMIT as usize);
        file.take(LFS_POINTER_SCAN_LIMIT)
            .read_to_end(&mut prefix)
            .map_err(|source| SourceIdentityError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if prefix.starts_with(LFS_POINTER_HEADER) {
            return Err(SourceIdentityError::UnresolvedLfsPointer(
                path.to_path_buf(),
            ));
        }
    }
    Ok(())
}

/// Hash a clean, fully materialized source projection with Ato's stable tree
/// algorithm. Callers must remove `.git` metadata before invoking this function.
pub fn materialized_tree_hash(root: &Path) -> Result<String, SourceIdentityError> {
    verify_fully_materialized(root)?;
    let git_dir = root.join(".git");
    if fs::symlink_metadata(&git_dir).is_ok() {
        return Err(SourceIdentityError::VcsMetadataPresent(git_dir));
    }
    Ok(hash_tree(root)?.blob_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir() -> tempfile::TempDir {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        fs::create_dir_all(&target).expect("create target test directory");
        tempfile::Builder::new()
            .prefix("source-identity-")
            .tempdir_in(target)
            .expect("create source identity test directory")
    }

    #[test]
    fn archive_and_tree_hashes_are_content_sensitive() {
        assert_ne!(
            source_archive_hash(b"first"),
            source_archive_hash(b"second")
        );

        let root = test_dir();
        fs::write(root.path().join("app.txt"), b"first").expect("write source");
        let first = materialized_tree_hash(root.path()).expect("hash first tree");
        fs::write(root.path().join("app.txt"), b"second").expect("mutate source");
        let second = materialized_tree_hash(root.path()).expect("hash second tree");
        assert_ne!(first, second);
    }

    #[test]
    fn unresolved_lfs_pointer_fails_closed() {
        let root = test_dir();
        fs::write(
            root.path().join("model.bin"),
            b"version https://git-lfs.github.com/spec/v1\noid sha256:1234\nsize 42\n",
        )
        .expect("write pointer");
        assert!(matches!(
            materialized_tree_hash(root.path()),
            Err(SourceIdentityError::UnresolvedLfsPointer(_))
        ));
    }

    #[test]
    fn submodule_marker_fails_closed() {
        let root = test_dir();
        fs::write(
            root.path().join(".gitmodules"),
            b"[submodule \"vendor\"]\npath = vendor\nurl = https://example.invalid/vendor\n",
        )
        .expect("write gitmodules");
        assert!(matches!(
            materialized_tree_hash(root.path()),
            Err(SourceIdentityError::UnexpandedSubmodule(_))
        ));
    }
}
