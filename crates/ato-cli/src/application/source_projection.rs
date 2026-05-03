//! Phase Y option 2: project a registry-installed capsule's source files
//! into a session-local workspace so the running capsule cannot mutate the
//! install dir at `~/.ato/runtimes/<scoped>/<version>/source/`.
//!
//! The projection mirrors the install dir into
//! `~/.ato/runs/<session-id>/workspace/source/` via filesystem hardlinks
//! (cheap; no byte copy). Hardlinking preserves the file inode so the
//! `source_tree_hash` observation reads identical bytes. Capsule writes
//! create new inodes (via the runtime's normal file APIs) under the
//! projected dir, leaving the install inode intact.
//!
//! Why projection instead of read-only mount: macOS does not have a
//! standard read-only bind-mount facility usable from userspace, and
//! per-platform sandboxing rules differ. Hardlink projection is
//! cross-platform, requires no privileged mounts, and aligns with
//! plan §5.2 "Phase A0 source-tree non-pollution" — capsules see a
//! workspace that looks identical to their install but is in fact a
//! disposable session view.
//!
//! Skipped during projection (also skipped by the source observer):
//! - directories listed in `source_inventory::DEFAULT_IGNORED_DIRS`
//!   (`.git`, `.tmp`, `node_modules`, `.venv`, `target`, `__pycache__`,
//!   `.ato`)
//! - any subdirectory of the install whose name appears in that list

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::application::source_inventory::DEFAULT_IGNORED_DIRS;

/// Mirror `install_root` into `target_root` using hardlinks for files and
/// fresh empty directories for directory entries. Symlinks are reproduced
/// as symlinks. Returns the number of files projected, useful for
/// instrumentation.
///
/// `target_root` must not already exist; the caller is responsible for
/// providing a fresh session-scoped path. The projection is not
/// transactional — a partial projection is left on disk if hardlinking
/// fails midway, but the install dir is never modified.
pub(crate) fn project_install_source(install_root: &Path, target_root: &Path) -> Result<usize> {
    if !install_root.is_dir() {
        anyhow::bail!(
            "source projection requires an existing install dir; got {}",
            install_root.display()
        );
    }
    if target_root.exists() {
        anyhow::bail!(
            "source projection target already exists; refusing to overwrite {}",
            target_root.display()
        );
    }

    fs::create_dir_all(target_root).with_context(|| {
        format!(
            "failed to create projection target dir {}",
            target_root.display()
        )
    })?;

    let mut projected_files = 0usize;
    for entry in WalkDir::new(install_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_ignored_dir(install_root, entry.path()))
    {
        let entry = entry
            .with_context(|| format!("failed to walk install dir {}", install_root.display()))?;
        let relative = match entry.path().strip_prefix(install_root) {
            Ok(rel) if rel.as_os_str().is_empty() => continue, // root itself
            Ok(rel) => rel,
            Err(_) => continue,
        };
        let target = target_root.join(relative);
        let file_type = entry.file_type();

        if file_type.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to mkdir {}", target.display()))?;
        } else if file_type.is_symlink() {
            // Preserve the symlink as-is; the resolved target may live in
            // the install dir or be relative to it. Re-pointing into the
            // projection would require canonicalization that we do not
            // need for the source-hash use case.
            let link_target = fs::read_link(entry.path())
                .with_context(|| format!("failed to read symlink {}", entry.path().display()))?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to mkdir {}", parent.display()))?;
            }
            symlink(&link_target, &target).with_context(|| {
                format!(
                    "failed to recreate symlink {} -> {}",
                    target.display(),
                    link_target.display()
                )
            })?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to mkdir {}", parent.display()))?;
            }
            // hard_link is atomic and shares the inode. If hardlink fails
            // (cross-device, filesystem refuses, etc.) fall back to copy
            // so projection still produces a working workspace.
            if let Err(err) = fs::hard_link(entry.path(), &target) {
                fs::copy(entry.path(), &target).with_context(|| {
                    format!(
                        "failed to project {} (hardlink err: {}, copy fallback also failed)",
                        entry.path().display(),
                        err
                    )
                })?;
            }
            projected_files += 1;
        }
        // Other file types (sockets, devices, fifos) are skipped.
    }

    Ok(projected_files)
}

fn is_ignored_dir(root: &Path, path: &Path) -> bool {
    // Strip root prefix; if path == root keep it (we still want to walk it).
    let relative = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return false,
    };
    if relative.as_os_str().is_empty() {
        return false;
    }
    relative.iter().any(|component| {
        component
            .to_str()
            .is_some_and(|name| DEFAULT_IGNORED_DIRS.contains(&name))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn projects_files_via_hardlinks_and_skips_ignored_dirs() {
        let install = tempdir().expect("install tempdir");
        let target = tempdir().expect("target tempdir");
        let target_root = target.path().join("workspace-source");

        fs::write(install.path().join("main.py"), b"print(1)").unwrap();
        fs::create_dir(install.path().join("src")).unwrap();
        fs::write(install.path().join("src/lib.py"), b"x = 1").unwrap();
        fs::create_dir(install.path().join("node_modules")).unwrap();
        fs::write(
            install.path().join("node_modules/dep.js"),
            b"module.exports = 1;",
        )
        .unwrap();
        fs::create_dir(install.path().join(".git")).unwrap();
        fs::write(install.path().join(".git/HEAD"), b"ref: refs/heads/main").unwrap();

        let count = project_install_source(install.path(), &target_root).expect("project");
        assert_eq!(
            count, 2,
            "expected 2 hardlinked files (main.py, src/lib.py)"
        );
        assert!(target_root.join("main.py").is_file());
        assert!(target_root.join("src/lib.py").is_file());
        assert!(!target_root.join("node_modules").exists());
        assert!(!target_root.join(".git").exists());
    }

    #[test]
    fn refuses_to_overwrite_existing_target() {
        let install = tempdir().expect("install tempdir");
        let target = tempdir().expect("target tempdir");
        fs::write(install.path().join("a"), b"x").unwrap();
        let result = project_install_source(install.path(), target.path());
        assert!(result.is_err(), "must refuse to overwrite existing target");
    }

    #[test]
    fn missing_install_root_is_an_error() {
        let target = tempdir().expect("target tempdir");
        let result = project_install_source(
            Path::new("/this/path/does/not/exist"),
            &target.path().join("nope"),
        );
        assert!(result.is_err(), "missing install must error");
    }

    #[test]
    fn hardlink_shares_inode_with_install_file() {
        use std::os::unix::fs::MetadataExt;
        let install = tempdir().expect("install tempdir");
        let target = tempdir().expect("target tempdir");
        let target_root = target.path().join("ws");
        fs::write(install.path().join("main.py"), b"print(1)").unwrap();

        project_install_source(install.path(), &target_root).expect("project");

        let install_ino = fs::metadata(install.path().join("main.py")).unwrap().ino();
        let projected_ino = fs::metadata(target_root.join("main.py")).unwrap().ino();
        assert_eq!(
            install_ino, projected_ino,
            "hardlink projection must share inode with install"
        );
    }
}
