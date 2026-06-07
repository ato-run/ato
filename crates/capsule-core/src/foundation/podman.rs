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

/// Name of the Podman machine Ato creates and manages on macOS/Windows. Ato
/// only ever inits/starts a machine with this name; it never mutates a user's
/// own machines or changes the global default connection.
pub const ATO_PODMAN_MACHINE_NAME: &str = "ato-podman";

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

    /// Build a [`PodmanInvocation`] from this already-resolved binary, so a
    /// caller that probed the version and then spawns a command targets the
    /// exact same binary instead of re-resolving.
    pub fn invocation(&self) -> PodmanInvocation {
        let path_env = self.bin_dir().and_then(prepend_path_env);
        PodmanInvocation {
            program: self.bin.clone().into_os_string(),
            path_env,
            found: true,
        }
    }
}

/// Why podman resolution failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PodmanResolveError {
    /// `ATO_PODMAN_BIN` was set but does not point at a usable executable. This
    /// is an explicit user runtime selection, so it fails hard rather than
    /// silently falling back to a *different* podman on `PATH` — which would
    /// make the override look effective while a different binary is used.
    InvalidEnvOverride { path: PathBuf },
    /// Podman could not be found anywhere. Carries the paths searched (besides
    /// `PATH`) so the diagnostic is actionable.
    NotFound { searched: Vec<PathBuf> },
}

impl std::fmt::Display for PodmanResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEnvOverride { path } => write!(
                f,
                "ATO_PODMAN_BIN points at '{}', which is not a usable executable",
                path.display()
            ),
            Self::NotFound { searched } => {
                write!(f, "podman binary not found")?;
                if !searched.is_empty() {
                    let joined = searched
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(f, " (searched PATH and: {joined})")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for PodmanResolveError {}

/// How to invoke podman as a child process: the program to spawn plus an
/// optional `PATH` override that prepends the binary's directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodmanInvocation {
    /// The program to spawn. A resolved absolute binary; on an *invalid*
    /// `ATO_PODMAN_BIN` override the override path itself (so the spawn fails
    /// loudly on that exact path rather than silently using a different
    /// podman); only the bare `"podman"` when podman is genuinely absent.
    pub program: OsString,
    /// `PATH` value to set on the child, when a directory could be resolved.
    pub path_env: Option<OsString>,
    /// Whether a real binary was resolved (vs. a fallback program).
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

/// Build a [`PodmanInvocation`] for spawning podman commands.
///
/// On an invalid `ATO_PODMAN_BIN` override the override path is used verbatim so
/// the spawn fails loudly on that exact path — it never silently substitutes a
/// different podman from `PATH`. Only a genuine "not found" falls back to the
/// bare `"podman"` name (no worse than spawning the bare name as before).
pub fn podman_invocation() -> PodmanInvocation {
    invocation_from(resolve_podman())
}

/// Pure mapping from a resolution result to a [`PodmanInvocation`], split out so
/// the fallback policy is unit-testable without touching process env / `PATH`.
fn invocation_from(resolved: Result<ResolvedPodman, PodmanResolveError>) -> PodmanInvocation {
    match resolved {
        Ok(resolved) => resolved.invocation(),
        // Invalid override: spawn the exact override path so it fails visibly
        // rather than picking up a different podman on PATH.
        Err(PodmanResolveError::InvalidEnvOverride { path }) => PodmanInvocation {
            program: path.into_os_string(),
            path_env: None,
            found: false,
        },
        // Genuinely missing: fall back to the bare name.
        Err(PodmanResolveError::NotFound { .. }) => PodmanInvocation {
            program: OsString::from("podman"),
            path_env: None,
            found: false,
        },
    }
}

/// OS-specific known podman install locations, newest-preferred first.
///
/// Ato-managed installs (`~/.ato/tools/podman-<version>/…`, created without
/// Homebrew) are searched *before* the system locations so a freshly
/// Ato-installed Podman is found on a clean machine that has no brew/system
/// copy. The system Homebrew / Program Files / `bin` locations follow.
pub fn known_podman_locations() -> Vec<PathBuf> {
    let mut locations = ato_managed_podman_locations();
    locations.extend(known_locations_for(std::env::consts::OS));
    locations
}

/// File name of the podman binary on the current OS.
fn podman_binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "podman.exe"
    } else {
        "podman"
    }
}

/// Candidate binary paths for any Ato-managed Podman install under
/// `~/.ato/tools`. Each `podman-<version>` directory is probed both for a
/// top-level binary and a `bin/` subdir (release archives vary). Newest
/// versions are preferred (descending sort). Returns empty when the tools dir
/// does not exist or the ato home cannot be determined.
fn ato_managed_podman_locations() -> Vec<PathBuf> {
    let Ok(tools_dir) = crate::common::paths::ato_tools_dir() else {
        return Vec::new();
    };
    let bin = podman_binary_name();
    let mut versions: Vec<PathBuf> = match std::fs::read_dir(&tools_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("podman-"))
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    // Lexical descending order approximates newest-first for `podman-<version>`.
    versions.sort();
    versions.reverse();

    let mut out = Vec::new();
    for dir in versions {
        // Probe the layouts Ato-managed installs may use. The macOS release
        // zips place the binary at `usr/bin/podman` under the version dir;
        // `bin/podman` and a top-level `podman` cover other archive shapes.
        out.push(dir.join("usr").join("bin").join(bin));
        out.push(dir.join("bin").join(bin));
        out.push(dir.join(bin));
    }
    out
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
    // An explicit override is an exact runtime selection: if it is set but
    // unusable, fail hard. Do NOT fall back to PATH/known dirs — that would run
    // a *different* podman while the override appears to be in effect, which is
    // undebuggable once PR B carries machine/socket context on the binary.
    if let Some(candidate) = env_override {
        if is_usable(&candidate) {
            return Ok((candidate, PodmanBinarySource::EnvOverride));
        }
        return Err(PodmanResolveError::InvalidEnvOverride { path: candidate });
    }

    if let Some(candidate) = path_lookup {
        // `which` already validated executability; trust it.
        return Ok((candidate, PodmanBinarySource::Path));
    }

    let mut searched = Vec::new();
    for candidate in known {
        if is_usable(candidate) {
            return Ok((candidate.clone(), PodmanBinarySource::KnownLocation));
        }
        searched.push(candidate.clone());
    }

    Err(PodmanResolveError::NotFound { searched })
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
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn always_usable(_: &Path) -> bool {
        true
    }

    /// Serialize tests that mutate `ATO_HOME` (process-global env).
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock")
    }

    /// RAII guard that sets an env var to a path and restores it on drop.
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
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
    fn unusable_override_is_error() {
        // An explicit but broken ATO_PODMAN_BIN must fail hard, NOT silently
        // run a different podman that happens to be on PATH.
        let err = resolve_from(
            Some(PathBuf::from("/stale/podman")),
            Some(PathBuf::from("/usr/bin/podman")),
            &[PathBuf::from("/opt/homebrew/bin/podman")],
            &|p| p != Path::new("/stale/podman"),
        )
        .expect_err("invalid override must not fall through");
        assert_eq!(
            err,
            PodmanResolveError::InvalidEnvOverride {
                path: PathBuf::from("/stale/podman"),
            }
        );
        assert!(err.to_string().contains("/stale/podman"), "{err}");
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
        // No override, no PATH, nothing usable => NotFound listing known dirs.
        let err =
            resolve_from(None, None, &known, &|_| false).expect_err("nothing usable => missing");
        let PodmanResolveError::NotFound { searched } = &err else {
            panic!("expected NotFound, got {err:?}");
        };
        assert!(searched.contains(&PathBuf::from("/opt/homebrew/bin/podman")));
        let rendered = err.to_string();
        assert!(rendered.contains("/opt/homebrew/bin/podman"), "{rendered}");
    }

    #[test]
    fn invocation_uses_override_path_on_invalid_override() {
        // An invalid override must spawn the override path itself (so it fails
        // visibly), never the bare "podman" that could pick up a PATH binary.
        let inv = invocation_from(Err(PodmanResolveError::InvalidEnvOverride {
            path: PathBuf::from("/stale/podman"),
        }));
        assert_eq!(inv.program, OsString::from("/stale/podman"));
        assert!(!inv.found);
        assert!(inv.path_env.is_none());
    }

    #[test]
    fn invocation_falls_back_to_bare_name_only_when_not_found() {
        let inv = invocation_from(Err(PodmanResolveError::NotFound {
            searched: Vec::new(),
        }));
        assert_eq!(inv.program, OsString::from("podman"));
        assert!(!inv.found);
    }

    #[test]
    fn invocation_from_resolved_targets_that_binary() {
        let resolved = ResolvedPodman {
            bin: PathBuf::from("/opt/homebrew/bin/podman"),
            source: PodmanBinarySource::KnownLocation,
            version: None,
        };
        let inv = invocation_from(Ok(resolved));
        assert_eq!(inv.program, OsString::from("/opt/homebrew/bin/podman"));
        assert!(inv.found);
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

    #[test]
    fn ato_managed_locations_probe_known_install_layouts() {
        // An Ato-managed install places the binary under a `podman-<version>/`
        // dir; resolution must probe the `usr/bin`, `bin`, and top-level
        // layouts so a freshly Ato-installed Podman is found on a brew-less
        // host. Drive the pure helper against a temp tools dir via ATO_HOME.
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set_path("ATO_HOME", temp.path());

        let install = temp.path().join("tools").join("podman-5.2.3");
        std::fs::create_dir_all(install.join("usr").join("bin")).expect("mkdir");

        let locs = super::ato_managed_podman_locations();
        let bin = super::podman_binary_name();
        assert!(
            locs.contains(&install.join("usr").join("bin").join(bin)),
            "must probe usr/bin layout: {locs:?}"
        );
        assert!(locs.contains(&install.join("bin").join(bin)));
        assert!(locs.contains(&install.join(bin)));
    }

    #[test]
    fn ato_managed_locations_prefer_newest_version() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = EnvVarGuard::set_path("ATO_HOME", temp.path());
        let tools = temp.path().join("tools");
        std::fs::create_dir_all(tools.join("podman-5.1.0")).expect("mkdir");
        std::fs::create_dir_all(tools.join("podman-5.2.3")).expect("mkdir");

        let locs = super::ato_managed_podman_locations();
        // The first probed path must be under the newest version dir.
        let first = locs.first().expect("at least one location");
        assert!(
            first.to_string_lossy().contains("podman-5.2.3"),
            "newest version should be probed first: {first:?}"
        );
    }
}
