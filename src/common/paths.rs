use anyhow::{Context, Result};
use std::path::PathBuf;

/// Returns the root directory used by capsuled/nacelle for per-user state.
///
/// We intentionally standardize on `~/.capsuled` across components.
pub fn capsuled_home_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home.join(".capsuled"))
}

/// Returns the toolchain cache directory.
///
/// Layout: `~/.capsuled/toolchain`
pub fn toolchain_cache_dir() -> Result<PathBuf> {
    Ok(capsuled_home_dir()?.join("toolchain"))
}
