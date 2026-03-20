use anyhow::{Context, Result};
use std::path::PathBuf;

const ENV_ATO_HOME: &str = "ATO_HOME";

/// Returns the best-effort user home directory without falling back to `/tmp`.
pub fn home_dir_or_workspace_tmp() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".").join(".tmp"))
}

/// Returns the root directory used by nacelle/capsule for per-user state.
///
/// We intentionally standardize on `~/.ato` for runtime caches.
pub fn nacelle_home_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(ENV_ATO_HOME) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }

    let home = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home.join(".ato"))
}

/// Returns the best-effort ato home directory without ever falling back to `/tmp`.
///
/// When the real home directory cannot be determined, use a workspace-local
/// `.tmp/.ato` path so CLI and core fallbacks stay within the repository rules.
pub fn nacelle_home_dir_or_workspace_tmp() -> PathBuf {
    nacelle_home_dir().unwrap_or_else(|_| home_dir_or_workspace_tmp().join(".ato"))
}

/// Returns the toolchain cache directory.
///
/// Layout: `~/.ato/toolchains`
pub fn toolchain_cache_dir() -> Result<PathBuf> {
    Ok(nacelle_home_dir()?.join("toolchains"))
}
