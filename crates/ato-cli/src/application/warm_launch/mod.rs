//! Warm-launch materialization helpers.
//!
//! Provides content/identity-addressed projection roots so source projection
//! and build/dependency materialization are only executed once per unique
//! (identity, target, platform, toolchain) tuple.
//!
//! Layout under ATO_HOME:
//! ```text
//! ~/.ato/projections/v1/<full_key>/
//!   source/                     # hardlink/clonefile projection of install dir
//!   build/                      # build outputs (e.g. .next/)
//!   .ato/state/materializations.json
//!   .complete                   # atomic commit marker
//!   <full_key>.lock             # per-projection advisory lock (creation guard)
//! ```
//!
//! A projection is reused when `.complete` is present. A directory without
//! `.complete` is "stale-partial" (crash during creation) and is renamed aside
//! so a fresh projection can be created in its place.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blake3::Hasher;
use fs2::FileExt;

/// Content/identity-addressed key for a projection root.
///
/// Computed from: resolver algorithm version, install workspace canonical
/// identity, manifest digest, lock digest, target label, platform, and
/// toolchain resolution identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectionKey {
    /// Full 64-char lowercase hex (blake3 output).
    pub(crate) full_hex: String,
}

impl ProjectionKey {
    const RESOLVER_VERSION: &'static str = "v1";

    /// Derive a `ProjectionKey` from the identity inputs.
    pub(crate) fn compute(
        install_identity: &str,
        manifest_digest: &str,
        lock_digest: Option<&str>,
        target_label: &str,
        platform: &str,
        toolchain_fingerprint: &str,
    ) -> Self {
        let mut hasher = Hasher::new();
        update_field(&mut hasher, "resolver_version", Self::RESOLVER_VERSION);
        update_field(&mut hasher, "install_identity", install_identity);
        update_field(&mut hasher, "manifest_digest", manifest_digest);
        update_field(
            &mut hasher,
            "lock_digest",
            lock_digest.unwrap_or("none"),
        );
        update_field(&mut hasher, "target_label", target_label);
        update_field(&mut hasher, "platform", platform);
        update_field(&mut hasher, "toolchain_fingerprint", toolchain_fingerprint);
        Self {
            full_hex: hasher.finalize().to_hex().to_string(),
        }
    }

    /// Short human-readable label: `"v1:<hex12>"`. Use in logs and display only.
    pub(crate) fn display_key(&self) -> String {
        format!("v1:{}", &self.full_hex[..12])
    }

    /// Full 64-char hex suitable for directory names and digest inputs.
    pub(crate) fn full_key(&self) -> &str {
        &self.full_hex
    }
}

/// Status returned by [`resolve_identity_projection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectionStatus {
    /// Projection did not exist; source files were projected into a new root.
    Created,
    /// A complete, previously-created projection was found and reused.
    Reused,
    /// A partial (incomplete) projection directory was found and renamed aside;
    /// a fresh projection was created in its place.
    StalePartial,
}

/// Result of resolving (or creating) a content-addressed projection root.
pub(crate) struct ProjectionResolution {
    pub(crate) projection_key: ProjectionKey,
    /// `<projections_dir>/v1/<full_key>/source` — projected source files.
    pub(crate) source_root: PathBuf,
    /// `<projections_dir>/v1/<full_key>/build` — build output directory.
    pub(crate) build_root: PathBuf,
    /// How the projection was resolved.
    pub(crate) status: ProjectionStatus,
}

/// Granularity of a lifecycle execution step.
#[allow(dead_code)] // variants used for instrumentation; some may be unused at call sites yet
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleStepKind {
    /// `npm install`, `uv sync`, `cargo fetch`, etc.
    DependencyMaterialization,
    /// Compile, bundle, generate assets.
    BuildMaterialization,
    /// Schema migration, state init.
    StateEnsure,
    /// Start supporting services (DB, cache, …).
    ProviderStartup,
    /// Start the capsule's main process.
    AppStartup,
}

// ---------------------------------------------------------------------------
// Core resolve function
// ---------------------------------------------------------------------------

/// Resolve (or create) the content-addressed projection for the given key.
///
/// ## Protocol
/// 1. Acquire a per-key advisory file lock to serialize concurrent projection
///    creation across processes.
/// 2. Check for `.complete` marker → return `Reused` immediately.
/// 3. If the directory exists *without* `.complete`, rename it to a `.stale-*`
///    aside and proceed as if fresh.
/// 4. Project `install_workspace` into a temp dir, write `.complete`, then
///    atomic-rename to the final path.
/// 5. Return `ProjectionResolution` with the appropriate status.
pub(crate) fn resolve_identity_projection(
    key: &ProjectionKey,
    install_workspace: &Path,
) -> Result<ProjectionResolution> {
    let projections_dir = capsule_core::common::paths::ato_projections_dir();
    let version_dir = projections_dir.join("v1");
    fs::create_dir_all(&version_dir)
        .with_context(|| format!("failed to create projections dir {}", version_dir.display()))?;

    let root = version_dir.join(key.full_key());
    let complete_marker = root.join(".complete");
    let source_root = root.join("source");
    let build_root = root.join("build");

    // Per-projection advisory lock to prevent two processes from creating the
    // same projection simultaneously. Failures are non-fatal (exotic FS, etc.)
    // — we fall through to the creation logic without exclusion.
    let _proj_lock = acquire_projection_lock(&version_dir, key.full_key());

    if complete_marker.exists() {
        return Ok(ProjectionResolution {
            projection_key: key.clone(),
            source_root,
            build_root,
            status: ProjectionStatus::Reused,
        });
    }

    let mut status = ProjectionStatus::Created;

    if root.exists() {
        // Partial projection: crash mid-creation left a dir without .complete.
        // Rename it aside so we can create a fresh one.
        let stale_path = version_dir.join(format!(
            ".stale-{}-{}",
            key.full_key(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        ));
        fs::rename(&root, &stale_path).with_context(|| {
            format!(
                "failed to rename stale projection {} -> {}",
                root.display(),
                stale_path.display()
            )
        })?;
        status = ProjectionStatus::StalePartial;
    }

    // Project into a temp dir first, then atomic-rename to final path so
    // there is never a moment where the final path exists without .complete.
    let temp_root = version_dir.join(format!(
        ".tmp-{}-{}",
        key.full_key(),
        std::process::id()
    ));
    let temp_source = temp_root.join("source");

    // Clean up any leftover temp dir from a previous crashed process.
    if temp_root.exists() {
        let _ = fs::remove_dir_all(&temp_root);
    }

    fs::create_dir_all(&temp_source).with_context(|| {
        format!(
            "failed to create temp projection dir {}",
            temp_source.display()
        )
    })?;

    crate::application::source_projection::project_install_source(install_workspace, &temp_source)
        .with_context(|| {
            format!(
                "failed to project install source {} -> {}",
                install_workspace.display(),
                temp_source.display()
            )
        })?;

    // Write .complete marker inside the temp dir *before* the rename so the
    // marker is either fully present or absent — never partially written.
    fs::write(temp_root.join(".complete"), b"ok").with_context(|| {
        format!(
            "failed to write .complete marker in {}",
            temp_root.display()
        )
    })?;

    fs::rename(&temp_root, &root).with_context(|| {
        format!(
            "failed to atomic-rename projection {} -> {}",
            temp_root.display(),
            root.display()
        )
    })?;

    Ok(ProjectionResolution {
        projection_key: key.clone(),
        source_root,
        build_root,
        status,
    })
}

// ---------------------------------------------------------------------------
// Projection-level advisory lock
// ---------------------------------------------------------------------------

struct ProjectionLock {
    _file: File,
}

fn acquire_projection_lock(version_dir: &Path, full_key: &str) -> Option<ProjectionLock> {
    let lock_path = version_dir.join(format!("{}.lock", full_key));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .ok()?;
    file.lock_exclusive().ok()?;
    Some(ProjectionLock { _file: file })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn update_field(hasher: &mut Hasher, key: &str, value: &str) {
    hasher.update(&(key.len() as u64).to_le_bytes());
    hasher.update(key.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

/// Compute a blake3 hex digest of a file's contents.
pub(crate) fn compute_file_digest(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Compute a blake3 hex digest of the first lock file found in `workspace`.
/// Returns `None` if no lock file is found.
pub(crate) fn compute_lock_digest(workspace: &Path) -> Option<String> {
    const LOCK_FILES: &[&str] = &[
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "uv.lock",
        "Cargo.lock",
        "Gemfile.lock",
        "go.sum",
    ];
    for name in LOCK_FILES {
        let p = workspace.join(name);
        if let Ok(bytes) = std::fs::read(&p) {
            return Some(blake3::hash(&bytes).to_hex().to_string());
        }
    }
    None
}

/// Returns the current OS/arch pair, e.g. `"darwin-arm64"`.
pub(crate) fn current_platform() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "unknown"
    };
    format!("{}-{}", os, arch)
}

/// Build the logical cwd string for a registry capsule projection.
///
/// Uses the full 64-char key (never the display key) so collisions between
/// 12-char prefixes cannot cause incorrect session reuse.
pub(crate) fn make_logical_cwd(key: &ProjectionKey, workdir_relative: &Path) -> String {
    let rel = workdir_relative.to_string_lossy();
    if rel.is_empty() || rel == "." {
        format!("projection:{}/source", key.full_key())
    } else {
        format!("projection:{}/source:{}", key.full_key(), rel)
    }
}

impl ProjectionResolution {
    /// A fallback resolution that points back at `install_workspace` as if it
    /// were the source root. Used when warm-launch projection fails and the
    /// caller wants to proceed with the install dir directly (degraded path).
    pub(crate) fn fallback(install_workspace: &Path) -> Self {
        use std::sync::OnceLock;
        static FALLBACK_KEY: OnceLock<ProjectionKey> = OnceLock::new();
        let key = FALLBACK_KEY.get_or_init(|| ProjectionKey {
            full_hex: "0".repeat(64),
        });
        Self {
            projection_key: key.clone(),
            source_root: install_workspace.to_path_buf(),
            build_root: install_workspace.join("build"),
            status: ProjectionStatus::Created,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> ProjectionKey {
        ProjectionKey::compute(
            "~/.ato/runtimes/publisher/my-app/1.0.0",
            "deadbeef",
            Some("cafebabe"),
            "app",
            "darwin-arm64",
            "node:20|darwin-arm64",
        )
    }

    #[test]
    fn projection_key_is_stable_for_same_inputs() {
        let a = sample_key();
        let b = sample_key();
        assert_eq!(a.full_hex, b.full_hex);
        assert_eq!(a.full_hex.len(), 64);
        assert!(a.display_key().starts_with("v1:"));
    }

    #[test]
    fn projection_key_differs_with_target_label() {
        let a = ProjectionKey::compute(
            "install/dir",
            "aabbcc",
            None,
            "app",
            "darwin-arm64",
            "node:20",
        );
        let b = ProjectionKey::compute(
            "install/dir",
            "aabbcc",
            None,
            "worker",
            "darwin-arm64",
            "node:20",
        );
        assert_ne!(a.full_hex, b.full_hex);
    }

    #[test]
    fn projection_key_differs_with_manifest_digest() {
        let a = ProjectionKey::compute("install/dir", "aabbcc", None, "app", "linux-x86_64", "node:20");
        let b = ProjectionKey::compute("install/dir", "ddeeff", None, "app", "linux-x86_64", "node:20");
        assert_ne!(a.full_hex, b.full_hex);
    }

    #[test]
    fn projection_key_differs_with_lock_digest() {
        let a = ProjectionKey::compute("install/dir", "aabbcc", Some("lock1"), "app", "linux-x86_64", "node:20");
        let b = ProjectionKey::compute("install/dir", "aabbcc", Some("lock2"), "app", "linux-x86_64", "node:20");
        assert_ne!(a.full_hex, b.full_hex);
    }

    #[test]
    fn projection_key_differs_with_platform() {
        let a = ProjectionKey::compute("install/dir", "aabbcc", None, "app", "darwin-arm64", "node:20");
        let b = ProjectionKey::compute("install/dir", "aabbcc", None, "app", "linux-x86_64", "node:20");
        assert_ne!(a.full_hex, b.full_hex);
    }

    #[test]
    fn projection_key_differs_with_toolchain() {
        let a = ProjectionKey::compute("install/dir", "aabbcc", None, "app", "darwin-arm64", "node:20");
        let b = ProjectionKey::compute("install/dir", "aabbcc", None, "app", "darwin-arm64", "node:22");
        assert_ne!(a.full_hex, b.full_hex);
    }

    #[test]
    fn display_key_is_v1_prefixed_short_form() {
        let k = sample_key();
        let d = k.display_key();
        assert!(d.starts_with("v1:"));
        // "v1:" + 12 hex chars
        assert_eq!(d.len(), 3 + 12);
    }

    #[test]
    fn same_install_identity_different_target_no_collision() {
        let identity = "~/.ato/runtimes/publisher/capsule/2.0.0";
        let a = ProjectionKey::compute(identity, "mfst", None, "api", "darwin-arm64", "node:20");
        let b = ProjectionKey::compute(identity, "mfst", None, "worker", "darwin-arm64", "node:20");
        assert_ne!(a.full_key(), b.full_key(), "different targets must not collide");
    }
}
