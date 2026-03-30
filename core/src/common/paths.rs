use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const ENV_ATO_HOME: &str = "ATO_HOME";
const WORKSPACE_STATE_DIR: &str = ".ato";
const WORKSPACE_TMP_DIR: &str = ".ato/tmp";
const WORKSPACE_ARTIFACTS_DIR: &str = ".ato/artifacts";
const WORKSPACE_DERIVED_DIR: &str = ".ato/derived";

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

/// Returns the runtime cache directory.
///
/// Layout: `~/.ato/runtimes`
pub fn runtime_cache_dir() -> Result<PathBuf> {
    Ok(nacelle_home_dir()?.join("runtimes"))
}

/// Returns the durable engine cache directory.
///
/// Layout: `~/.ato/engines`
pub fn engine_cache_dir() -> Result<PathBuf> {
    Ok(nacelle_home_dir()?.join("engines"))
}

/// Returns the workspace-local directory for generated compatibility artifacts.
pub(crate) fn workspace_derived_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(WORKSPACE_DERIVED_DIR)
}

/// Returns the workspace-local root for mutable ato state.
pub fn workspace_state_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(WORKSPACE_STATE_DIR)
}

/// Returns the workspace-local root for temporary ato state.
pub fn workspace_tmp_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(WORKSPACE_TMP_DIR)
}

/// Returns the workspace-local root for generated runtime artifacts.
pub fn workspace_artifacts_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(WORKSPACE_ARTIFACTS_DIR)
}
