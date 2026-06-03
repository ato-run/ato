//! GUI-safe Podman binary resolver.
//!
//! When Ato Desktop is launched from Finder/Dock on macOS, its subprocesses
//! inherit a minimal `PATH` that omits `/opt/homebrew/bin`. Spawning the bare
//! string `"podman"` then fails with `NotFound` even though Homebrew Podman is
//! installed — surfacing as a false "missing required binary 'podman'" before an
//! OCI session can start.
//!
//! This module owns the single place that decides *which* podman binary Ato
//! spawns. It is used by both the CLI runtime-setup detection and the OCI
//! provider so the two never drift. Resolution itself never spawns a process;
//! version reading is opt-in via [`ResolvedPodman::query_version`].
//!
//! Resolution order:
//! 1. `ATO_PODMAN_BIN` (explicit override), when it points at a usable binary.
//! 2. `PATH` lookup (`which`).
//! 3. OS-specific known install locations (Homebrew on macOS, Program Files on
//!    Windows, the usual `bin` dirs on Linux).
//! 4. Otherwise: missing, with the searched paths reported for diagnostics.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where a resolved podman binary was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PodmanBinarySource {
    /// `ATO_PODMAN_BIN` override.
    EnvOverride,
    /// Found on the process `PATH`.
    Path,
    /// An OS-specific known install location (Homebrew, Program Files, …).
    KnownLocation,
}

/// A resolved podman binary. `version` is populated lazily by
/// [`ResolvedPodman::query_version`] so plain resolution stays side-effect free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPodman {
    pub bin: PathBuf,
    pub source: PodmanBinarySource,
    pub version: Option<String>,
}

impl ResolvedPodman {
    /// The directory containing the resolved binary, used to prepend a child
    /// process `PATH` so sibling helpers (e.g. `gvproxy`) resolve.
    pub fn bin_dir(&self) -> Option<&Path> {
        self.bin.parent()
    }

    /// Read and cache `<bin> --version` (first line, trimmed). Returns the
    /// cached value on subsequent calls. `None` if the binary cannot be run.
    pub fn query_version(&mut self) -> Option<&str> {
        if self.version.is_none() {
            self.version = read_podman_version(&self.bin);
        }
        self.version.as_deref()
    }
}

/// Podman could not be resolved anywhere. Carries the paths that were searched
/// so the diagnostic is actionable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodmanResolveError {
    pub searched: Vec<PathBuf>,
}

impl std::fmt::Display for PodmanResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "podman binary not found")?;
        if !self.searched.is_empty() {
            let joined = self
                .searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            write!(f, " (searched PATH and: {joined})")?;
        }
        Ok(())
    }
}

impl std::error::Error for PodmanResolveError {}

/// How to invoke podman as a child process: the program to spawn plus an
/// optional `PATH` override that prepends the binary's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodmanInvocation {
    /// Absolute resolved binary, or the literal `"podman"` fallback when
    /// unresolved (never worse than spawning the bare name as before).
    pub program: OsString,
    /// `PATH` value to set on the child, when a directory could be resolved.
    pub path_env: Option<OsString>,
    /// Whether a real binary was resolved (vs. the `"podman"` fallback).
    pub found: bool,
}

/// Resolve the podman binary Ato should spawn (no process spawn).
pub fn resolve_podman() -> Result<ResolvedPodman, PodmanResolveError> {
    let env_override = std::env::var_os("ATO_PODMAN_BIN").map(PathBuf::from);
    let path_lookup = which::which("podman").ok();
    let known = known_podman_locations();
    let (bin, source) = resolve_from(env_override, path_lookup, &known, &is_usable)?;
    Ok(ResolvedPodman {
        bin,
        source,
        version: None,
    })
}

/// Build a [`PodmanInvocation`] for spawning podman commands. Falls back to the
/// bare `"podman"` name (no PATH override) when resolution fails.
pub fn podman_invocation() -> PodmanInvocation {
    match resolve_podman() {
        Ok(resolved) => {
            let path_env = resolved.bin_dir().and_then(prepend_path_env);
            PodmanInvocation {
                program: resolved.bin.into_os_string(),
                path_env,
                found: true,
            }
        }
        Err(_) => PodmanInvocation {
            program: OsString::from("podman"),
            path_env: None,
            found: false,
        },
    }
}

/// OS-specific known podman install locations, newest-preferred first.
pub fn known_podman_locations() -> Vec<PathBuf> {
    known_locations_for(std::env::consts::OS)
}

// ── Internal, testable helpers ───────────────────────────────────────────────

/// Pure resolution core. Split out so resolution is unit-testable without a real
/// podman binary or a real `PATH`.
fn resolve_from(
    env_override: Option<PathBuf>,
    path_lookup: Option<PathBuf>,
    known: &[PathBuf],
    is_usable: &dyn Fn(&Path) -> bool,
) -> Result<(PathBuf, PodmanBinarySource), PodmanResolveError> {
    let mut searched = Vec::new();

    if let Some(candidate) = env_override {
        if is_usable(&candidate) {
            return Ok((candidate, PodmanBinarySource::EnvOverride));
        }
        // A stale override should not hard-fail when PATH/known dirs still work.
        searched.push(candidate);
    }

    if let Some(candidate) = path_lookup {
        // `which` already validated executability; trust it.
        return Ok((candidate, PodmanBinarySource::Path));
    }

    for candidate in known {
        if is_usable(candidate) {
            return Ok((candidate.clone(), PodmanBinarySource::KnownLocation));
        }
        searched.push(candidate.clone());
    }

    Err(PodmanResolveError { searched })
}

/// Per-OS known locations, parameterized on the OS string for testability.
fn known_locations_for(os: &str) -> Vec<PathBuf> {
    match os {
        "macos" => vec![
            PathBuf::from("/opt/homebrew/bin/podman"),
            PathBuf::from("/usr/local/bin/podman"),
        ],
        "windows" => windows_known_locations(),
        "linux" => vec![
            PathBuf::from("/usr/bin/podman"),
            PathBuf::from("/usr/local/bin/podman"),
        ],
        _ => Vec::new(),
    }
}

/// Common Windows Podman install locations under Program Files.
fn windows_known_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(base) = std::env::var_os(var) {
            let base = PathBuf::from(base);
            out.push(base.join("RedHat\\Podman\\podman.exe"));
            out.push(base.join("Podman\\podman.exe"));
        }
    }
    out
}

/// A path is usable as a podman binary when it is an existing executable file.
fn is_usable(path: &Path) -> bool {
    path.is_file() && is_executable(path)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Prepend `dir` to the current `PATH`, returning the joined value. `None` only
/// if the resulting value can't be encoded (paths with `:`/`;` on the platform).
fn prepend_path_env(dir: &Path) -> Option<OsString> {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths).ok()
}

/// Run `<bin> --version` and return the first non-empty line, trimmed.
fn read_podman_version(bin: &Path) -> Option<String> {
    let output = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always_usable(_: &Path) -> bool {
        true
    }

    #[test]
    fn env_override_wins_over_path_and_known() {
        let (bin, source) = resolve_from(
            Some(PathBuf::from("/custom/podman")),
            Some(PathBuf::from("/usr/bin/podman")),
            &[PathBuf::from("/opt/homebrew/bin/podman")],
            &always_usable,
        )
        .expect("override resolves");
        assert_eq!(bin, PathBuf::from("/custom/podman"));
        assert_eq!(source, PodmanBinarySource::EnvOverride);
    }

    #[test]
    fn unusable_override_falls_through_to_path() {
        let (bin, source) = resolve_from(
            Some(PathBuf::from("/stale/podman")),
            Some(PathBuf::from("/usr/bin/podman")),
            &[],
            &|p| p != Path::new("/stale/podman"),
        )
        .expect("falls through to PATH");
        assert_eq!(bin, PathBuf::from("/usr/bin/podman"));
        assert_eq!(source, PodmanBinarySource::Path);
    }

    #[test]
    fn path_lookup_is_trusted_when_no_override() {
        let (bin, source) =
            resolve_from(None, Some(PathBuf::from("/usr/bin/podman")), &[], &|_| {
                false
            })
            .expect("PATH result is trusted without re-checking executability");
        assert_eq!(bin, PathBuf::from("/usr/bin/podman"));
        assert_eq!(source, PodmanBinarySource::Path);
    }

    #[test]
    fn minimal_path_resolves_homebrew_known_location() {
        // Simulates a macOS GUI launch: PATH lookup fails, but the Homebrew
        // location is present.
        let homebrew = PathBuf::from("/opt/homebrew/bin/podman");
        let (bin, source) = resolve_from(
            None,
            None,
            &[homebrew.clone(), PathBuf::from("/usr/local/bin/podman")],
            &|p| p == homebrew,
        )
        .expect("homebrew known location resolves under minimal PATH");
        assert_eq!(bin, homebrew);
        assert_eq!(source, PodmanBinarySource::KnownLocation);
    }

    #[test]
    fn does_not_require_usr_local_symlink() {
        // Only Homebrew exists; the /usr/local/bin symlink is absent. Resolution
        // must still succeed without it.
        let homebrew = PathBuf::from("/opt/homebrew/bin/podman");
        let usr_local = PathBuf::from("/usr/local/bin/podman");
        let (bin, _) = resolve_from(None, None, &[homebrew.clone(), usr_local.clone()], &|p| {
            p == homebrew
        })
        .expect("resolves without /usr/local symlink");
        assert_eq!(bin, homebrew);
    }

    #[test]
    fn missing_reports_searched_paths() {
        let known = vec![
            PathBuf::from("/opt/homebrew/bin/podman"),
            PathBuf::from("/usr/local/bin/podman"),
        ];
        let err = resolve_from(Some(PathBuf::from("/stale/podman")), None, &known, &|_| {
            false
        })
        .expect_err("nothing usable => missing");
        assert!(err.searched.contains(&PathBuf::from("/stale/podman")));
        assert!(
            err.searched
                .contains(&PathBuf::from("/opt/homebrew/bin/podman"))
        );
        let rendered = err.to_string();
        assert!(rendered.contains("/opt/homebrew/bin/podman"), "{rendered}");
    }

    #[test]
    fn macos_known_locations_include_both_homebrew_paths() {
        let locs = known_locations_for("macos");
        assert!(locs.contains(&PathBuf::from("/opt/homebrew/bin/podman")));
        assert!(locs.contains(&PathBuf::from("/usr/local/bin/podman")));
    }

    #[test]
    fn linux_known_locations_cover_usual_bins() {
        let locs = known_locations_for("linux");
        assert!(locs.contains(&PathBuf::from("/usr/bin/podman")));
        assert!(locs.contains(&PathBuf::from("/usr/local/bin/podman")));
    }

    #[test]
    fn unknown_os_has_no_known_locations() {
        assert!(known_locations_for("plan9").is_empty());
    }
}
