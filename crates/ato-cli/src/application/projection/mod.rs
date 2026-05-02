//! Project a frozen blob payload into a session's `runs/<session>/deps/`
//! workspace.
//!
//! The projection must satisfy two invariants:
//!
//! 1. **Modifying the projection MUST NOT corrupt the immutable store.**
//!    Every file the consumer sees is either an independent inode (copy)
//!    or a copy-on-write clone (clonefile / future reflink). Hardlinking
//!    is rejected outright because Node's `realpath` resolution and pnpm's
//!    own symlink farms can be sensitive to inode sharing.
//! 2. **Projection is deterministic about what it does NOT do.** Symlinks
//!    inside the payload are recreated as symlinks in the projection;
//!    we never rewrite them or follow them. Empty directories are
//!    recreated even though they are not part of the blob hash.
//!
//! ## Strategy ladder
//!
//! - **macOS (APFS clonefile)**: a single `clonefile(2)` call clones the
//!   payload tree atomically with block-level copy-on-write. This is the
//!   default on macOS and falls back if the volume isn't APFS or the
//!   target lives on a different filesystem.
//! - **Per-file copy**: walks the tree and uses `fs::copy` per file. On
//!   Linux this opportunistically gets reflink semantics on btrfs/xfs via
//!   `copy_file_range(2)`. Symlinks are recreated, directories are
//!   `mkdir`'d, regular files are copied byte-for-byte.
//! - **Windows**: out of scope for A1; returns an error so callers can
//!   surface the limitation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

/// Strategy actually used to materialize a projection. Reported back to the
/// caller for tracing / cache stats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStrategy {
    /// macOS APFS `clonefile(2)` cloned the whole payload tree.
    Clonefile,
    /// Fell back to per-file `fs::copy`. On Linux this may use reflink
    /// semantics under the hood when the underlying filesystem supports it.
    Copy,
}

/// Outcome of a successful projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionOutcome {
    pub strategy: ProjectionStrategy,
    pub file_count: usize,
    pub symlink_count: usize,
    pub dir_count: usize,
    pub bytes_realized: u64,
}

/// Errors specific to projection that callers may want to inspect.
#[derive(Debug, thiserror::Error)]
pub enum ProjectionError {
    #[error("projection target already exists: {0}")]
    TargetExists(PathBuf),
    #[error("blob payload is not a directory: {0}")]
    PayloadNotDirectory(PathBuf),
    #[error("projection is not supported on this platform yet")]
    Unsupported,
}

/// Projects `payload` (the immutable blob payload directory) into `target`.
///
/// `target` must not already exist. Callers that want to refresh an existing
/// projection must remove the previous target first; this matches the
/// "1 capsule = 1 derivation = 1 projection" rule documented in the A1
/// scope notes.
pub fn project_payload(payload: &Path, target: &Path) -> Result<ProjectionOutcome> {
    if !payload.is_dir() {
        return Err(ProjectionError::PayloadNotDirectory(payload.to_path_buf()).into());
    }
    if target.exists() {
        return Err(ProjectionError::TargetExists(target.to_path_buf()).into());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    #[cfg(target_os = "macos")]
    {
        match try_clonefile(payload, target) {
            Ok(Some(outcome)) => {
                tracing::debug!(
                    payload = %payload.display(),
                    target = %target.display(),
                    "projection used APFS clonefile"
                );
                return Ok(outcome);
            }
            Ok(None) => {
                tracing::debug!(
                    payload = %payload.display(),
                    target = %target.display(),
                    "clonefile not applicable, falling back to copy"
                );
            }
            Err(err) => {
                tracing::warn!(
                    payload = %payload.display(),
                    target = %target.display(),
                    "clonefile failed, falling back to copy: {err}"
                );
                // If clonefile partially populated the target, scrub it so
                // the copy fallback starts from a clean slate.
                let _ = fs::remove_dir_all(target);
            }
        }
    }

    #[cfg(any(unix, target_os = "macos"))]
    {
        return project_via_copy(payload, target);
    }

    #[allow(unreachable_code)]
    Err(ProjectionError::Unsupported.into())
}

#[cfg(target_os = "macos")]
fn try_clonefile(src: &Path, dst: &Path) -> Result<Option<ProjectionOutcome>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let src_c = CString::new(src.as_os_str().as_bytes())
        .context("payload path contains an interior NUL byte")?;
    let dst_c = CString::new(dst.as_os_str().as_bytes())
        .context("target path contains an interior NUL byte")?;

    let rc = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        // EXDEV (cross-device), ENOTSUP (not APFS), and ENOSYS are
        // expected fall-back signals. Treat anything else as a real
        // error so the caller surfaces it.
        match err.raw_os_error() {
            Some(libc::EXDEV) | Some(libc::ENOTSUP) | Some(libc::ENOSYS) => {
                return Ok(None);
            }
            _ => return Err(err).context("clonefile failed"),
        }
    }

    let stats = collect_stats(dst).context("failed to inventory cloned tree")?;
    Ok(Some(ProjectionOutcome {
        strategy: ProjectionStrategy::Clonefile,
        file_count: stats.files,
        symlink_count: stats.symlinks,
        dir_count: stats.dirs,
        bytes_realized: stats.bytes,
    }))
}

fn project_via_copy(src: &Path, dst: &Path) -> Result<ProjectionOutcome> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    let mut stats = WalkStats::default();
    for entry in walkdir::WalkDir::new(src).min_depth(1) {
        let entry = entry.with_context(|| format!("failed to walk {}", src.display()))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir entry is inside src");
        let target = dst.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
            stats.dirs += 1;
        } else if ft.is_symlink() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let link = fs::read_link(entry.path())
                .with_context(|| format!("failed to read symlink {}", entry.path().display()))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link, &target).with_context(|| {
                format!(
                    "failed to recreate symlink {} -> {}",
                    target.display(),
                    link.display()
                )
            })?;
            #[cfg(not(unix))]
            return Err(ProjectionError::Unsupported.into());
            stats.symlinks += 1;
        } else if ft.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let bytes = fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(bytes);
        }
    }
    Ok(ProjectionOutcome {
        strategy: ProjectionStrategy::Copy,
        file_count: stats.files,
        symlink_count: stats.symlinks,
        dir_count: stats.dirs,
        bytes_realized: stats.bytes,
    })
}

#[derive(Default)]
struct WalkStats {
    files: usize,
    symlinks: usize,
    dirs: usize,
    bytes: u64,
}

#[cfg(target_os = "macos")]
fn collect_stats(root: &Path) -> Result<WalkStats> {
    let mut stats = WalkStats::default();
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        let ft = metadata.file_type();
        if ft.is_dir() {
            stats.dirs += 1;
        } else if ft.is_symlink() {
            stats.symlinks += 1;
        } else if ft.is_file() {
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(metadata.len());
        }
    }
    Ok(stats)
}
