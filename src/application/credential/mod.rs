//! Shared credential storage layer.
//!
//! This module provides a common `CredentialBackend` trait and a priority
//! chain used by both `secrets/*` and (eventually) `auth/*` domains. A single
//! namespace string (`"secrets/default"`, `"auth/session"`, …) identifies the
//! storage domain, so one backend implementation can serve every credential
//! kind.
//!
//! Phase 1 scope: only `secrets` uses this module. `auth` migration is
//! Phase 2.

pub(crate) mod backend;
pub(crate) mod chain;
pub(crate) mod config;

pub(crate) use backend::{
    load_identity_bytes, AgeFileBackend, BackendEntry, CredentialKey, EnvBackend,
    LegacyKeychainBackend, MemoryBackend,
};
pub(crate) use chain::BackendChain;

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

/// Write a file with `0600` permissions, using an atomic temp-rename.
///
/// Shared utility used by every credential backend and by the session-key
/// flow. Historically lived in `secrets/store.rs`; lifted here so that
/// `auth` can depend on it without going through the `secrets` module.
pub(crate) fn write_secure_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("credential path must have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    let tmp_path = path.with_extension("tmp");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp_path)
            .with_context(|| format!("failed to open {}", tmp_path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        file.flush()
            .with_context(|| format!("failed to flush {}", tmp_path.display()))?;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", tmp_path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("failed to open {}", tmp_path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        file.flush()
            .with_context(|| format!("failed to flush {}", tmp_path.display()))?;
    }

    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to rename {} → {}",
            tmp_path.display(),
            path.display()
        )
    })
}
