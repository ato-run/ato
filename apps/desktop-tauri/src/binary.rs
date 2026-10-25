//! Resolution of the bundled `ato` CLI binary.
//!
//! The Tauri shell is a composition root, not an execution owner: it delegates
//! every Capsule operation to the `ato` binary. Where that binary lives depends
//! on how the shell was launched, so resolution is explicit policy:
//!
//! - development: `ATO_DESKTOP_ATO_BIN`, then the sibling next to the shell
//!   executable, then (debug builds only) `PATH`.
//! - release: the bundled sibling next to the shell executable, then an
//!   explicit failure — never a silent `PATH` fallback that could pick up an
//!   unrelated `ato` install.

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BinaryError {
    #[error("ato binary not found")]
    NotFound,
    #[error("ATO_DESKTOP_ATO_BIN points to a missing file: {0}")]
    MissingOverride(PathBuf),
}

/// The platform `ato` executable name.
pub fn ato_binary_name() -> &'static str {
    if cfg!(windows) { "ato.exe" } else { "ato" }
}

/// Resolve the `ato` binary against the current process environment.
pub fn resolve_ato_binary() -> Result<PathBuf, BinaryError> {
    resolve_ato_binary_with(
        &|name| std::env::var_os(name).map(PathBuf::from),
        &std::env::current_exe().unwrap_or_default(),
        cfg!(debug_assertions),
        &find_on_path,
    )
}

fn resolve_ato_binary_with(
    env: &dyn Fn(&str) -> Option<PathBuf>,
    current_exe: &Path,
    debug: bool,
    path_lookup: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<PathBuf, BinaryError> {
    let name = ato_binary_name();
    if debug && let Some(path) = env("ATO_DESKTOP_ATO_BIN") {
        return if path.is_file() {
            Ok(path)
        } else {
            Err(BinaryError::MissingOverride(path))
        };
    }
    let sibling = current_exe.with_file_name(name);
    if sibling.is_file() {
        return Ok(sibling);
    }
    if debug && let Some(path) = path_lookup(name) {
        return Ok(path);
    }
    Err(BinaryError::NotFound)
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<PathBuf> {
        None
    }

    fn no_path(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn debug_prefers_the_explicit_override() {
        let dir = tempfile::tempdir().unwrap();
        let override_bin = dir.path().join("ato");
        std::fs::write(&override_bin, b"").unwrap();
        let env = |name: &str| (name == "ATO_DESKTOP_ATO_BIN").then(|| override_bin.clone());
        let resolved =
            resolve_ato_binary_with(&env, &dir.path().join("shell"), true, &no_path).unwrap();
        assert_eq!(resolved, override_bin);
    }

    #[test]
    fn debug_override_that_is_missing_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing-ato");
        let env = |name: &str| (name == "ATO_DESKTOP_ATO_BIN").then(|| missing.clone());
        let err =
            resolve_ato_binary_with(&env, &dir.path().join("shell"), true, &no_path).unwrap_err();
        assert_eq!(err, BinaryError::MissingOverride(missing));
    }

    #[test]
    fn debug_falls_back_to_the_sibling_then_path() {
        let dir = tempfile::tempdir().unwrap();
        let sibling = dir.path().join("ato");
        std::fs::write(&sibling, b"").unwrap();
        let resolved =
            resolve_ato_binary_with(&no_env, &dir.path().join("shell"), true, &no_path).unwrap();
        assert_eq!(resolved, sibling);

        // No sibling → PATH lookup.
        let path_bin = dir.path().join("on-path-ato");
        std::fs::write(&path_bin, b"").unwrap();
        let empty_dir = tempfile::tempdir().unwrap();
        let lookup = |name: &str| (name == "ato").then(|| path_bin.clone());
        let resolved =
            resolve_ato_binary_with(&no_env, &empty_dir.path().join("shell"), true, &lookup)
                .unwrap();
        assert_eq!(resolved, path_bin);
    }

    #[test]
    fn release_requires_the_bundled_sibling_and_ignores_path_and_override() {
        let dir = tempfile::tempdir().unwrap();
        let sibling = dir.path().join("ato");
        std::fs::write(&sibling, b"").unwrap();
        let env =
            |name: &str| (name == "ATO_DESKTOP_ATO_BIN").then(|| dir.path().join("elsewhere-ato"));
        let path_bin = dir.path().join("path-ato");
        std::fs::write(&path_bin, b"").unwrap();
        let lookup = |name: &str| (name == "ato").then(|| path_bin.clone());

        // Sibling wins in release; override and PATH are both ignored.
        let resolved =
            resolve_ato_binary_with(&env, &dir.path().join("shell"), false, &lookup).unwrap();
        assert_eq!(resolved, sibling);

        // No sibling → explicit failure even though PATH and override exist.
        let empty_dir = tempfile::tempdir().unwrap();
        let err = resolve_ato_binary_with(&env, &empty_dir.path().join("shell"), false, &lookup)
            .unwrap_err();
        assert_eq!(err, BinaryError::NotFound);
    }
}
