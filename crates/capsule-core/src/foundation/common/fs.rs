//! Shared filesystem helpers.
//!
//! Centralizes the recursive directory-copy primitive that used to be
//! re-implemented across the workspace with drifting symlink semantics
//! (some copies embedded the symlink *target contents*, which lets a
//! source tree containing `cfg -> ~/.ssh/config` exfiltrate foreign
//! files into a staged capsule). Every caller now states its symlink
//! policy explicitly.

use std::path::Path;

/// How [`copy_dir_recursive`] treats symlinks found in the source tree.
///
/// Symlinks are never followed: copying the link *target contents* would
/// let a hostile source tree embed files from outside the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkPolicy {
    /// Drop symlinks entirely. The fail-closed choice for staging or
    /// caching source trees that may point outside themselves.
    Skip,
    /// Recreate the symlink with its original (literal, unresolved)
    /// target at the destination. Unix only; errors on other platforms.
    Preserve,
}

/// Recursively copies the directory tree at `src` into `dst`.
///
/// - Directories are created with `create_dir_all`; entries are visited
///   in file-name order so copies are deterministic.
/// - Regular files are copied with [`std::fs::copy`], which preserves
///   permission bits (including the executable bit) on Unix.
/// - Symlinks follow the explicit [`SymlinkPolicy`] and are never followed.
/// - Other special files (sockets, FIFOs, devices) are skipped.
pub fn copy_dir_recursive(src: &Path, dst: &Path, symlinks: SymlinkPolicy) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let mut entries = std::fs::read_dir(src)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path, symlinks)?;
        } else if file_type.is_symlink() {
            match symlinks {
                SymlinkPolicy::Skip => continue,
                SymlinkPolicy::Preserve => recreate_symlink(&entry.path(), &dst_path)?,
            }
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn recreate_symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    let target = std::fs::read_link(src)?;
    std::os::unix::fs::symlink(target, dst)
}

#[cfg(not(unix))]
fn recreate_symlink(src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("cannot preserve symlink {} on this platform", src.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_nested_files_and_directories() {
        let src = tempfile::tempdir().expect("src dir");
        let dst = tempfile::tempdir().expect("dst dir");
        std::fs::create_dir_all(src.path().join("nested/deep")).unwrap();
        std::fs::write(src.path().join("top.txt"), b"top").unwrap();
        std::fs::write(src.path().join("nested/deep/leaf.txt"), b"leaf").unwrap();

        let out = dst.path().join("copy");
        copy_dir_recursive(src.path(), &out, SymlinkPolicy::Skip).unwrap();

        assert_eq!(std::fs::read(out.join("top.txt")).unwrap(), b"top");
        assert_eq!(
            std::fs::read(out.join("nested/deep/leaf.txt")).unwrap(),
            b"leaf"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copies_preserve_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let src = tempfile::tempdir().expect("src dir");
        let dst = tempfile::tempdir().expect("dst dir");
        let script = src.path().join("bootstrap.sh");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out = dst.path().join("copy");
        copy_dir_recursive(src.path(), &out, SymlinkPolicy::Skip).unwrap();

        let mode = std::fs::metadata(out.join("bootstrap.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "executable bit must survive the copy");
    }

    #[cfg(unix)]
    #[test]
    fn skip_policy_drops_symlinks_instead_of_embedding_target_contents() {
        let outside = tempfile::tempdir().expect("outside dir");
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, b"do not embed").unwrap();

        let src = tempfile::tempdir().expect("src dir");
        std::os::unix::fs::symlink(&secret, src.path().join("cfg")).unwrap();
        std::fs::write(src.path().join("kept.txt"), b"kept").unwrap();

        let dst = tempfile::tempdir().expect("dst dir");
        let out = dst.path().join("copy");
        copy_dir_recursive(src.path(), &out, SymlinkPolicy::Skip).unwrap();

        assert!(
            out.join("cfg").symlink_metadata().is_err(),
            "symlink must not be copied or materialized"
        );
        assert_eq!(std::fs::read(out.join("kept.txt")).unwrap(), b"kept");
    }

    #[cfg(unix)]
    #[test]
    fn preserve_policy_recreates_symlink_with_literal_target() {
        let src = tempfile::tempdir().expect("src dir");
        std::fs::write(src.path().join("real.txt"), b"real").unwrap();
        std::os::unix::fs::symlink("real.txt", src.path().join("link")).unwrap();

        let dst = tempfile::tempdir().expect("dst dir");
        let out = dst.path().join("copy");
        copy_dir_recursive(src.path(), &out, SymlinkPolicy::Preserve).unwrap();

        let link = out.join("link");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::PathBuf::from("real.txt")
        );
    }
}
