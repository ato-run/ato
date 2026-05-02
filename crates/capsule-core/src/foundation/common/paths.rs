use std::path::{Component, Path, PathBuf};

use crate::error::{CapsuleError, Result};

/// Returns the directory containing `manifest_path`.
///
/// Falls back to `"."` when the path has no parent (bare filename).
pub(crate) fn manifest_dir(manifest_path: &Path) -> PathBuf {
    manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

const ENV_ATO_HOME: &str = "ATO_HOME";
const WORKSPACE_STATE_DIR: &str = ".ato";
const WORKSPACE_TMP_DIR: &str = ".ato/tmp";
const WORKSPACE_ARTIFACTS_DIR: &str = ".ato/artifacts";
const WORKSPACE_DERIVED_DIR: &str = ".ato/derived";
const WORKSPACE_FALLBACK_HOME_DIR: &str = ".ato/fallback-home";
const WORKSPACE_INTERNAL_SUBDIRS: &[&str] = &[
    "tmp",
    "artifacts",
    "derived",
    "source-inference",
    "binding",
    "policy",
    "attestations",
    "run",
    "previews",
    "fallback-home",
    "publish",
];

/// Returns the best-effort user home directory without ever falling back to `/tmp`.
///
/// When `dirs::home_dir()` cannot determine the real user home, use a
/// workspace-local `.ato/fallback-home` path so any downstream state stays
/// under the ato-managed `.ato/` tree instead of leaking a `.tmp/` sibling.
pub fn home_dir_or_workspace_tmp() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".").join(WORKSPACE_FALLBACK_HOME_DIR))
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

    let home = dirs::home_dir()
        .ok_or_else(|| CapsuleError::Config("Failed to determine home directory".to_string()))?;
    Ok(home.join(".ato"))
}

/// Returns the best-effort ato home directory without ever falling back to `/tmp`.
///
/// When the real home directory cannot be determined, use a workspace-local
/// `.ato/fallback-home/.ato` path so CLI and core fallbacks remain strictly
/// inside the workspace `.ato/` tree.
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

/// Returns the shared cache directory for ephemeral CLI-managed artifacts.
///
/// Layout: `~/.ato/cache`
pub fn ato_cache_dir() -> PathBuf {
    nacelle_home_dir_or_workspace_tmp().join("cache")
}

/// Returns the shared run state directory for ephemeral CLI-managed artifacts.
///
/// Layout: `~/.ato/runs`
pub fn ato_runs_dir() -> PathBuf {
    nacelle_home_dir_or_workspace_tmp().join("runs")
}

/// Returns the durable execution receipt directory.
///
/// Layout: `~/.ato/executions`
pub fn ato_executions_dir() -> PathBuf {
    nacelle_home_dir_or_workspace_tmp().join("executions")
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

pub fn path_contains_workspace_state_dir(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(value) => value == WORKSPACE_STATE_DIR,
        _ => false,
    })
}

pub fn path_contains_workspace_internal_subtree(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if components
        .iter()
        .any(|component| component == WORKSPACE_STATE_DIR)
        && components
            .last()
            .is_some_and(|component| component == WORKSPACE_STATE_DIR)
    {
        return true;
    }

    components.windows(2).any(|window| {
        window[0] == WORKSPACE_STATE_DIR && WORKSPACE_INTERNAL_SUBDIRS.contains(&window[1].as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        path_contains_workspace_internal_subtree, path_contains_workspace_state_dir,
        WORKSPACE_FALLBACK_HOME_DIR,
    };
    use std::path::Path;

    #[test]
    fn detects_workspace_state_dir_components() {
        assert!(path_contains_workspace_state_dir(Path::new(
            "project/.ato/tmp/run"
        )));
        assert!(path_contains_workspace_state_dir(Path::new(
            ".ato/source-inference"
        )));
        assert!(!path_contains_workspace_state_dir(Path::new(
            "project/source"
        )));
    }

    #[test]
    fn detects_internal_workspace_subtrees_without_matching_store_paths() {
        assert!(path_contains_workspace_internal_subtree(Path::new(
            "project/.ato/tmp/run"
        )));
        assert!(path_contains_workspace_internal_subtree(Path::new(
            "project/.ato"
        )));
        assert!(path_contains_workspace_internal_subtree(Path::new(
            "project/.ato/artifacts"
        )));
        assert!(!path_contains_workspace_internal_subtree(Path::new(
            "/Users/test/.ato/store/pkg"
        )));
    }

    #[test]
    fn fallback_home_lives_under_workspace_ato_dir() {
        // Regression guard: the home-dir fallback must never leak a `.tmp/`
        // sibling in the workspace. Every fallback path must stay inside
        // `.ato/fallback-home/...`.
        assert_eq!(WORKSPACE_FALLBACK_HOME_DIR, ".ato/fallback-home");
        assert!(path_contains_workspace_internal_subtree(Path::new(
            "project/.ato/fallback-home"
        )));
        assert!(path_contains_workspace_internal_subtree(Path::new(
            "project/.ato/fallback-home/.ato/cache"
        )));
    }
}
