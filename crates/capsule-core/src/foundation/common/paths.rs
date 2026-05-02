use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Returns the durable local user state directory.
///
/// Layout: `~/.ato/state`
pub fn ato_state_dir() -> PathBuf {
    nacelle_home_dir_or_workspace_tmp().join("state")
}

/// Returns the immutable artifact store root.
///
/// Layout: `~/.ato/store`
pub fn ato_store_dir() -> PathBuf {
    nacelle_home_dir_or_workspace_tmp().join("store")
}

/// Returns the local trust metadata root.
///
/// Layout: `~/.ato/trust`
pub fn ato_trust_dir() -> PathBuf {
    nacelle_home_dir_or_workspace_tmp().join("trust")
}

pub fn ato_store_blobs_dir() -> PathBuf {
    ato_store_dir().join("blobs")
}

pub fn ato_store_refs_dir() -> PathBuf {
    ato_store_dir().join("refs")
}

pub fn ato_store_meta_dir() -> PathBuf {
    ato_store_dir().join("meta")
}

pub fn ato_store_attestations_dir() -> PathBuf {
    ato_store_dir().join("attestations")
}

pub fn ato_trust_roots_dir() -> PathBuf {
    ato_trust_dir().join("roots")
}

pub fn ato_trust_policies_dir() -> PathBuf {
    ato_trust_dir().join("policies")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtoRunLayout {
    pub root: PathBuf,
    pub session_json: PathBuf,
    pub workspace: PathBuf,
    pub workspace_source: PathBuf,
    pub workspace_build: PathBuf,
    pub deps: PathBuf,
    pub cache: PathBuf,
    pub tmp: PathBuf,
    pub log: PathBuf,
}

impl AtoRunLayout {
    pub fn for_root(root: PathBuf) -> Self {
        let workspace = root.join("workspace");
        Self {
            session_json: root.join("session.json"),
            workspace_source: workspace.join("source"),
            workspace_build: workspace.join("build"),
            workspace,
            deps: root.join("deps"),
            cache: root.join("cache"),
            tmp: root.join("tmp"),
            log: root.join("log"),
            root,
        }
    }
}

/// Returns the path layout for one isolated A0 run session.
///
/// The token is for filesystem uniqueness only and does not participate in
/// artifact identity.
pub fn ato_run_layout(kind: &str) -> AtoRunLayout {
    let kind = sanitize_run_kind(kind);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let random = rand::random::<u64>();
    AtoRunLayout::for_root(ato_runs_dir().join(format!("{kind}-{millis:x}-{random:x}")))
}

fn sanitize_run_kind(kind: &str) -> String {
    let sanitized = kind
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "run".to_string()
    } else {
        sanitized.to_string()
    }
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

    #[test]
    fn ato_layout_helpers_never_use_system_tmp_fallbacks() {
        for path in [
            super::ato_runs_dir(),
            super::ato_state_dir(),
            super::ato_store_dir(),
            super::ato_trust_dir(),
            super::ato_store_blobs_dir(),
            super::ato_store_refs_dir(),
            super::ato_store_attestations_dir(),
            super::ato_trust_roots_dir(),
            super::ato_trust_policies_dir(),
        ] {
            let rendered = path.to_string_lossy();
            assert!(
                !rendered.starts_with("/tmp") && !rendered.starts_with("/var/tmp"),
                "{rendered} must stay out of system tmp"
            );
        }
    }

    #[test]
    fn run_layout_uses_kind_prefixed_root() {
        let layout = super::ato_run_layout("provider/npm");
        let file_name = layout
            .root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        assert!(file_name.starts_with("provider-npm-"));
        assert_eq!(
            layout.workspace_source,
            layout.root.join("workspace/source")
        );
        assert_eq!(layout.deps, layout.root.join("deps"));
    }
}
