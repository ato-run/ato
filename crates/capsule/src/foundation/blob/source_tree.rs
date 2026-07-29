//! Source-tree admissibility and identity for materialized repository source.
//!
//! This module implements the profile specified in
//! `docs/rfcs/draft/A1_SOURCE_TREE_PROFILE.md`. Trees without symlinks retain
//! the frozen A1 v1 digest byte-for-byte. A tree containing a permitted
//! repository-internal symlink uses the source-profile extension described in
//! that RFC, committing the A1 digest plus each link's path, raw target,
//! normalized resolved target, and terminal target kind.
//!
//! ## Admissibility rules (all rejections route to `blocked_repo`)
//!
//! 1. Non-UTF-8 path component.
//! 2. Non-NFC path component (checked with [`unicode_normalization::is_nfc`];
//!    the profile *requires* NFC rather than normalizing, so identity is never
//!    silently changed by the platform).
//! 3. Unicode case-fold collision within a single directory (two distinct
//!    sibling names that fold equal).
//! 4. Symlinks are relative, resolve inside the repository to an existing
//!    regular file or directory, and have no cycle.
//! 5. Submodule / gitlink signal: a nested `.git` entry (below the root) or a
//!    top-level `.gitmodules` file.
//! 6. Git-LFS pointer file (content begins with the LFS spec header).
//! 7. Unsupported node type (device / socket / FIFO) — also an error in A1.
//! 8. Size/count caps: `> 50_000` regular files, or any single regular file
//!    `> 50 MiB`.
//! 9. Explicit path, target, resolution-depth, and expansion caps.
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

use std::collections::{HashMap, HashSet, VecDeque};
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

/// Portable path and symlink bounds. These are checked before hashing and again
/// after archive extraction through the same validator.
pub const MAX_PATH_COMPONENT_BYTES: usize = 255;
pub const MAX_SYMLINK_TARGET_BYTES: usize = 4 * 1024;
pub const MAX_RESOLVED_PATH_BYTES: usize = 4 * 1024;
pub const MAX_SYMLINK_RESOLUTION_DEPTH: usize = 40;
pub const MAX_SYMLINK_EXPANSIONS: usize = 100_000;
pub const MAX_TREE_ENTRY_COUNT: usize = 100_000;

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
    max_path_component_bytes: usize,
    max_symlink_target_bytes: usize,
    max_resolved_path_bytes: usize,
    max_symlink_resolution_depth: usize,
    max_symlink_expansions: usize,
    max_tree_entry_count: usize,
}

impl Limits {
    const PRODUCTION: Limits = Limits {
        max_file_count: MAX_FILE_COUNT,
        max_file_size: MAX_FILE_SIZE_BYTES,
        max_path_component_bytes: MAX_PATH_COMPONENT_BYTES,
        max_symlink_target_bytes: MAX_SYMLINK_TARGET_BYTES,
        max_resolved_path_bytes: MAX_RESOLVED_PATH_BYTES,
        max_symlink_resolution_depth: MAX_SYMLINK_RESOLUTION_DEPTH,
        max_symlink_expansions: MAX_SYMLINK_EXPANSIONS,
        max_tree_entry_count: MAX_TREE_ENTRY_COUNT,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedTargetKind {
    File,
    Directory,
}

impl ResolvedTargetKind {
    fn identity_label(self) -> &'static [u8] {
        match self {
            Self::File => b"file",
            Self::Directory => b"dir",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ValidatedEntryKind {
    Directory,
    File {
        size: u64,
        executable: bool,
    },
    Symlink {
        raw_target: String,
        resolved_target: PathBuf,
        target_kind: ResolvedTargetKind,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedSourceEntry {
    pub(crate) rel: PathBuf,
    pub(crate) abs: PathBuf,
    pub(crate) kind: ValidatedEntryKind,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidatedSourceTree {
    pub(crate) entries: Vec<ValidatedSourceEntry>,
    pub(crate) tree_hash: String,
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

    #[error("symlink target at {path} is not valid UTF-8")]
    NonUtf8SymlinkTarget { path: PathBuf },

    #[error("symlink target at {path} is empty")]
    EmptySymlinkTarget { path: PathBuf },

    #[error("symlink target at {path} contains a NUL byte")]
    NulSymlinkTarget { path: PathBuf },

    #[error("symlink target at {path} must be a relative portable path: {target}")]
    AbsoluteOrPlatformSymlinkTarget { path: PathBuf, target: String },

    #[error("symlink target at {path} escapes the repository root: {target}")]
    SymlinkEscape { path: PathBuf, target: String },

    #[error("symlink target at {path} is dangling: {target}")]
    DanglingSymlink { path: PathBuf, target: PathBuf },

    #[error("symlink target at {path} traverses non-directory entry {target}")]
    SymlinkTraversesNonDirectory { path: PathBuf, target: PathBuf },

    #[error("symlink cycle detected at {path}")]
    SymlinkCycle { path: PathBuf },

    #[error("symlink resolution depth at {path} exceeds {limit}")]
    SymlinkDepthExceeded { path: PathBuf, limit: usize },

    #[error("symlink expansion count exceeds {limit} at {path}")]
    SymlinkExpansionLimit { path: PathBuf, limit: usize },

    #[error("path component at {path} is {bytes} bytes (limit {limit})")]
    PathComponentTooLong {
        path: PathBuf,
        bytes: usize,
        limit: usize,
    },

    #[error("symlink target at {path} is {bytes} bytes (limit {limit})")]
    SymlinkTargetTooLong {
        path: PathBuf,
        bytes: usize,
        limit: usize,
    },

    #[error("resolved symlink target at {path} is {bytes} bytes (limit {limit})")]
    ResolvedPathTooLong {
        path: PathBuf,
        bytes: usize,
        limit: usize,
    },

    #[error("ambiguous cross-platform path at {path}: {reason}")]
    AmbiguousPath { path: PathBuf, reason: &'static str },

    #[error("source tree has more than {limit} entries at {path}")]
    TooManyEntries { path: PathBuf, limit: usize },

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
            | SourceAdmissibilityError::NonUtf8SymlinkTarget { .. }
            | SourceAdmissibilityError::EmptySymlinkTarget { .. }
            | SourceAdmissibilityError::NulSymlinkTarget { .. }
            | SourceAdmissibilityError::AbsoluteOrPlatformSymlinkTarget { .. }
            | SourceAdmissibilityError::SymlinkEscape { .. }
            | SourceAdmissibilityError::DanglingSymlink { .. }
            | SourceAdmissibilityError::SymlinkTraversesNonDirectory { .. }
            | SourceAdmissibilityError::SymlinkCycle { .. }
            | SourceAdmissibilityError::SymlinkDepthExceeded { .. }
            | SourceAdmissibilityError::SymlinkExpansionLimit { .. }
            | SourceAdmissibilityError::PathComponentTooLong { .. }
            | SourceAdmissibilityError::SymlinkTargetTooLong { .. }
            | SourceAdmissibilityError::ResolvedPathTooLong { .. }
            | SourceAdmissibilityError::AmbiguousPath { .. }
            | SourceAdmissibilityError::TooManyEntries { .. }
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
            | SourceAdmissibilityError::NonUtf8SymlinkTarget { path }
            | SourceAdmissibilityError::EmptySymlinkTarget { path }
            | SourceAdmissibilityError::NulSymlinkTarget { path }
            | SourceAdmissibilityError::AbsoluteOrPlatformSymlinkTarget { path, .. }
            | SourceAdmissibilityError::SymlinkEscape { path, .. }
            | SourceAdmissibilityError::DanglingSymlink { path, .. }
            | SourceAdmissibilityError::SymlinkTraversesNonDirectory { path, .. }
            | SourceAdmissibilityError::SymlinkCycle { path }
            | SourceAdmissibilityError::SymlinkDepthExceeded { path, .. }
            | SourceAdmissibilityError::SymlinkExpansionLimit { path, .. }
            | SourceAdmissibilityError::PathComponentTooLong { path, .. }
            | SourceAdmissibilityError::SymlinkTargetTooLong { path, .. }
            | SourceAdmissibilityError::ResolvedPathTooLong { path, .. }
            | SourceAdmissibilityError::AmbiguousPath { path, .. }
            | SourceAdmissibilityError::TooManyEntries { path, .. }
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
    Ok(validate_source_tree(root)?.tree_hash)
}

#[cfg(test)]
fn materialized_source_tree_hash_with_limits(
    root: &Path,
    limits: &Limits,
) -> Result<String, SourceAdmissibilityError> {
    Ok(validate_source_tree_with_limits(root, limits)?.tree_hash)
}

pub(crate) fn validate_source_tree(
    root: &Path,
) -> Result<ValidatedSourceTree, SourceAdmissibilityError> {
    validate_source_tree_with_limits(root, &Limits::PRODUCTION)
}

fn validate_source_tree_with_limits(
    root: &Path,
    limits: &Limits,
) -> Result<ValidatedSourceTree, SourceAdmissibilityError> {
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

    let mut entries = Vec::new();
    let mut file_count: usize = 0;
    walk_dir(root, root, limits, &mut file_count, &mut entries)?;

    let by_path: HashMap<PathBuf, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.rel.clone(), index))
        .collect();
    let mut total_expansions = 0usize;
    for index in 0..entries.len() {
        let (raw_target, link_path) = match &entries[index].kind {
            ValidatedEntryKind::Symlink { raw_target, .. } => {
                (raw_target.clone(), entries[index].rel.clone())
            }
            _ => continue,
        };
        let normalized = normalize_symlink_target(&link_path, &raw_target, limits)?;
        let (resolved_target, target_kind) = resolve_symlink_target(
            &link_path,
            normalized,
            &entries,
            &by_path,
            limits,
            &mut total_expansions,
        )?;
        if target_kind == ResolvedTargetKind::Directory
            && (resolved_target.as_os_str().is_empty() || link_path.starts_with(&resolved_target))
        {
            return Err(SourceAdmissibilityError::SymlinkCycle { path: link_path });
        }
        entries[index].kind = ValidatedEntryKind::Symlink {
            raw_target,
            resolved_target,
            target_kind,
        };
    }

    let tree = hash_tree(root).map_err(map_tree_hash_error)?;
    let tree_hash = if tree.symlink_count == 0 {
        tree.blob_hash
    } else {
        extended_symlink_tree_hash(&tree.blob_hash, &entries)?
    };
    Ok(ValidatedSourceTree { entries, tree_hash })
}

/// Recursively checks the admissibility rules over `dir`. `root` is the walk's
/// starting directory (used to tell a repo's own top-level `.git` from a
/// nested submodule `.git`).
fn walk_dir(
    dir: &Path,
    root: &Path,
    limits: &Limits,
    file_count: &mut usize,
    entries: &mut Vec<ValidatedSourceEntry>,
) -> Result<(), SourceAdmissibilityError> {
    let read = fs::read_dir(dir).map_err(|source| SourceAdmissibilityError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut directory_entries =
        read.collect::<Result<Vec<_>, _>>()
            .map_err(|source| SourceAdmissibilityError::Io {
                path: dir.to_path_buf(),
                source,
            })?;
    directory_entries.sort_by(|left, right| {
        left.file_name()
            .to_string_lossy()
            .as_bytes()
            .cmp(right.file_name().to_string_lossy().as_bytes())
    });

    let is_root = dir == root;
    // Maps a case-fold key to the first sibling name that produced it, so a
    // second name folding equal is a collision.
    let mut fold_keys: HashMap<String, PathBuf> = HashMap::new();

    for entry in directory_entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .expect("read_dir child remains below its root")
            .to_path_buf();
        let name_os = entry.file_name();

        // Rule 1: UTF-8.
        let name = name_os
            .to_str()
            .ok_or_else(|| SourceAdmissibilityError::NonUtf8Path { path: rel.clone() })?;
        validate_path_component(name, &rel, limits)?;

        // Rule 2: NFC.
        if !is_nfc(name) {
            return Err(SourceAdmissibilityError::NonNfcPath { path: rel });
        }

        // Rule 3: case-fold collision within this directory.
        let fold_key = name.to_lowercase();
        if let Some(existing) = fold_keys.get(&fold_key) {
            return Err(SourceAdmissibilityError::CaseFoldCollision {
                path: rel.clone(),
                existing: existing.clone(),
            });
        }
        fold_keys.insert(fold_key, rel.clone());

        let metadata =
            fs::symlink_metadata(&path).map_err(|source| SourceAdmissibilityError::Io {
                path: path.clone(),
                source,
            })?;
        let file_type = metadata.file_type();

        // Rule 5: submodule / gitlink signals. A nested `.git` (below the root)
        // is a submodule or embedded repo; a top-level `.gitmodules` file
        // declares submodules. The root's own top-level `.git` is not a signal
        // — a materialized source archive normally excludes it, and treating it
        // as an ordinary directory keeps this wrapper's hash identical to
        // `hash_tree` for every admissible tree.
        if name == ".git" && !is_root {
            return Err(SourceAdmissibilityError::Submodule { path: rel });
        }
        if is_root && name == ".gitmodules" && file_type.is_file() {
            return Err(SourceAdmissibilityError::Submodule { path: rel });
        }

        if entries.len() >= limits.max_tree_entry_count {
            return Err(SourceAdmissibilityError::TooManyEntries {
                path: rel,
                limit: limits.max_tree_entry_count,
            });
        }

        if file_type.is_dir() {
            entries.push(ValidatedSourceEntry {
                rel,
                abs: path.clone(),
                kind: ValidatedEntryKind::Directory,
            });
            walk_dir(&path, root, limits, file_count, entries)?;
        } else if file_type.is_file() {
            let size = metadata.len();

            // Rule 8a: single-file size cap.
            if size > limits.max_file_size {
                return Err(SourceAdmissibilityError::FileTooLarge {
                    path: rel,
                    size,
                    limit: limits.max_file_size,
                });
            }

            // Rule 6: Git-LFS pointer.
            if is_lfs_pointer(&path, size)? {
                return Err(SourceAdmissibilityError::LfsPointer { path: rel });
            }

            // Rule 8b: file-count cap.
            *file_count += 1;
            if *file_count > limits.max_file_count {
                return Err(SourceAdmissibilityError::TooManyFiles {
                    path: rel,
                    limit: limits.max_file_count,
                });
            }
            entries.push(ValidatedSourceEntry {
                rel,
                abs: path,
                kind: ValidatedEntryKind::File {
                    size,
                    executable: is_executable(&metadata),
                },
            });
        } else if file_type.is_symlink() {
            let target = fs::read_link(&path).map_err(|source| SourceAdmissibilityError::Io {
                path: rel.clone(),
                source,
            })?;
            let raw_target = target
                .to_str()
                .ok_or_else(|| SourceAdmissibilityError::NonUtf8SymlinkTarget {
                    path: rel.clone(),
                })?
                .to_string();
            normalize_symlink_target(&rel, &raw_target, limits)?;
            entries.push(ValidatedSourceEntry {
                rel,
                abs: path,
                kind: ValidatedEntryKind::Symlink {
                    raw_target,
                    resolved_target: PathBuf::new(),
                    target_kind: ResolvedTargetKind::File,
                },
            });
        } else {
            // Rule 7: device / socket / FIFO.
            return Err(SourceAdmissibilityError::UnsupportedNodeType { path: rel });
        }
    }

    Ok(())
}

fn validate_path_component(
    component: &str,
    path: &Path,
    limits: &Limits,
) -> Result<(), SourceAdmissibilityError> {
    if component.len() > limits.max_path_component_bytes {
        return Err(SourceAdmissibilityError::PathComponentTooLong {
            path: path.to_path_buf(),
            bytes: component.len(),
            limit: limits.max_path_component_bytes,
        });
    }
    if component.contains('\\') {
        return Err(SourceAdmissibilityError::AmbiguousPath {
            path: path.to_path_buf(),
            reason: "backslash changes meaning across archive extraction platforms",
        });
    }
    if component.as_bytes().contains(&0) {
        return Err(SourceAdmissibilityError::AmbiguousPath {
            path: path.to_path_buf(),
            reason: "NUL is not a portable path byte",
        });
    }
    Ok(())
}

fn normalize_symlink_target(
    link_path: &Path,
    raw_target: &str,
    limits: &Limits,
) -> Result<PathBuf, SourceAdmissibilityError> {
    if raw_target.is_empty() {
        return Err(SourceAdmissibilityError::EmptySymlinkTarget {
            path: link_path.to_path_buf(),
        });
    }
    if raw_target.as_bytes().contains(&0) {
        return Err(SourceAdmissibilityError::NulSymlinkTarget {
            path: link_path.to_path_buf(),
        });
    }
    if raw_target.len() > limits.max_symlink_target_bytes {
        return Err(SourceAdmissibilityError::SymlinkTargetTooLong {
            path: link_path.to_path_buf(),
            bytes: raw_target.len(),
            limit: limits.max_symlink_target_bytes,
        });
    }
    let bytes = raw_target.as_bytes();
    let windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if raw_target.starts_with('/')
        || raw_target.starts_with('\\')
        || raw_target.contains('\\')
        || windows_drive
    {
        return Err(SourceAdmissibilityError::AbsoluteOrPlatformSymlinkTarget {
            path: link_path.to_path_buf(),
            target: raw_target.to_string(),
        });
    }
    if raw_target.ends_with('/') || raw_target.split('/').any(str::is_empty) {
        return Err(SourceAdmissibilityError::AmbiguousPath {
            path: link_path.to_path_buf(),
            reason: "empty or trailing target components are not canonical",
        });
    }

    let mut components: Vec<String> = link_path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    for component in raw_target.split('/') {
        match component {
            "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(SourceAdmissibilityError::SymlinkEscape {
                        path: link_path.to_path_buf(),
                        target: raw_target.to_string(),
                    });
                }
            }
            normal => {
                validate_path_component(normal, link_path, limits)?;
                components.push(normal.to_string());
            }
        }
    }
    let normalized = components.iter().collect::<PathBuf>();
    let normalized_len =
        components.iter().map(String::len).sum::<usize>() + components.len().saturating_sub(1);
    if normalized_len > limits.max_resolved_path_bytes {
        return Err(SourceAdmissibilityError::ResolvedPathTooLong {
            path: link_path.to_path_buf(),
            bytes: normalized_len,
            limit: limits.max_resolved_path_bytes,
        });
    }
    Ok(normalized)
}

pub(crate) fn validate_symlink_target(
    link_path: &Path,
    raw_target: &str,
) -> Result<PathBuf, SourceAdmissibilityError> {
    normalize_symlink_target(link_path, raw_target, &Limits::PRODUCTION)
}

fn resolve_symlink_target(
    origin_link: &Path,
    initial: PathBuf,
    entries: &[ValidatedSourceEntry],
    by_path: &HashMap<PathBuf, usize>,
    limits: &Limits,
    total_expansions: &mut usize,
) -> Result<(PathBuf, ResolvedTargetKind), SourceAdmissibilityError> {
    let mut pending: VecDeque<String> = initial
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut resolved: Vec<String> = Vec::new();
    let mut visited = HashSet::new();
    let mut depth = 1usize;
    *total_expansions += 1;
    if depth > limits.max_symlink_resolution_depth {
        return Err(SourceAdmissibilityError::SymlinkDepthExceeded {
            path: origin_link.to_path_buf(),
            limit: limits.max_symlink_resolution_depth,
        });
    }
    if *total_expansions > limits.max_symlink_expansions {
        return Err(SourceAdmissibilityError::SymlinkExpansionLimit {
            path: origin_link.to_path_buf(),
            limit: limits.max_symlink_expansions,
        });
    }

    while let Some(component) = pending.pop_front() {
        let mut candidate = resolved.iter().collect::<PathBuf>();
        candidate.push(&component);
        let Some(index) = by_path.get(&candidate).copied() else {
            return Err(SourceAdmissibilityError::DanglingSymlink {
                path: origin_link.to_path_buf(),
                target: candidate,
            });
        };
        match &entries[index].kind {
            ValidatedEntryKind::Symlink { raw_target, .. } => {
                if !visited.insert(candidate.clone()) {
                    return Err(SourceAdmissibilityError::SymlinkCycle { path: candidate });
                }
                depth += 1;
                *total_expansions += 1;
                if depth > limits.max_symlink_resolution_depth {
                    return Err(SourceAdmissibilityError::SymlinkDepthExceeded {
                        path: origin_link.to_path_buf(),
                        limit: limits.max_symlink_resolution_depth,
                    });
                }
                if *total_expansions > limits.max_symlink_expansions {
                    return Err(SourceAdmissibilityError::SymlinkExpansionLimit {
                        path: origin_link.to_path_buf(),
                        limit: limits.max_symlink_expansions,
                    });
                }
                let target = normalize_symlink_target(&candidate, raw_target, limits)?;
                let mut replacement: VecDeque<String> = target
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                replacement.append(&mut pending);
                pending = replacement;
                resolved.clear();
            }
            ValidatedEntryKind::Directory => resolved.push(component),
            ValidatedEntryKind::File { .. } => {
                if !pending.is_empty() {
                    return Err(SourceAdmissibilityError::SymlinkTraversesNonDirectory {
                        path: origin_link.to_path_buf(),
                        target: candidate,
                    });
                }
                resolved.push(component);
            }
        }
    }

    let target = resolved.iter().collect::<PathBuf>();
    if target.as_os_str().is_empty() {
        return Ok((target, ResolvedTargetKind::Directory));
    }
    let Some(index) = by_path.get(&target).copied() else {
        return Err(SourceAdmissibilityError::DanglingSymlink {
            path: origin_link.to_path_buf(),
            target,
        });
    };
    let kind = match entries[index].kind {
        ValidatedEntryKind::File { .. } => ResolvedTargetKind::File,
        ValidatedEntryKind::Directory => ResolvedTargetKind::Directory,
        ValidatedEntryKind::Symlink { .. } => {
            return Err(SourceAdmissibilityError::SymlinkCycle {
                path: origin_link.to_path_buf(),
            });
        }
    };
    Ok((target, kind))
}

fn extended_symlink_tree_hash(
    a1_hash: &str,
    entries: &[ValidatedSourceEntry],
) -> Result<String, SourceAdmissibilityError> {
    let mut hasher = Sha256::new();
    hasher.update(b"ato-source-tree-symlink-v1\0");
    update_identity_field(&mut hasher, a1_hash.as_bytes());
    let mut symlinks = entries
        .iter()
        .filter(|entry| matches!(&entry.kind, ValidatedEntryKind::Symlink { .. }))
        .map(|entry| Ok((portable_path_bytes(&entry.rel)?, entry)))
        .collect::<Result<Vec<_>, SourceAdmissibilityError>>()?;
    symlinks.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for (link_path, entry) in symlinks {
        let ValidatedEntryKind::Symlink {
            raw_target,
            resolved_target,
            target_kind,
        } = &entry.kind
        else {
            continue;
        };
        hasher.update(b"symlink\0");
        update_identity_field(&mut hasher, link_path.as_bytes());
        update_identity_field(&mut hasher, raw_target.as_bytes());
        update_identity_field(
            &mut hasher,
            portable_path_bytes(resolved_target)?.as_bytes(),
        );
        update_identity_field(&mut hasher, target_kind.identity_label());
    }
    Ok(format!("sha256:{}", hex::encode(hasher.finalize())))
}

fn update_identity_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

pub(crate) fn portable_path_bytes(path: &Path) -> Result<String, SourceAdmissibilityError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let value = component.as_os_str().to_str().ok_or_else(|| {
            SourceAdmissibilityError::NonUtf8Path {
                path: path.to_path_buf(),
            }
        })?;
        parts.push(value);
    }
    Ok(parts.join("/"))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    (metadata.permissions().mode() & 0o100) != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
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
        max_path_component_bytes: 16,
        max_symlink_target_bytes: 16,
        max_resolved_path_bytes: 32,
        max_symlink_resolution_depth: 3,
        max_symlink_expansions: 8,
        max_tree_entry_count: 16,
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
    fn accepts_relative_file_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "real.txt", b"target");
        symlink("real.txt", tmp.path().join("link")).unwrap();

        let validated = validate_source_tree(tmp.path()).unwrap();
        assert!(validated.tree_hash.starts_with("sha256:"));
        assert!(matches!(
            validated
                .entries
                .iter()
                .find(|entry| entry.rel == Path::new("link"))
                .map(|entry| &entry.kind),
            Some(ValidatedEntryKind::Symlink {
                resolved_target,
                target_kind: ResolvedTargetKind::File,
                ..
            }) if resolved_target == Path::new("real.txt")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_relative_directory_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "lib/assets/logo.txt", b"target");
        fs::create_dir_all(tmp.path().join("lib/site")).unwrap();
        symlink("../assets", tmp.path().join("lib/site/assets")).unwrap();

        let validated = validate_source_tree(tmp.path()).unwrap();
        assert!(matches!(
            validated
                .entries
                .iter()
                .find(|entry| entry.rel == Path::new("lib/site/assets"))
                .map(|entry| &entry.kind),
            Some(ValidatedEntryKind::Symlink {
                resolved_target,
                target_kind: ResolvedTargetKind::Directory,
                ..
            }) if resolved_target == Path::new("lib/assets")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_nested_relative_symlink_chain() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "target.txt", b"target");
        symlink("target.txt", tmp.path().join("second")).unwrap();
        symlink("second", tmp.path().join("first")).unwrap();

        let validated = validate_source_tree(tmp.path()).unwrap();
        assert!(matches!(
            validated
                .entries
                .iter()
                .find(|entry| entry.rel == Path::new("first"))
                .map(|entry| &entry.kind),
            Some(ValidatedEntryKind::Symlink {
                resolved_target,
                target_kind: ResolvedTargetKind::File,
                ..
            }) if resolved_target == Path::new("target.txt")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_absolute_windows_drive_and_unc_symlinks() {
        use std::os::unix::fs::symlink;

        for target in ["/etc/passwd", "C:/Windows/system.ini", r"\\server\share"] {
            let tmp = TempDir::new().unwrap();
            symlink(target, tmp.path().join("link")).unwrap();
            let err = materialized_source_tree_hash(tmp.path()).unwrap_err();
            assert!(
                matches!(
                    err,
                    SourceAdmissibilityError::AbsoluteOrPlatformSymlinkTarget { .. }
                ),
                "target {target}: got {err:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_and_nul_symlink_targets_before_filesystem_access() {
        assert!(matches!(
            validate_symlink_target(Path::new("link"), "").unwrap_err(),
            SourceAdmissibilityError::EmptySymlinkTarget { .. }
        ));
        assert!(matches!(
            validate_symlink_target(Path::new("link"), "bad\0target").unwrap_err(),
            SourceAdmissibilityError::NulSymlinkTarget { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_symlink_target() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        symlink(OsStr::from_bytes(b"target-\xff"), tmp.path().join("link")).unwrap();
        assert!(matches!(
            materialized_source_tree_hash(tmp.path()).unwrap_err(),
            SourceAdmissibilityError::NonUtf8SymlinkTarget { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape_and_dangling_target() {
        use std::os::unix::fs::symlink;

        let escaped = TempDir::new().unwrap();
        fs::create_dir_all(escaped.path().join("nested")).unwrap();
        symlink("../../outside", escaped.path().join("nested/link")).unwrap();
        assert!(matches!(
            materialized_source_tree_hash(escaped.path()).unwrap_err(),
            SourceAdmissibilityError::SymlinkEscape { .. }
        ));

        let dangling = TempDir::new().unwrap();
        symlink("missing", dangling.path().join("link")).unwrap();
        assert!(matches!(
            materialized_source_tree_hash(dangling.path()).unwrap_err(),
            SourceAdmissibilityError::DanglingSymlink { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_self_and_two_node_symlink_cycles() {
        use std::os::unix::fs::symlink;

        let self_cycle = TempDir::new().unwrap();
        symlink("self", self_cycle.path().join("self")).unwrap();
        assert!(matches!(
            materialized_source_tree_hash(self_cycle.path()).unwrap_err(),
            SourceAdmissibilityError::SymlinkCycle { .. }
        ));

        let pair = TempDir::new().unwrap();
        symlink("b", pair.path().join("a")).unwrap();
        symlink("a", pair.path().join("b")).unwrap();
        assert!(matches!(
            materialized_source_tree_hash(pair.path()).unwrap_err(),
            SourceAdmissibilityError::SymlinkCycle { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_depth_and_target_length_limits() {
        use std::os::unix::fs::symlink;

        let deep = TempDir::new().unwrap();
        write_file(deep.path(), "target", b"x");
        symlink("target", deep.path().join("d")).unwrap();
        symlink("d", deep.path().join("c")).unwrap();
        symlink("c", deep.path().join("b")).unwrap();
        symlink("b", deep.path().join("a")).unwrap();
        assert!(matches!(
            validate_source_tree_with_limits(deep.path(), &TINY_LIMITS).unwrap_err(),
            SourceAdmissibilityError::SymlinkDepthExceeded { .. }
        ));

        let long = TempDir::new().unwrap();
        let target = "x".repeat(TINY_LIMITS.max_symlink_target_bytes + 1);
        symlink(&target, long.path().join("link")).unwrap();
        assert!(matches!(
            validate_source_tree_with_limits(long.path(), &TINY_LIMITS).unwrap_err(),
            SourceAdmissibilityError::SymlinkTargetTooLong { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_total_symlink_expansion_limit() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "target", b"x");
        symlink("target", tmp.path().join("a")).unwrap();
        symlink("target", tmp.path().join("b")).unwrap();
        symlink("target", tmp.path().join("c")).unwrap();
        let limits = Limits {
            max_symlink_expansions: 2,
            ..TINY_LIMITS
        };
        assert!(matches!(
            validate_source_tree_with_limits(tmp.path(), &limits).unwrap_err(),
            SourceAdmissibilityError::SymlinkExpansionLimit { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_identity_commits_raw_target_and_entry_kind() {
        use std::os::unix::fs::symlink;

        let first = TempDir::new().unwrap();
        write_file(first.path(), "dir/target", b"same");
        symlink("dir/target", first.path().join("link")).unwrap();
        let first_hash = materialized_source_tree_hash(first.path()).unwrap();

        let spelling = TempDir::new().unwrap();
        write_file(spelling.path(), "dir/target", b"same");
        symlink("./dir/target", spelling.path().join("link")).unwrap();
        let spelling_hash = materialized_source_tree_hash(spelling.path()).unwrap();
        assert_ne!(first_hash, spelling_hash, "raw target spelling is identity");

        let regular = TempDir::new().unwrap();
        write_file(regular.path(), "dir/target", b"same");
        write_file(regular.path(), "link", b"same");
        let regular_hash = materialized_source_tree_hash(regular.path()).unwrap();
        assert_ne!(
            first_hash, regular_hash,
            "a regular file and symlink with the same target content differ"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_identity_is_independent_of_filesystem_creation_order() {
        use std::os::unix::fs::symlink;

        let first = TempDir::new().unwrap();
        write_file(first.path(), "target-a", b"a");
        write_file(first.path(), "target-b", b"b");
        symlink("target-a", first.path().join("link-a")).unwrap();
        symlink("target-b", first.path().join("link-b")).unwrap();

        let second = TempDir::new().unwrap();
        symlink("target-b", second.path().join("link-b")).unwrap();
        write_file(second.path(), "target-b", b"b");
        symlink("target-a", second.path().join("link-a")).unwrap();
        write_file(second.path(), "target-a", b"a");

        assert_eq!(
            materialized_source_tree_hash(first.path()).unwrap(),
            materialized_source_tree_hash(second.path()).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn jspaint_tracky_mouse_symlink_fixture_is_admissible() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        write_file(
            tmp.path(),
            "lib/tracky-mouse/images/placeholder.png",
            b"image",
        );
        write_file(
            tmp.path(),
            "lib/tracky-mouse/core/index.js",
            b"export default {};\n",
        );
        fs::create_dir_all(tmp.path().join("lib/tracky-mouse/website")).unwrap();
        symlink(
            "../images",
            tmp.path().join("lib/tracky-mouse/website/images"),
        )
        .unwrap();
        symlink("../core", tmp.path().join("lib/tracky-mouse/website/core")).unwrap();

        assert!(materialized_source_tree_hash(tmp.path()).is_ok());
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
