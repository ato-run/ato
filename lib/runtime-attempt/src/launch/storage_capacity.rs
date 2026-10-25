//! Shared physical free-space observation. This is a preflight, not a disk
//! quota/reservation or proof about a Docker daemon's separate image store.

use anyhow::{Context, Result};
use std::path::Path;

pub const STORAGE_INSUFFICIENT_SPACE: &str = "runner_workroot_insufficient_space";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsufficientStorageSpace {
    pub required_bytes: u64,
    pub available_bytes: u64,
}

impl std::fmt::Display for InsufficientStorageSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{STORAGE_INSUFFICIENT_SPACE}: requires at least {} bytes; {} available",
            self.required_bytes, self.available_bytes
        )
    }
}
impl std::error::Error for InsufficientStorageSpace {}

pub fn available_bytes(path: &Path) -> Result<u64> {
    // The Run's directory may not exist yet. Observe its existing filesystem
    // without creating a directory merely to discover that the disk is full.
    let mut ancestor = path;
    loop {
        match std::fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => {
                return fs2::available_space(ancestor)
                    .context("cannot observe Runner filesystem capacity");
            }
            Ok(_) => anyhow::bail!("Runner storage parent is not a directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .context("Runner storage has no existing filesystem parent")?;
            }
            Err(error) => return Err(error).context("cannot inspect Runner storage parent"),
        }
    }
}

pub fn require_capacity(path: &Path, required_bytes: u64) -> Result<()> {
    check_capacity(available_bytes(path)?, required_bytes).map_err(Into::into)
}

pub fn check_capacity(
    available_bytes: u64,
    required_bytes: u64,
) -> Result<(), InsufficientStorageSpace> {
    if available_bytes < required_bytes {
        return Err(InsufficientStorageSpace {
            required_bytes,
            available_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_reports_exact_observations_without_changing_a_quota_or_creating_storage() {
        assert_eq!(
            check_capacity(0, 1).unwrap_err(),
            InsufficientStorageSpace {
                required_bytes: 1,
                available_bytes: 0
            }
        );
        assert!(check_capacity(100, 100).is_ok());
        assert!(check_capacity(100, 101).is_err());
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing/lease/workspace");
        assert!(available_bytes(&missing).is_ok());
        assert!(!missing.exists());
        std::fs::write(root.path().join("file"), "x").unwrap();
        assert!(available_bytes(&root.path().join("file/child")).is_err());
    }
}
