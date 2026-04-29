//! Single source of truth for "where does this capsule's project live on disk?"
//!
//! A GitHub-installed capsule unpacks as `<artifact>/capsule.toml` plus
//! `<artifact>/source/<files>`, while a flat-layout capsule lives directly
//! under `<artifact>/`. Provision, preflight, build, and shadow workspace
//! materialization all need to agree on the same root, otherwise:
//!
//! - `uv sync --frozen` runs from the wrong directory and uv reports
//!   "No `pyproject.toml` found" (the original Bug 2 symptom).
//! - The shadow workspace puts the manifest at one path while the run-time
//!   provision step targets a different path, so generated lockfiles never
//!   reach the executor.
//!
//! The previous implementation had two parallel resolvers — one in
//! `cli::commands::run::preflight` and one in
//! `adapters::runtime::provisioning::shadow` — that happened to follow the
//! same precedence by convention. They could (and did, pre-fix) drift
//! independently. This module centralises the contract:
//!
//! 1. **Author intent first** — explicit `working_dir` from `[targets.<label>]`
//!    in `capsule.toml`. What the manifest declares is what every phase uses.
//!    A typo (path that doesn't exist) silently falls through; we don't want
//!    a stale string to invent a directory.
//! 2. **`source/` heuristic** — if `<manifest_dir>/source/` looks like a
//!    runnable project (Node / Python / Rust / Go marker file), use it. This
//!    is what GitHub-installed capsules need.
//! 3. **Final fallback** — `plan.execution_working_directory()` (which is
//!    `manifest_dir` when no `working_dir` was declared anywhere). Preserves
//!    existing behaviour for flat-layout capsules.
//!
//! All call sites that previously reached into `plan.manifest_dir` directly
//! (or built `plan.manifest_dir.join("source")` ad-hoc) should consume this
//! module instead.

use std::path::{Path, PathBuf};

use capsule_core::router::ManifestData;

/// Absolute path to the capsule's dependency / project root.
///
/// This is the directory that owns `pyproject.toml` / `package.json` /
/// `Cargo.toml` / `go.mod`, and the directory `uv sync` / `npm install` /
/// `cargo fetch` should run from.
pub(crate) fn dependency_root(plan: &ManifestData) -> PathBuf {
    if let Some(raw) = plan.execution_working_dir() {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && trimmed != "." {
            let candidate = plan.manifest_dir.join(trimmed);
            if candidate.is_dir() {
                return candidate;
            }
        }
    }

    let source_dir = plan.manifest_dir.join("source");
    if looks_like_source_project(&source_dir) {
        return source_dir;
    }

    plan.execution_working_directory()
}

/// Same precedence as [`dependency_root`], but returned relative to
/// `plan.manifest_dir`. The shadow workspace materializer stores manifests
/// under a mirrored relative path, so it consumes this form.
///
/// Empty `PathBuf` means "the manifest root itself".
pub(crate) fn relative_dependency_root_from_manifest(plan: &ManifestData) -> PathBuf {
    if let Some(raw) = plan.execution_working_dir() {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && trimmed != "." {
            return PathBuf::from(trimmed);
        }
    }

    let source_dir = plan.manifest_dir.join("source");
    if looks_like_source_project(&source_dir) {
        return PathBuf::from("source");
    }

    pathdiff::diff_paths(plan.execution_working_directory(), &plan.manifest_dir).unwrap_or_default()
}

/// True when `dir` contains a marker file that identifies a runnable Node /
/// Python / Rust / Go project. Used to decide whether `<manifest_dir>/source/`
/// holds the actual project (GitHub-installed layout).
///
/// The list is intentionally conservative: every entry is a file that some
/// real importer reads at provision time. Adding a marker here implicitly
/// promises that the corresponding `dependency_root(plan)` will work for
/// that ecosystem.
pub(crate) fn looks_like_source_project(dir: &Path) -> bool {
    dir.join("package.json").exists()
        || dir.join("pyproject.toml").exists()
        || dir.join("uv.lock").exists()
        || dir.join("requirements.txt").exists()
        || dir.join("Pipfile").exists()
        || dir.join("Cargo.toml").exists()
        || dir.join("go.mod").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::router::{
        execution_descriptor_from_manifest_parts, ExecutionProfile, ManifestData,
    };
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    fn build_plan_with_target(manifest_dir: &Path, manifest: &str, target: &str) -> ManifestData {
        execution_descriptor_from_manifest_parts(
            toml::from_str::<toml::Value>(manifest).expect("parse manifest"),
            manifest_dir.join("capsule.toml"),
            manifest_dir.to_path_buf(),
            ExecutionProfile::Dev,
            Some(target),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir for touch");
        }
        fs::write(path, "").expect("touch test fixture");
    }

    #[test]
    fn looks_like_source_project_recognises_each_ecosystem_marker() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("source");
        fs::create_dir_all(&source).expect("create source");
        assert!(!looks_like_source_project(&source));

        for marker in [
            "package.json",
            "pyproject.toml",
            "uv.lock",
            "requirements.txt",
            "Pipfile",
            "Cargo.toml",
            "go.mod",
        ] {
            fs::remove_dir_all(&source).expect("rm source");
            fs::create_dir_all(&source).expect("create source");
            touch(&source.join(marker));
            assert!(
                looks_like_source_project(&source),
                "expected `{}` to register as a source project",
                marker
            );
        }
    }

    #[test]
    fn dependency_root_honors_explicit_working_dir() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("source")).expect("create source");
        // Explicit `working_dir` wins even if `source/` looks empty enough
        // not to trigger the heuristic. Author intent is the contract.
        let manifest = r#"
schema_version = "0.3"
name = "explicit-wd"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run = "start.py"
working_dir = "source"
"#;
        let plan = build_plan_with_target(dir.path(), manifest, "app");
        assert_eq!(dependency_root(&plan), dir.path().join("source"));
        assert_eq!(
            relative_dependency_root_from_manifest(&plan),
            PathBuf::from("source")
        );
    }

    #[test]
    fn dependency_root_falls_back_to_source_layout_for_python() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("source");
        fs::create_dir_all(&source).expect("create source");
        touch(&source.join("pyproject.toml"));
        touch(&source.join("uv.lock"));
        let manifest = r#"
schema_version = "0.3"
name = "py-source-only"
version = "0.1.0"
type = "app"
runtime = "source/python"
runtime_version = "3.11"
run = "start.py"
"#;
        let plan = execution_descriptor_from_manifest_parts(
            toml::from_str::<toml::Value>(manifest).expect("parse manifest"),
            dir.path().join("capsule.toml"),
            dir.path().to_path_buf(),
            ExecutionProfile::Dev,
            None,
            HashMap::new(),
        )
        .expect("execution descriptor");
        assert_eq!(dependency_root(&plan), source);
        assert_eq!(
            relative_dependency_root_from_manifest(&plan),
            PathBuf::from("source")
        );
    }

    #[test]
    fn dependency_root_falls_back_to_execution_directory_for_flat_layout() {
        let dir = tempdir().expect("tempdir");
        // No source/, no marker files anywhere — the resolver must not
        // invent a directory and must yield exactly what the legacy
        // `plan.execution_working_directory()` would have returned.
        let manifest = r#"
schema_version = "0.3"
name = "flat-layout"
version = "0.1.0"
type = "app"
runtime = "source/python"
runtime_version = "3.11"
run = "start.py"
"#;
        let plan = execution_descriptor_from_manifest_parts(
            toml::from_str::<toml::Value>(manifest).expect("parse manifest"),
            dir.path().join("capsule.toml"),
            dir.path().to_path_buf(),
            ExecutionProfile::Dev,
            None,
            HashMap::new(),
        )
        .expect("execution descriptor");
        assert_eq!(dependency_root(&plan), plan.execution_working_directory());
    }

    #[test]
    fn dependency_root_does_not_invent_missing_explicit_working_dir() {
        // If the manifest declares working_dir = "frontend" but the directory
        // doesn't exist on disk, the resolver must fall through to the
        // `source/` heuristic / final fallback rather than return a path that
        // would explode at provision time.
        let dir = tempdir().expect("tempdir");
        let manifest = r#"
schema_version = "0.3"
name = "typo-wd"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
runtime_version = "20"
run = "start.js"
working_dir = "frontend"
"#;
        let plan = build_plan_with_target(dir.path(), manifest, "app");
        // No frontend/ on disk, no source/ either → fall through to
        // execution_working_directory(), which is the manifest_dir.
        assert_eq!(dependency_root(&plan), plan.execution_working_directory());
    }
}
