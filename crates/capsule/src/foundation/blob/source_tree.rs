//! A1v2 source-tree profile: admissibility preconditions in front of the
//! frozen A1 v1 tree hash.
//!
//! This module implements the profile specified in
//! `docs/rfcs/draft/A1_SOURCE_TREE_PROFILE.md`. It does **not** define a new
//! digest. [`materialized_source_tree_hash`] first walks a materialized
//! checkout to check the A1v2 admissibility rules (§3.3 of the profile) and,
//! only if the tree is admissible, delegates to the frozen A1 v1 algorithm in
//! [`super::tree_hash::hash_tree`], returning its `sha256:<hex>` (`ato-blob-v1`)
//! string verbatim. Because the digest is reused byte-for-byte, every existing
//! A1 `blob_hash` output stays bit-identical; A1v2 only constrains *which*
//! trees may be hashed as source.
//!
//! ## Admissibility rules (all rejections route to `blocked_repo`)
//!
//! 1. Non-UTF-8 path component.
//! 2. Non-NFC path component (checked with [`unicode_normalization::is_nfc`];
//!    the profile *requires* NFC rather than normalizing, so identity is never
//!    silently changed by the platform).
//! 3. Unicode case-fold collision within a single directory (two distinct
//!    sibling names that fold equal).
//! 4. Any symlink (MVP rejects all symlinks).
//! 5. Submodule / gitlink signal: a nested `.git` entry (below the root) or a
//!    top-level `.gitmodules` file.
//! 6. Git-LFS pointer file (content begins with the LFS spec header).
//! 7. Unsupported node type (device / socket / FIFO) — also an error in A1.
//! 8. Size/count caps: `> 50_000` regular files, or any single regular file
//!    `> 50 MiB`.
//!
//! ## Case-fold used
//!
//! Rule 3 approximates Unicode *simple* case-folding with Rust std
//! [`str::to_lowercase`] (Unicode Default Case Conversion, full lowercase
//! mapping). Two sibling names collide iff their lowercase mappings are
//! byte-equal. This is the fold a case-insensitive filesystem (APFS / NTFS)
//! effectively applies, so it catches trees that are valid on a case-sensitive
//! filesystem but would collide on a case-insensitive one. The exact fold is
//! recorded here so the admissibility decision is reproducible.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_normalization::is_nfc;

use super::tree_hash::{TreeHashError, hash_tree};

/// Production cap: maximum number of regular files an admissible source tree
/// may contain (SOURCE_MATERIALIZATION_SPEC §3.4).
pub const MAX_FILE_COUNT: usize = 50_000;

/// Production cap: maximum size in bytes of any single regular file (50 MiB).
pub const MAX_FILE_SIZE_BYTES: u64 = 50 * 1024 * 1024;

/// A file must be no larger than this to be scanned for a Git-LFS pointer
/// header. Real LFS pointer files are ~130 bytes; anything larger is not a
/// pointer and is skipped to avoid reading big binaries.
const LFS_POINTER_SCAN_MAX: u64 = 1024;

/// The magic prefix of a Git-LFS pointer file (the `version` line of the
/// pointer spec, <https://git-lfs.github.com/spec/v1>).
const LFS_POINTER_MAGIC: &[u8] = b"version https://git-lfs.github.com/spec/";

/// Count/size caps applied during the admissibility walk.
///
/// Production always uses [`Limits::PRODUCTION`]; the field-taking form exists
/// only so in-crate tests can exercise the caps with tiny thresholds instead
/// of materializing 50k files or a 50 MiB file. There is no public API that
/// lets a caller lower the production caps.
#[derive(Debug, Clone, Copy)]
struct Limits {
    max_file_count: usize,
    max_file_size: u64,
}

impl Limits {
    const PRODUCTION: Limits = Limits {
        max_file_count: MAX_FILE_COUNT,
        max_file_size: MAX_FILE_SIZE_BYTES,
    };
}

/// Why a materialized checkout is not admissible as A1v2 source.
///
/// Each variant carries the offending path. Every structural/size variant maps
/// to the `blocked_repo` pipeline state (see [`Self::pipeline_state`]); only
/// [`SourceAdmissibilityError::Io`] is transient.
#[derive(Debug, Error)]
pub enum SourceAdmissibilityError {
    /// A path component is not valid UTF-8.
    #[error("non-UTF-8 path component at {path}")]
    NonUtf8Path { path: PathBuf },

    /// A path component is not in Unicode Normalization Form C.
    #[error("non-NFC path component at {path} (paths must already be NFC)")]
    NonNfcPath { path: PathBuf },

    /// Two distinct sibling names fold to the same value under simple
    /// case-folding, so the tree would collide on a case-insensitive
    /// filesystem.
    #[error("case-fold collision between {path} and {existing}")]
    CaseFoldCollision { path: PathBuf, existing: PathBuf },

    /// A symlink was found (all symlinks are rejected in the MVP).
    #[error("symlink is not allowed in source (MVP) at {path}")]
    Symlink { path: PathBuf },

    /// A git submodule / gitlink signal was found: a nested `.git` entry or a
    /// top-level `.gitmodules` file.
    #[error("git submodule / gitlink is not allowed in source at {path}")]
    Submodule { path: PathBuf },

    /// A Git-LFS pointer file was found (the MVP does not resolve LFS).
    #[error("unresolved Git-LFS pointer file at {path}")]
    LfsPointer { path: PathBuf },

    /// A device file, socket, or FIFO was found.
    #[error("unsupported node type (device/socket/FIFO) at {path}")]
    UnsupportedNodeType { path: PathBuf },

    /// The tree contains more than [`MAX_FILE_COUNT`] regular files.
    #[error("too many files: exceeded the limit of {limit} at {path}")]
    TooManyFiles { path: PathBuf, limit: usize },

    /// A single regular file exceeds [`MAX_FILE_SIZE_BYTES`].
    #[error("file too large: {path} is {size} bytes (limit {limit})")]
    FileTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },

    /// An I/O error occurred while walking or reading the tree.
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl SourceAdmissibilityError {
    /// The GitHub-capsule-request pipeline state this rejection maps to.
    ///
    /// Every structural/size admissibility violation is a terminal
    /// `blocked_repo` (pipeline spec §4.2). An [`Io`](Self::Io) is a transient
    /// failure the materialize job retries, so it maps to `failed_internal`.
    pub fn pipeline_state(&self) -> &'static str {
        match self {
            SourceAdmissibilityError::NonUtf8Path { .. }
            | SourceAdmissibilityError::NonNfcPath { .. }
            | SourceAdmissibilityError::CaseFoldCollision { .. }
            | SourceAdmissibilityError::Symlink { .. }
            | SourceAdmissibilityError::Submodule { .. }
            | SourceAdmissibilityError::LfsPointer { .. }
            | SourceAdmissibilityError::UnsupportedNodeType { .. }
            | SourceAdmissibilityError::TooManyFiles { .. }
            | SourceAdmissibilityError::FileTooLarge { .. } => "blocked_repo",
            SourceAdmissibilityError::Io { .. } => "failed_internal",
        }
    }

    /// The path the error refers to.
    pub fn path(&self) -> &Path {
        match self {
            SourceAdmissibilityError::NonUtf8Path { path }
            | SourceAdmissibilityError::NonNfcPath { path }
            | SourceAdmissibilityError::CaseFoldCollision { path, .. }
            | SourceAdmissibilityError::Symlink { path }
            | SourceAdmissibilityError::Submodule { path }
            | SourceAdmissibilityError::LfsPointer { path }
            | SourceAdmissibilityError::UnsupportedNodeType { path }
            | SourceAdmissibilityError::TooManyFiles { path, .. }
            | SourceAdmissibilityError::FileTooLarge { path, .. }
            | SourceAdmissibilityError::Io { path, .. } => path,
        }
    }
}

/// Computes the A1v2 `materialized_source_tree_hash` of `root`.
///
/// First runs the §3.3 admissibility checks over the whole tree; only if the
/// tree is admissible does it delegate to the frozen A1 v1 tree hash and return
/// that `sha256:<hex>` (`ato-blob-v1`) string verbatim. A tree that violates
/// any admissibility rule has **no** hash and returns the corresponding
/// [`SourceAdmissibilityError`].
pub fn materialized_source_tree_hash(root: &Path) -> Result<String, SourceAdmissibilityError> {
    materialized_source_tree_hash_with_limits(root, &Limits::PRODUCTION)
}

fn materialized_source_tree_hash_with_limits(
    root: &Path,
    limits: &Limits,
) -> Result<String, SourceAdmissibilityError> {
    // The root itself must be a directory. Mirror `hash_tree`'s stance: a
    // missing root or a non-directory root is not admissible source.
    let root_meta = fs::symlink_metadata(root).map_err(|source| SourceAdmissibilityError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    if !root_meta.file_type().is_dir() {
        return Err(SourceAdmissibilityError::Io {
            path: root.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "root path is not a directory",
            ),
        });
    }

    let mut file_count: usize = 0;
    walk_dir(root, root, limits, &mut file_count)?;

    // Admissible: reuse the frozen A1 v1 digest verbatim. Any error here is a
    // TOCTOU race (the tree changed between the two walks); admissibility has
    // already ruled out the exotic entries A1 would otherwise reject.
    let tree = hash_tree(root).map_err(map_tree_hash_error)?;
    Ok(tree.blob_hash)
}

/// Recursively checks the admissibility rules over `dir`. `root` is the walk's
/// starting directory (used to tell a repo's own top-level `.git` from a
/// nested submodule `.git`).
fn walk_dir(
    dir: &Path,
    root: &Path,
    limits: &Limits,
    file_count: &mut usize,
) -> Result<(), SourceAdmissibilityError> {
    let read = fs::read_dir(dir).map_err(|source| SourceAdmissibilityError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    let is_root = dir == root;
    // Maps a case-fold key to the first sibling name that produced it, so a
    // second name folding equal is a collision.
    let mut fold_keys: HashMap<String, PathBuf> = HashMap::new();

    for entry in read {
        let entry = entry.map_err(|source| SourceAdmissibilityError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name_os = entry.file_name();

        // Rule 1: UTF-8.
        let name = name_os
            .to_str()
            .ok_or_else(|| SourceAdmissibilityError::NonUtf8Path { path: path.clone() })?;

        // Rule 2: NFC.
        if !is_nfc(name) {
            return Err(SourceAdmissibilityError::NonNfcPath { path });
        }

        // Rule 3: case-fold collision within this directory.
        let fold_key = name.to_lowercase();
        if let Some(existing) = fold_keys.get(&fold_key) {
            return Err(SourceAdmissibilityError::CaseFoldCollision {
                path,
                existing: existing.clone(),
            });
        }
        fold_keys.insert(fold_key, path.clone());

        let metadata =
            fs::symlink_metadata(&path).map_err(|source| SourceAdmissibilityError::Io {
                path: path.clone(),
                source,
            })?;
        let file_type = metadata.file_type();

        // Rule 4: symlinks (checked before `.git` so a symlink named `.git`
        // is still rejected as a symlink).
        if file_type.is_symlink() {
            return Err(SourceAdmissibilityError::Symlink { path });
        }

        // Rule 5: submodule / gitlink signals. A nested `.git` (below the root)
        // is a submodule or embedded repo; a top-level `.gitmodules` file
        // declares submodules. The root's own top-level `.git` is not a signal
        // — a materialized source archive normally excludes it, and treating it
        // as an ordinary directory keeps this wrapper's hash identical to
        // `hash_tree` for every admissible tree.
        if name == ".git" && !is_root {
            return Err(SourceAdmissibilityError::Submodule { path });
        }
        if is_root && name == ".gitmodules" && file_type.is_file() {
            return Err(SourceAdmissibilityError::Submodule { path });
        }

        if file_type.is_dir() {
            walk_dir(&path, root, limits, file_count)?;
        } else if file_type.is_file() {
            let size = metadata.len();

            // Rule 8a: single-file size cap.
            if size > limits.max_file_size {
                return Err(SourceAdmissibilityError::FileTooLarge {
                    path,
                    size,
                    limit: limits.max_file_size,
                });
            }

            // Rule 6: Git-LFS pointer.
            if is_lfs_pointer(&path, size)? {
                return Err(SourceAdmissibilityError::LfsPointer { path });
            }

            // Rule 8b: file-count cap.
            *file_count += 1;
            if *file_count > limits.max_file_count {
                return Err(SourceAdmissibilityError::TooManyFiles {
                    path,
                    limit: limits.max_file_count,
                });
            }
        } else {
            // Rule 7: device / socket / FIFO.
            return Err(SourceAdmissibilityError::UnsupportedNodeType { path });
        }
    }

    Ok(())
}

/// Returns `true` if the regular file at `path` is a Git-LFS pointer stub.
/// Only files no larger than [`LFS_POINTER_SCAN_MAX`] are inspected.
fn is_lfs_pointer(path: &Path, size: u64) -> Result<bool, SourceAdmissibilityError> {
    if size > LFS_POINTER_SCAN_MAX {
        return Ok(false);
    }
    let bytes = fs::read(path).map_err(|source| SourceAdmissibilityError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(bytes.starts_with(LFS_POINTER_MAGIC))
}

/// Maps a residual [`TreeHashError`] from the delegated `hash_tree` call onto a
/// [`SourceAdmissibilityError`]. After a successful admissibility walk the only
/// error `hash_tree` can realistically return is a TOCTOU `Io`; the other arms
/// are defensive.
fn map_tree_hash_error(err: TreeHashError) -> SourceAdmissibilityError {
    match err {
        TreeHashError::Io { path, source } => SourceAdmissibilityError::Io { path, source },
        TreeHashError::NonUtf8Name { path } => SourceAdmissibilityError::NonUtf8Path { path },
        TreeHashError::UnsupportedFileType { path } => {
            SourceAdmissibilityError::UnsupportedNodeType { path }
        }
        TreeHashError::RootMissing(path) => SourceAdmissibilityError::Io {
            path,
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "root path does not exist"),
        },
        TreeHashError::RootNotDirectory(path) => SourceAdmissibilityError::Io {
            path,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "root path is not a directory",
            ),
        },
    }
}

/// The byte identity of a stored source archive: `sha256:<hex>` over the exact
/// `.tar.zst` bytes the builder produced.
///
/// This is deliberately **not** the tree hash — two different valid `.tar.zst`
/// encodings of the same tree share a [`materialized_source_tree_hash`] but have
/// different `source_archive_hash` (profile §3.4).
pub fn source_archive_hash(tar_zst_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tar_zst_bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_file(root: &Path, rel: &str, contents: &[u8]) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    /// Small caps used to exercise the count/size rules without materializing
    /// 50k files or a 50 MiB file. Production always uses `Limits::PRODUCTION`;
    /// this override is reachable only from in-crate tests.
    const TINY_LIMITS: Limits = Limits {
        max_file_count: 3,
        max_file_size: 16,
    };

    /// Detects whether the filesystem backing `dir` is case-sensitive. Needed
    /// because a real case-fold *collision* (two files differing only by case)
    /// cannot be materialized on a case-insensitive filesystem such as the
    /// default macOS APFS.
    fn fs_is_case_sensitive(dir: &Path) -> bool {
        let upper = dir.join("CASEPROBE");
        fs::write(&upper, b"x").unwrap();
        let lower_exists = dir.join("caseprobe").exists();
        fs::remove_file(&upper).ok();
        !lower_exists
    }

    // --- A1 freeze: the wrapper is the frozen digest, not a new one ---------

    #[test]
    fn admissible_tree_equals_frozen_a1_hash() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.rs", b"fn main() {}\n");
        write_file(tmp.path(), "README.md", b"hello\n");
        write_file(tmp.path(), "assets/logo.txt", b"art");

        let wrapped = materialized_source_tree_hash(tmp.path()).unwrap();
        let frozen = hash_tree(tmp.path()).unwrap().blob_hash;

        assert_eq!(
            wrapped, frozen,
            "materialized_source_tree_hash must reuse the frozen A1 v1 digest verbatim"
        );
        assert!(wrapped.starts_with("sha256:"));
    }

    #[test]
    fn admissible_tree_matches_pinned_golden_vector() {
        // A fixed conformant tree hashes to a pinned value. If this ever
        // changes, either the A1 v1 digest drifted (a freeze violation) or the
        // wrapper stopped delegating verbatim.
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "a.txt", b"alpha\n");
        write_file(tmp.path(), "dir/b.txt", b"beta\n");

        let wrapped = materialized_source_tree_hash(tmp.path()).unwrap();
        assert_eq!(
            wrapped,
            "sha256:2a3d4d3738248f96aec7590be1074dd7a7c99eeaa8430688cb591d192221ee64"
        );
    }

    #[test]
    fn determinism_same_tree_twice() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.rs", b"fn main() {}\n");
        write_file(tmp.path(), "README.md", b"hello\n");

        let first = materialized_source_tree_hash(tmp.path()).unwrap();
        let second = materialized_source_tree_hash(tmp.path()).unwrap();
        assert_eq!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_changes_hash_through_wrapper() {
        use std::os::unix::fs::PermissionsExt;

        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();
        write_file(tmp_a.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        write_file(tmp_b.path(), "bin/run", b"#!/bin/sh\necho hi\n");
        fs::set_permissions(
            tmp_a.path().join("bin/run"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::set_permissions(
            tmp_b.path().join("bin/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let a = materialized_source_tree_hash(tmp_a.path()).unwrap();
        let b = materialized_source_tree_hash(tmp_b.path()).unwrap();
        assert_ne!(a, b, "executable bit must still influence the hash");
    }

    // --- Rejection: one per admissibility rule ------------------------------

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "ok.txt", b"ok");
        let bad = tmp.path().join(OsStr::from_bytes(b"bad\xff"));
        if fs::write(&bad, b"x").is_err() {
            // Some filesystems (e.g. macOS APFS) reject non-UTF-8 filenames at
            // creation time with EILSEQ, so this rule cannot be exercised here.
            eprintln!(
                "skipping rejects_non_utf8_path: filesystem at {} rejects non-UTF-8 names",
                tmp.path().display()
            );
            return;
        }

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::NonUtf8Path { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn rejects_non_nfc_path() {
        // NFC "é" is U+00E9; the NFD form is "e" + U+0301 (combining acute).
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "caf\u{00e9}.txt", b"nfc is fine"); // admissible sibling
        write_file(tmp.path(), "e\u{0301}clair.txt", b"nfd name"); // decomposed -> reject

        // Sanity: the two forms are genuinely NFC vs NFD.
        assert!(is_nfc("caf\u{00e9}.txt"));
        assert!(!is_nfc("e\u{0301}clair.txt"));

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::NonNfcPath { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn nfc_name_alone_is_admissible() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "caf\u{00e9}.txt", b"nfc");
        assert!(materialized_source_tree_hash(tmp.path()).is_ok());
    }

    #[test]
    fn case_fold_maps_ascii_case_pairs_equal() {
        // Platform-independent proof of the exact fold used by rule 3.
        assert_eq!("README.md".to_lowercase(), "readme.md".to_lowercase());
        assert_eq!("Foo".to_lowercase(), "foo".to_lowercase());
        assert_ne!("a.txt".to_lowercase(), "b.txt".to_lowercase());
    }

    #[test]
    fn rejects_case_fold_collision() {
        let tmp = TempDir::new().unwrap();
        if !fs_is_case_sensitive(tmp.path()) {
            eprintln!(
                "skipping rejects_case_fold_collision: filesystem at {} is case-insensitive, \
                 so two case-folding siblings cannot be materialized",
                tmp.path().display()
            );
            return;
        }
        write_file(tmp.path(), "README.md", b"upper");
        write_file(tmp.path(), "readme.md", b"lower");

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::CaseFoldCollision { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "real.txt", b"target");
        symlink("real.txt", tmp.path().join("link")).unwrap();

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::Symlink { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn rejects_nested_git_gitlink() {
        // A submodule / embedded repo shows up as a nested `.git` entry.
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.rs", b"fn main() {}\n");
        write_file(
            tmp.path(),
            "vendor/sub/.git",
            b"gitdir: ../../.git/modules/sub\n",
        );

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::Submodule { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn rejects_top_level_gitmodules() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.rs", b"fn main() {}\n");
        write_file(
            tmp.path(),
            ".gitmodules",
            b"[submodule \"sub\"]\n\tpath = sub\n\turl = https://example.com/sub.git\n",
        );

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::Submodule { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn root_level_dot_git_is_treated_as_ordinary_directory() {
        // A repo's own top-level `.git` is not a submodule signal; when present
        // it hashes exactly as `hash_tree` sees it (they must agree).
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "src/main.rs", b"fn main() {}\n");
        write_file(tmp.path(), ".git/config", b"[core]\n");

        let wrapped = materialized_source_tree_hash(tmp.path()).unwrap();
        let frozen = hash_tree(tmp.path()).unwrap().blob_hash;
        assert_eq!(wrapped, frozen);
    }

    #[test]
    fn rejects_lfs_pointer() {
        let tmp = TempDir::new().unwrap();
        write_file(
            tmp.path(),
            "big.bin",
            b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 12345\n",
        );

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::LfsPointer { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn ordinary_file_is_not_mistaken_for_lfs_pointer() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "notes.txt", b"version 1 of my notes\n");
        assert!(materialized_source_tree_hash(tmp.path()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_unsupported_node_type_fifo() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "keep.txt", b"x");
        let fifo = tmp.path().join("pipe");
        let c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: standard libc call with a valid NUL-terminated path.
        let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o644) };
        if rc != 0 {
            eprintln!("skipping rejects_unsupported_node_type_fifo: mkfifo failed");
            return;
        }

        let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::UnsupportedNodeType { .. }),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    // --- Size / count caps (test-only tiny thresholds) ----------------------

    #[test]
    fn rejects_too_many_files() {
        let tmp = TempDir::new().unwrap();
        for i in 0..(TINY_LIMITS.max_file_count + 1) {
            write_file(tmp.path(), &format!("f{i}.txt"), b"x");
        }
        let err = materialized_source_tree_hash_with_limits(tmp.path(), &TINY_LIMITS).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::TooManyFiles { limit, .. } if limit == TINY_LIMITS.max_file_count),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn file_count_at_limit_is_admissible() {
        let tmp = TempDir::new().unwrap();
        for i in 0..TINY_LIMITS.max_file_count {
            write_file(tmp.path(), &format!("f{i}.txt"), b"x");
        }
        assert!(materialized_source_tree_hash_with_limits(tmp.path(), &TINY_LIMITS).is_ok());
    }

    #[test]
    fn rejects_file_too_large() {
        let tmp = TempDir::new().unwrap();
        // One byte over the tiny 16-byte limit.
        write_file(
            tmp.path(),
            "blob.bin",
            &vec![0u8; (TINY_LIMITS.max_file_size + 1) as usize],
        );

        let err = materialized_source_tree_hash_with_limits(tmp.path(), &TINY_LIMITS).unwrap_err();
        assert!(
            matches!(err, SourceAdmissibilityError::FileTooLarge { size, limit, .. }
                if size == TINY_LIMITS.max_file_size + 1 && limit == TINY_LIMITS.max_file_size),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
    }

    #[test]
    fn production_caps_are_50k_and_50mib() {
        // Guard against accidentally shipping the test thresholds.
        assert_eq!(Limits::PRODUCTION.max_file_count, 50_000);
        assert_eq!(Limits::PRODUCTION.max_file_size, 50 * 1024 * 1024);
    }

    // --- source_archive_hash ------------------------------------------------

    #[test]
    fn source_archive_hash_is_sha256_over_exact_bytes() {
        // Known SHA-256 of the ASCII bytes "hello".
        let h = source_archive_hash(b"hello");
        assert_eq!(
            h,
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        // Distinct bytes -> distinct hash; same bytes -> same hash.
        assert_ne!(source_archive_hash(b"hello"), source_archive_hash(b"hellp"));
        assert_eq!(source_archive_hash(b"hello"), source_archive_hash(b"hello"));
    }
}
