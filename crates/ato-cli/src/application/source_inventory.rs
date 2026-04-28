//! Source-tree inventory helpers shared between `ato build` v0.3 build cache
//! and `ato run` build materialization (RFC: BUILD_MATERIALIZATION).
//!
//! These helpers were extracted from `cli/commands/build.rs` without any
//! behavioral change. The goal is to let the run-side materialization compute
//! the same canonical source-file set that the existing build cache uses,
//! without duplicating walker / output normalization logic.

use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

/// Directory names skipped while walking a project source tree for cache /
/// materialization digest computation. These are uniformly volatile (VCS,
/// language package caches, build artifacts) and never contribute to a
/// reproducible build input set.
pub(crate) const DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".git",
    ".tmp",
    "node_modules",
    ".venv",
    "target",
    "__pycache__",
    // Project-local ato state. `.ato/state/materializations.json` is itself a
    // build artifact of the materialization layer; including it would create
    // a feedback loop where every save invalidates the digest it was based on.
    ".ato",
];

/// A normalized declared output path, anchored at the project working
/// directory. Always relative; never escapes the working directory.
#[derive(Debug, Clone)]
pub(crate) struct OutputSpec {
    pub(crate) relative_path: PathBuf,
}

/// Validate and normalize a list of declared `outputs` strings into
/// [`OutputSpec`] entries.
///
/// Accepted forms:
/// - `"a/b"` → file or directory at `<working_dir>/a/b`
/// - `"a/b/**"` → directory tree at `<working_dir>/a/b` (the `/**` suffix is
///   normalized away)
///
/// Rejected forms (error):
/// - absolute paths
/// - parent traversal (`..`)
/// - prefix components (Windows drive letters etc.)
/// - any other glob metacharacter (`*`, `?`, `[`)
pub(crate) fn normalize_outputs(raw_outputs: &[String]) -> Result<Vec<OutputSpec>> {
    let mut outputs = Vec::new();

    for raw_output in raw_outputs {
        let mut normalized = raw_output.trim();
        if normalized.is_empty() {
            continue;
        }

        if normalized.ends_with("/**") {
            normalized = normalized.trim_end_matches("/**");
        }
        normalized = normalized.trim_start_matches("./");
        normalized = normalized.trim_end_matches('/');

        if normalized.is_empty() {
            anyhow::bail!(
                "outputs entries must resolve to a relative path inside the package root"
            );
        }
        if normalized.contains('*') || normalized.contains('?') || normalized.contains('[') {
            anyhow::bail!(
                "unsupported outputs pattern '{}'; only exact relative paths and '<dir>/**' are supported",
                raw_output
            );
        }

        let path = Path::new(normalized);
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            anyhow::bail!(
                "outputs entry '{}' must stay inside the package root",
                raw_output
            );
        }

        outputs.push(OutputSpec {
            relative_path: path.to_path_buf(),
        });
    }

    Ok(outputs)
}

/// Collect lockfiles known to influence build determinism (npm/pnpm/bun/uv/
/// Cargo/deno/poetry). The returned list is sorted by path so callers can
/// stream them into a digest deterministically.
pub(crate) fn native_lockfiles(working_dir: &Path) -> Vec<PathBuf> {
    let mut paths = [
        "package-lock.json",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
        "uv.lock",
        "Cargo.lock",
        "deno.lock",
        "poetry.lock",
    ]
    .into_iter()
    .map(|name| working_dir.join(name))
    .filter(|path| path.exists())
    .collect::<Vec<_>>();
    paths.sort();
    paths
}

/// Walk `working_dir` and return every file path (relative to `working_dir`)
/// that should be considered a source input. Output trees declared via
/// [`OutputSpec`] and the ato-managed home directory are skipped, as are
/// every entry whose top-level directory name appears in
/// [`DEFAULT_IGNORED_DIRS`].
///
/// Results are sorted to keep digest computation deterministic.
pub(crate) fn collect_source_files(
    working_dir: &Path,
    outputs: &[OutputSpec],
) -> Result<Vec<PathBuf>> {
    let ignored_dynamic_roots = dynamic_ignored_roots(working_dir);
    let mut files = Vec::new();
    let walker = WalkDir::new(working_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let Ok(relative_path) = entry.path().strip_prefix(working_dir) else {
                return true;
            };
            if relative_path.as_os_str().is_empty() {
                return true;
            }
            if path_is_within_any_root(relative_path, &ignored_dynamic_roots) {
                return false;
            }
            if entry.file_type().is_dir() {
                if let Some(name) = relative_path.file_name().and_then(|value| value.to_str()) {
                    if DEFAULT_IGNORED_DIRS.contains(&name) {
                        return false;
                    }
                }
            }
            !path_is_within_outputs(relative_path, outputs)
        });

    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative_path = entry
            .path()
            .strip_prefix(working_dir)
            .with_context(|| format!("Failed to relativize {}", entry.path().display()))?;
        if path_is_within_any_root(relative_path, &ignored_dynamic_roots) {
            continue;
        }
        if path_is_within_outputs(relative_path, outputs) {
            continue;
        }
        files.push(relative_path.to_path_buf());
    }

    files.sort();
    Ok(files)
}

pub(crate) fn path_is_within_outputs(path: &Path, outputs: &[OutputSpec]) -> bool {
    outputs
        .iter()
        .any(|output| path.starts_with(&output.relative_path))
}

pub(crate) fn dynamic_ignored_roots(working_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(ato_home) = capsule_core::common::paths::nacelle_home_dir() {
        if let Ok(relative) = ato_home.strip_prefix(working_dir) {
            if !relative.as_os_str().is_empty() {
                roots.push(relative.to_path_buf());
            }
        }
    }
    roots
}

pub(crate) fn path_is_within_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_trailing_glob_and_dot_slash() {
        let outputs = normalize_outputs(&[
            "./.next/**".to_string(),
            "dist/".to_string(),
            "build/output.bin".to_string(),
        ])
        .expect("normalize");
        assert_eq!(outputs.len(), 3);
        assert_eq!(outputs[0].relative_path, PathBuf::from(".next"));
        assert_eq!(outputs[1].relative_path, PathBuf::from("dist"));
        assert_eq!(outputs[2].relative_path, PathBuf::from("build/output.bin"));
    }

    #[test]
    fn normalize_rejects_parent_traversal() {
        let err = normalize_outputs(&["../escape".to_string()]).unwrap_err();
        assert!(err.to_string().contains("must stay inside"));
    }

    #[test]
    fn normalize_rejects_absolute_path() {
        let err = normalize_outputs(&["/etc/passwd".to_string()]).unwrap_err();
        assert!(err.to_string().contains("must stay inside"));
    }

    #[test]
    fn normalize_rejects_unsupported_glob_metacharacters() {
        let err = normalize_outputs(&["a/*".to_string()]).unwrap_err();
        assert!(err.to_string().contains("unsupported outputs pattern"));
    }

    #[test]
    fn normalize_skips_empty_entries_silently() {
        let outputs = normalize_outputs(&["".to_string(), "  ".to_string()]).expect("normalize");
        assert!(outputs.is_empty());
    }

    #[test]
    fn path_is_within_outputs_matches_prefix() {
        let outputs = vec![OutputSpec {
            relative_path: PathBuf::from(".next"),
        }];
        assert!(path_is_within_outputs(
            Path::new(".next/server/foo.js"),
            &outputs
        ));
        assert!(!path_is_within_outputs(Path::new("src/index.ts"), &outputs));
    }
}
