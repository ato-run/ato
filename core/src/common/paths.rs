use anyhow::{Context, Result};
use std::path::PathBuf;

/// Returns the root directory used by nacelle/capsule for per-user state.
///
/// We intentionally standardize on `~/.ato` for runtime caches.
pub fn nacelle_home_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to determine home directory")?;
    Ok(home.join(".ato"))
}

/// Returns the toolchain cache directory.
///
/// Layout: `~/.ato/toolchains`
pub fn toolchain_cache_dir() -> Result<PathBuf> {
    Ok(nacelle_home_dir()?.join("toolchains"))
}
