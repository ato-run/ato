//! Ato-managed Podman installation — the path that lets a clean macOS VM with
//! **no Homebrew** still obtain a working Podman.
//!
//! Ato's promise is to prepare missing tools itself. Requiring a user to first
//! install Homebrew (`brew.sh`) before Ato can install Podman breaks that
//! promise on a fresh machine. This module demotes Homebrew to *one optional
//! strategy* and adds an Ato-owned installer that downloads a pinned Podman
//! release, **verifies a pinned SHA256 digest before using it**, extracts it
//! into the Ato tools cache (`~/.ato/tools/podman-<version>/…`), and records the
//! artifact provenance (version + digest + source URL) so the install is
//! auditable.
//!
//! ## Strategy order
//!
//! [`install_strategies`] returns the ordered strategies that
//! [`runtime_prepare`](crate::application::runtime_prepare) tries until one
//! succeeds:
//!   1. [`PodmanInstallStrategy::Homebrew`] — only when `brew` is already
//!      present. Never installs Homebrew itself.
//!   2. [`PodmanInstallStrategy::AtoManaged`] — download + digest-verify +
//!      extract a pinned release into `~/.ato/tools`.
//!   3. [`PodmanInstallStrategy::ManualInstructions`] — typed, actionable
//!      last-resort error. **Never** a "install Homebrew and re-run"
//!      instruction.
//!
//! ## Security invariants
//!
//! - The downloaded artifact's SHA256 **must** match the pinned digest before a
//!   single byte is extracted or executed. A mismatch is a hard error
//!   ([`PodmanInstallError::DigestMismatch`]); Ato never runs an unverified
//!   binary.
//! - The network fetch is behind the [`PodmanArtifactFetcher`] trait so it is
//!   unit-testable with a fake; tests never hit the network.
//!
//! ## macOS installer pkg approach
//!
//! On macOS, Ato downloads the official `.pkg` installer from the
//! `podman-container-tools/podman` GitHub release. This pkg bundles:
//! - `podman` CLI
//! - `gvproxy` (network helper for `podman machine`)
//! - `vfkit` (Apple Hypervisor VM provider for `podman machine`)
//! - Default machine configuration
//!
//! The pkg is extracted with `pkgutil --expand-full` (always available on
//! macOS — part of the OS, not Xcode CLT). Ato then searches the expanded
//! tree for the named binaries, validates their native arch, and assembles
//! `~/.ato/tools/podman-<version>/bin/` with all required executables.
//!
//! This replaces the former remote-zip + hand-assembled helper approach
//! (used through PR #579), which was fragile because the remote CLI zip is not
//! a complete machine runtime — helpers had to be pinned and downloaded
//! separately from different repos, and each step was a new failure mode in
//! clean-VM testing.
//!
//! ## Stale install migration
//!
//! If an Ato-managed install from the old remote-zip approach is detected
//! (`source_url` contains `"podman-remote-release-darwin"`), the install dir
//! is cleared and replaced with the pkg-derived bundle. User/system/default
//! Podman machines are never mutated.
//!
//! ## Updating the pinned release
//!
//! To bump the Podman version, update [`PINNED_PODMAN_VERSION`] and the
//! matching per-OS/arch entries in [`pinned_artifact`] (URL + `sha256`).
//! Obtain the SHA256 by downloading the pkg and running:
//!   ```sh
//!   shasum -a 256 podman-installer-macos-arm64.pkg
//!   ```
//! The digest is the security anchor — never leave it blank or guess it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Pinned Podman version Ato installs when no Homebrew/system copy exists.
///
/// Bump this together with the per-OS/arch entries in [`pinned_artifact`].
pub(crate) const PINNED_PODMAN_VERSION: &str = "5.8.2";

/// File name of the provenance manifest written next to an Ato-managed install.
const PROVENANCE_FILE: &str = "ato-podman-provenance.json";

/// A pinned, digest-verified Podman release artifact for one OS/arch.
///
/// On macOS the artifact is now a `.pkg` installer
/// ([`ArtifactFormat::MacosPkg`]) that bundles `podman`, `gvproxy`, and
/// `vfkit` in one download. The `helpers`, `binary_rel_path`, and
/// `strip_prefix` fields are unused for that format — binaries are located by
/// name within the expanded pkg tree. `pkg_helper_names` lists the additional
/// binary names to find and stage alongside `podman`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PinnedArtifact {
    /// Podman version (matches [`PINNED_PODMAN_VERSION`]).
    pub version: &'static str,
    /// Direct download URL of the release artifact.
    pub url: &'static str,
    /// Lowercase hex SHA256 of the artifact at `url`. The security anchor.
    pub sha256: &'static str,
    /// Archive format, so the extractor knows how to unpack.
    pub format: ArtifactFormat,
    /// Path of the `podman` binary inside the extracted archive, relative to
    /// the install dir after `strip_prefix` is applied.
    /// Unused for [`ArtifactFormat::MacosPkg`] (binary found by name search).
    pub binary_rel_path: &'static str,
    /// Leading path component to strip from every archive entry.
    /// Unused for [`ArtifactFormat::MacosPkg`].
    pub strip_prefix: &'static str,
    /// Helper binaries to download separately.
    /// Unused for [`ArtifactFormat::MacosPkg`] (helpers bundled in pkg).
    pub helpers: &'static [HelperArtifact],
    /// Directory (relative to the install dir) where helper binaries are placed
    /// and that `containers.conf` points Podman at via `helper_binaries_dir`.
    pub helper_binaries_rel_dir: &'static str,
    /// For [`ArtifactFormat::MacosPkg`]: names of helper binaries (`gvproxy`,
    /// `vfkit`) to find within the expanded pkg tree and stage into
    /// `helper_binaries_rel_dir`. Empty for archive-based formats.
    pub pkg_helper_names: &'static [&'static str],
}

/// A pinned, digest-verified `podman machine` helper binary. Used by the
/// legacy archive-based format; unused for [`ArtifactFormat::MacosPkg`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HelperArtifact {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

/// Supported release-artifact formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactFormat {
    #[cfg_attr(not(test), allow(dead_code))]
    TarGz,
    #[cfg_attr(not(test), allow(dead_code))]
    Zip,
    /// macOS `.pkg` installer. Expanded with `pkgutil --expand-full` (an OS
    /// built-in, no Xcode CLT required). After expansion the named binaries
    /// (`podman` + `pkg_helper_names`) are located by recursive search, arch-
    /// validated, and staged into the Ato install dir.
    MacosPkg,
}

/// Podman machine provider Ato pins on macOS. `applehv` (Apple Hypervisor, via
/// `vfkit`) needs no extra packages beyond `gvproxy` + `vfkit`.
const MACOS_MACHINE_PROVIDER: &str = "applehv";

/// Whether the Ato machine enables Podman's Rosetta guest share. `false`:
/// Rosetta must never be a hidden prerequisite of an Ato-managed runtime.
const MACOS_MACHINE_ROSETTA: bool = false;

/// File name of the Ato-generated Podman config written into an install dir.
const CONTAINERS_CONF_FILE: &str = "containers.conf";

// ── Pinned macOS installer pkg SHA256s ──────────────────────────────────────
//
// Obtain by downloading the pkg and running:
//   curl -L -o arm64.pkg <arm64-url>  &&  shasum -a 256 arm64.pkg
//   curl -L -o amd64.pkg <amd64-url>  &&  shasum -a 256 amd64.pkg
//
// Source release:
//   https://github.com/podman-container-tools/podman/releases/tag/v5.8.2
//
// These are the security anchors — FILL THEM IN before merging.

const MACOS_ARM64_PKG_SHA256: &str =
    "8aeaa329cd86c502156d9ca6608776e9b72d0f6cc082255c31c8a936f64bbc8c";
const MACOS_AMD64_PKG_SHA256: &str =
    "2312f91523aeb168709f35d41576ade763c891c3991befe7173aac0edf133af9";

/// Resolve the pinned artifact for an `(os, arch)` pair, or `None` when Ato
/// has no pinned managed install for that target.
///
/// `os` / `arch` use Rust's `std::env::consts` spelling (`"macos"`,
/// `"aarch64"`, `"x86_64"`, `"windows"`).
pub(crate) fn pinned_artifact(os: &str, arch: &str) -> Option<PinnedArtifact> {
    match (os, arch) {
        ("macos", "aarch64") => Some(PinnedArtifact {
            version: PINNED_PODMAN_VERSION,
            url: "https://github.com/podman-container-tools/podman/releases/download/v5.8.2/podman-installer-macos-arm64.pkg",
            sha256: MACOS_ARM64_PKG_SHA256,
            format: ArtifactFormat::MacosPkg,
            binary_rel_path: "",
            strip_prefix: "",
            helpers: &[],
            helper_binaries_rel_dir: "bin",
            pkg_helper_names: &["gvproxy", "vfkit"],
        }),
        ("macos", "x86_64") => Some(PinnedArtifact {
            version: PINNED_PODMAN_VERSION,
            url: "https://github.com/podman-container-tools/podman/releases/download/v5.8.2/podman-installer-macos-amd64.pkg",
            sha256: MACOS_AMD64_PKG_SHA256,
            format: ArtifactFormat::MacosPkg,
            binary_rel_path: "",
            strip_prefix: "",
            helpers: &[],
            helper_binaries_rel_dir: "bin",
            pkg_helper_names: &["gvproxy", "vfkit"],
        }),
        // Windows/Linux: existing instruction-based path. Ato-managed installs
        // for those targets are a documented follow-up.
        _ => None,
    }
}

/// The available Podman install strategies, in the order Ato should try them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PodmanInstallStrategy {
    Homebrew,
    AtoManaged,
    ManualInstructions,
}

/// Ordered strategies for the given host.
pub(crate) fn install_strategies(
    brew_present: bool,
    managed_available: bool,
) -> Vec<PodmanInstallStrategy> {
    let mut strategies = Vec::new();
    if brew_present {
        strategies.push(PodmanInstallStrategy::Homebrew);
    }
    if managed_available {
        strategies.push(PodmanInstallStrategy::AtoManaged);
    }
    strategies.push(PodmanInstallStrategy::ManualInstructions);
    strategies
}

/// Typed failures from the Ato-managed installer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PodmanInstallError {
    NoPinnedArtifact { os: String, arch: String },
    Fetch { url: String, message: String },
    DigestMismatch {
        url: String,
        expected: String,
        actual: String,
    },
    Extract { message: String },
    BinaryMissing { expected: PathBuf },
    /// A required binary (`podman`, `gvproxy`, `vfkit`) was not found anywhere
    /// in the expanded pkg tree. Ato packaging issue, not a user issue.
    PkgBinaryNotFound { name: String },
    HelperMissing { helper: String },
    NotNativeArch {
        binary: String,
        host_arch: String,
    },
    Provenance { message: String },
}

impl std::fmt::Display for PodmanInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPinnedArtifact { os, arch } => write!(
                f,
                "Ato has no managed Podman build for {os}/{arch}. Install Podman from \
                 https://podman.io/docs/installation and re-run."
            ),
            Self::Fetch { url, message } => {
                write!(f, "failed to download Podman from {url}: {message}")
            }
            Self::DigestMismatch { url, expected, actual } => write!(
                f,
                "downloaded Podman from {url} but its SHA256 did not match the pinned \
                 digest (expected {expected}, got {actual}); refusing to use an \
                 unverified binary"
            ),
            Self::Extract { message } => write!(f, "failed to extract Podman archive: {message}"),
            Self::BinaryMissing { expected } => write!(
                f,
                "Podman archive did not contain the expected binary at '{}'",
                expected.display()
            ),
            Self::PkgBinaryNotFound { name } => write!(
                f,
                "Ato expanded the Podman installer pkg but could not find a binary named \
                 `{name}` anywhere within it. This is an Ato packaging issue, not a user issue."
            ),
            Self::HelperMissing { helper } => write!(
                f,
                "Ato-managed Podman is incomplete: required helper binary `{helper}` was not \
                 found. This is an Ato packaging/runtime setup issue, not a user issue."
            ),
            Self::NotNativeArch { binary, host_arch } => write!(
                f,
                "Ato-managed Podman binary `{binary}` has no native {host_arch} build (it would \
                 require Rosetta on Apple Silicon); refusing to install a non-native runtime. \
                 This is an Ato packaging/runtime setup issue, not a user issue."
            ),
            Self::Provenance { message } => {
                write!(f, "failed to record Podman install provenance: {message}")
            }
        }
    }
}

impl std::error::Error for PodmanInstallError {}

/// Network seam for fetching a release artifact.
pub(crate) trait PodmanArtifactFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Production fetcher backed by the blocking `reqwest` client.
pub(crate) struct ReqwestArtifactFetcher;

impl PodmanArtifactFetcher for ReqwestArtifactFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("ato-cli/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            // macOS .pkg installers are larger (~200 MB) than the former remote
            // CLI zip (~25 MB); 600 s gives headroom on slow connections.
            .timeout(Duration::from_secs(600))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(url).send().map_err(|e| e.to_string())?;
        let resp = resp.error_for_status().map_err(|e| e.to_string())?;
        let bytes = resp.bytes().map_err(|e| e.to_string())?;
        Ok(bytes.to_vec())
    }
}

/// Provenance of an Ato-managed Podman install.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PodmanProvenance {
    pub version: String,
    pub sha256: String,
    pub source_url: String,
    pub binary_path: String,
    #[serde(default)]
    pub helper_binaries_dir: String,
    #[serde(default)]
    pub helpers: Vec<HelperProvenance>,
}

/// Provenance of one bundled helper binary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HelperProvenance {
    pub name: String,
    pub sha256: String,
    pub source_url: String,
    pub path: String,
    #[serde(default)]
    pub required_arch: String,
}

/// Result of a successful Ato-managed install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledPodman {
    pub binary_path: PathBuf,
    pub provenance: PodmanProvenance,
}

/// Download, digest-verify, extract, and record an Ato-managed Podman install
/// for `(os, arch)` under `tools_dir` (normally `~/.ato/tools`).
///
/// Fails closed on a digest mismatch. If a stale remote-zip install exists it
/// is replaced before the new pkg-derived bundle is written.
pub(crate) fn install_ato_managed_podman<F: PodmanArtifactFetcher>(
    fetcher: &F,
    os: &str,
    arch: &str,
    tools_dir: &Path,
) -> Result<InstalledPodman, PodmanInstallError> {
    let artifact =
        pinned_artifact(os, arch).ok_or_else(|| PodmanInstallError::NoPinnedArtifact {
            os: os.to_string(),
            arch: arch.to_string(),
        })?;

    // Clear any stale installs from the old remote-zip approach. The old
    // version (5.2.3) and new version (5.8.2) are different directories, so
    // we scan all podman-* entries in tools_dir for the stale URL pattern.
    clear_all_stale_remote_zip_installs(tools_dir);
    let install_dir = tools_dir.join(format!("podman-{}", artifact.version));

    let bytes = fetch_and_verify(fetcher, artifact.url, artifact.sha256)?;

    match artifact.format {
        ArtifactFormat::MacosPkg => {
            install_from_pkg(&bytes, &artifact, arch, tools_dir, &install_dir)
        }
        ArtifactFormat::TarGz | ArtifactFormat::Zip => {
            // Archive path: fetch + verify each helper separately, then promote.
            let mut helper_blobs: Vec<(HelperArtifact, Vec<u8>)> =
                Vec::with_capacity(artifact.helpers.len());
            for helper in artifact.helpers {
                let helper_bytes = fetch_and_verify(fetcher, helper.url, helper.sha256)?;
                validate_native_arch(&helper_bytes, arch, helper.name)?;
                helper_blobs.push((*helper, helper_bytes));
            }
            install_into_temp_then_promote(
                &bytes,
                &artifact,
                &helper_blobs,
                arch,
                tools_dir,
                &install_dir,
            )
        }
    }
}

/// Scan `tools_dir` for every `podman-*` directory and remove any that were
/// built by the legacy remote-zip approach (provenance `source_url` contains
/// `"podman-remote-release-darwin"`). Silent on any filesystem error — worst
/// case the atomic rename at the end of the install replaces an old dir.
///
/// This must check all `podman-*` dirs, not just the new version's dir, because
/// the old version (5.2.3) and new version (5.8.2) live in different directories.
fn clear_all_stale_remote_zip_installs(tools_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(tools_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("podman-") {
            continue;
        }
        if is_stale_remote_zip_install(&path) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Returns `true` when `install_dir` was built by the legacy remote-zip
/// approach. Used by the preflight to detect installs that need the pkg repair.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn is_stale_remote_zip_install(install_dir: &Path) -> bool {
    read_provenance(install_dir)
        .map(|p| p.source_url.contains("podman-remote-release-darwin"))
        .unwrap_or(false)
}

// ── macOS pkg extraction ──────────────────────────────────────────────────────

/// Build a process-unique temp install dir path under `tools_dir`. The name
/// embeds the pid plus a monotonic counter so repeated attempts within one
/// process (e.g. retry, or sequential calls in tests) never collide on the same
/// path — the previous `podman-<ver>.tmp-<pid>` scheme reused one path per pid.
fn unique_tmp_install_dir(tools_dir: &Path, version: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    tools_dir.join(format!(
        "podman-{}.tmp-{}-{}",
        version,
        std::process::id(),
        n
    ))
}

/// Move a validated temp install dir into its final location, preserving the
/// previous install until the swap succeeds.
///
/// This is **not** a single atomic syscall — there is no portable atomic
/// directory replace. Instead it minimises the data-loss window: any existing
/// install is renamed to a sibling backup, the new dir is renamed into place,
/// and only then is the backup removed. If the promotion rename fails the backup
/// is restored. So a failure leaves either the new or the previous install in
/// `final_dir` — never an empty/half-removed directory (the hazard of the old
/// `remove_dir_all(final_dir)` → `rename` sequence). On all error paths the temp
/// dir is cleaned up.
fn promote_install_dir(tmp_dir: &Path, final_dir: &Path) -> Result<(), PodmanInstallError> {
    let name = final_dir.file_name().ok_or_else(|| PodmanInstallError::Extract {
        message: format!("install dir has no file name: {}", final_dir.display()),
    })?;
    let mut backup_name = name.to_os_string();
    backup_name.push(format!(".bak-{}", std::process::id()));
    let backup = final_dir.with_file_name(backup_name);

    // Stash any existing install out of the way (don't delete it yet).
    let had_existing = final_dir.exists();
    if had_existing {
        if backup.exists() {
            let _ = std::fs::remove_dir_all(&backup);
        }
        if let Err(e) = std::fs::rename(final_dir, &backup) {
            let _ = std::fs::remove_dir_all(tmp_dir);
            return Err(PodmanInstallError::Extract {
                message: format!("could not stash existing install dir: {e}"),
            });
        }
    }

    // Move the new install into place.
    if let Err(e) = std::fs::rename(tmp_dir, final_dir) {
        // Restore the previous install so `final_dir` is never left empty.
        if had_existing {
            let _ = std::fs::rename(&backup, final_dir);
        }
        let _ = std::fs::remove_dir_all(tmp_dir);
        return Err(PodmanInstallError::Extract {
            message: format!("could not promote install dir: {e}"),
        });
    }

    // New install is live; drop the backup.
    if had_existing {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// Install from a verified macOS `.pkg` installer.
///
/// Expands the pkg with `pkgutil --expand-full`, searches the expanded tree
/// for `podman` and the named helpers, validates native arch for each, then
/// assembles the Ato install dir and writes `containers.conf` + provenance.
/// All disk writes go to a temp dir; the final dir is only promoted after every
/// check passes.
fn install_from_pkg(
    pkg_bytes: &[u8],
    artifact: &PinnedArtifact,
    host_arch: &str,
    tools_dir: &Path,
    final_dir: &Path,
) -> Result<InstalledPodman, PodmanInstallError> {
    std::fs::create_dir_all(tools_dir).map_err(|e| PodmanInstallError::Extract {
        message: e.to_string(),
    })?;

    let tmp_dir = unique_tmp_install_dir(tools_dir, artifact.version);
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    std::fs::create_dir_all(&tmp_dir).map_err(|e| PodmanInstallError::Extract {
        message: e.to_string(),
    })?;

    let result =
        install_from_pkg_inner(pkg_bytes, artifact, host_arch, final_dir, &tmp_dir);

    let installed = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }
    };

    // Promote (preserves the previous install if the swap fails).
    promote_install_dir(&tmp_dir, final_dir)?;

    Ok(installed)
}

fn install_from_pkg_inner(
    pkg_bytes: &[u8],
    artifact: &PinnedArtifact,
    host_arch: &str,
    final_dir: &Path,
    tmp_dir: &Path,
) -> Result<InstalledPodman, PodmanInstallError> {
    // Expand the pkg into a dedicated sub-dir of tmp so the expanded contents
    // don't interfere with the Ato install layout. NOTE: the destination must
    // NOT exist before `pkgutil --expand-full` runs — pkgutil refuses to write
    // into an existing directory ("File exists"). `expand_pkg` owns creating the
    // parent and clearing any stale destination, so do not pre-create it here.
    let expand_dir = tmp_dir.join("pkg-expanded");
    expand_pkg(pkg_bytes, &expand_dir)?;

    let bin_dir = tmp_dir.join(artifact.helper_binaries_rel_dir);
    std::fs::create_dir_all(&bin_dir).map_err(|e| PodmanInstallError::Extract {
        message: e.to_string(),
    })?;
    let final_bin_dir = final_dir.join(artifact.helper_binaries_rel_dir);

    // Locate, arch-validate, and stage `podman`.
    let podman_src = find_binary_in_tree(&expand_dir, "podman").ok_or_else(|| {
        PodmanInstallError::PkgBinaryNotFound {
            name: "podman".to_string(),
        }
    })?;
    let podman_bytes = std::fs::read(&podman_src).map_err(|e| PodmanInstallError::Extract {
        message: format!("could not read expanded podman for arch check: {e}"),
    })?;
    validate_native_arch(&podman_bytes, host_arch, "podman")?;

    let tmp_podman = bin_dir.join("podman");
    std::fs::write(&tmp_podman, &podman_bytes).map_err(|e| PodmanInstallError::Extract {
        message: format!("could not copy podman: {e}"),
    })?;
    ensure_executable(&tmp_podman)
        .map_err(|message| PodmanInstallError::Extract { message })?;
    verify_binary_runs(&tmp_podman)?;

    // Locate, arch-validate, and stage each declared helper.
    let mut helper_provenance: Vec<HelperProvenance> = Vec::new();
    for &helper_name in artifact.pkg_helper_names {
        let src = find_binary_in_tree(&expand_dir, helper_name).ok_or_else(|| {
            PodmanInstallError::PkgBinaryNotFound {
                name: helper_name.to_string(),
            }
        })?;
        let helper_bytes = std::fs::read(&src).map_err(|e| PodmanInstallError::Extract {
            message: format!("could not read expanded {helper_name} for arch check: {e}"),
        })?;
        validate_native_arch(&helper_bytes, host_arch, helper_name)?;

        let dest = bin_dir.join(helper_name);
        std::fs::write(&dest, &helper_bytes).map_err(|e| PodmanInstallError::Extract {
            message: format!("could not write helper `{helper_name}`: {e}"),
        })?;
        ensure_executable(&dest)
            .map_err(|message| PodmanInstallError::Extract { message })?;

        // For pkg installs the individual helper SHA256 is computed from the
        // extracted bytes (the pkg's overall digest is the security anchor).
        helper_provenance.push(HelperProvenance {
            name: helper_name.to_string(),
            sha256: sha256_hex(&helper_bytes),
            source_url: artifact.url.to_string(),
            path: final_bin_dir.join(helper_name).to_string_lossy().to_string(),
            required_arch: host_arch.to_string(),
        });
    }

    // Verify every declared helper is present and executable in the staged dir.
    for &name in artifact.pkg_helper_names {
        if !is_executable_file(&bin_dir.join(name)) {
            return Err(PodmanInstallError::HelperMissing {
                helper: name.to_string(),
            });
        }
    }

    // Write containers.conf pointing Podman at the (final) bin dir.
    write_containers_conf(tmp_dir, &final_bin_dir)?;

    let final_binary = final_bin_dir.join("podman");
    let provenance = PodmanProvenance {
        version: artifact.version.to_string(),
        sha256: artifact.sha256.to_string(),
        source_url: artifact.url.to_string(),
        binary_path: final_binary.to_string_lossy().to_string(),
        helper_binaries_dir: final_bin_dir.to_string_lossy().to_string(),
        helpers: helper_provenance,
    };
    write_provenance(tmp_dir, &provenance)?;

    // Clean up the expanded pkg tree — large and no longer needed.
    let _ = std::fs::remove_dir_all(&expand_dir);

    Ok(InstalledPodman {
        binary_path: final_binary,
        provenance,
    })
}

/// Removes a path on drop, so a temporary file is cleaned up on *every* exit
/// path — success, error return, or panic. Used to guarantee the temp `.pkg`
/// is deleted even when `pkgutil` cannot be launched (a plain `remove_file`
/// after the call is skipped on early return; that is exactly the kind of leak
/// that breeds stale-path bugs).
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Prepare the destination path for `pkgutil --expand-full`.
///
/// `pkgutil --expand-full` *creates* `dest` itself and aborts with "File exists"
/// if it is already present (this was the PR #584 clean-VM regression: the caller
/// pre-created `pkg-expanded`). So this creates only the *parent* directory and
/// clears any stale `dest` left by a previous interrupted attempt — it must NOT
/// create `dest` itself.
///
/// The stale destination may be a directory, a regular file, a symlink, or a
/// broken symlink. `Path::exists` follows symlinks and silently misses broken
/// ones, so inspect the path entry itself with `symlink_metadata` and remove
/// directories with `remove_dir_all`, everything else (files / symlinks) with
/// `remove_file` (which unlinks the symlink, not its target).
fn prepare_expand_dest(dest: &Path) -> Result<(), PodmanInstallError> {
    let parent = dest.parent().ok_or_else(|| PodmanInstallError::Extract {
        message: format!("pkg expansion destination has no parent: {}", dest.display()),
    })?;
    std::fs::create_dir_all(parent).map_err(|e| PodmanInstallError::Extract {
        message: format!("could not create pkg expansion parent dir: {e}"),
    })?;

    match std::fs::symlink_metadata(dest) {
        Ok(meta) => {
            let removed = if meta.is_dir() {
                std::fs::remove_dir_all(dest)
            } else {
                std::fs::remove_file(dest)
            };
            removed.map_err(|e| PodmanInstallError::Extract {
                message: format!("could not clear stale pkg expansion path: {e}"),
            })?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(PodmanInstallError::Extract {
                message: format!("could not inspect pkg expansion path: {e}"),
            });
        }
    }
    Ok(())
}

/// Outcome of invoking the pkg-expansion command. Split from the live `pkgutil`
/// call so tests can drive `expand_pkg_with_runner` without a real `.pkg`.
struct ExpandRun {
    success: bool,
    status: String,
    stderr: String,
}

/// Expand a macOS `.pkg` file by writing it to a temp file and running
/// `pkgutil --expand-full`. `pkgutil` is an OS-provided tool (not part of
/// Xcode CLT) and is always available on macOS.
fn expand_pkg(pkg_bytes: &[u8], dest: &Path) -> Result<(), PodmanInstallError> {
    expand_pkg_with_runner(pkg_bytes, dest, |pkg_file, dest| {
        let output = std::process::Command::new("pkgutil")
            .args([
                "--expand-full",
                &pkg_file.to_string_lossy(),
                &dest.to_string_lossy(),
            ])
            .output()
            .map_err(|e| PodmanInstallError::Extract {
                message: format!(
                    "`pkgutil --expand-full` could not be launched: {e}. \
                     pkgutil is part of macOS and should always be available."
                ),
            })?;
        Ok(ExpandRun {
            success: output.status.success(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    })
}

/// Core of [`expand_pkg`] with the command invocation injected as `runner`, so
/// the destination-prep + temp-file-cleanup contract is testable without a real
/// `pkgutil` / `.pkg`. The temp `.pkg` is removed on every exit path (success,
/// non-zero exit, or launch error) via [`RemoveOnDrop`].
fn expand_pkg_with_runner<R>(
    pkg_bytes: &[u8],
    dest: &Path,
    runner: R,
) -> Result<(), PodmanInstallError>
where
    R: FnOnce(&Path, &Path) -> Result<ExpandRun, PodmanInstallError>,
{
    prepare_expand_dest(dest)?;

    let pkg_file = dest.with_extension("tmp.pkg");
    std::fs::write(&pkg_file, pkg_bytes).map_err(|e| PodmanInstallError::Extract {
        message: format!("could not write pkg to disk for expansion: {e}"),
    })?;
    // Guard ensures the temp pkg is deleted even if `runner` returns a launch
    // error before we reach any explicit cleanup.
    let _cleanup = RemoveOnDrop(pkg_file.clone());

    let run = runner(&pkg_file, dest)?;

    if !run.success {
        return Err(PodmanInstallError::Extract {
            message: format!("`pkgutil --expand-full` exited {}: {}", run.status, run.stderr),
        });
    }
    Ok(())
}

/// Recursively search `dir` for a file named exactly `name`. Returns the path
/// of the first match. Symlinks are not followed.
///
/// `pkgutil --expand-full` nests files under per-component `Payload/`
/// subdirectories within the expanded tree, so a recursive walk is required.
fn find_binary_in_tree(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            if let Some(found) = find_binary_in_tree(&path, name) {
                return Some(found);
            }
        } else if ft.is_file() && path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

// ── Archive-based install (kept for Linux/Windows follow-up) ─────────────────

fn fetch_and_verify<F: PodmanArtifactFetcher>(
    fetcher: &F,
    url: &str,
    expected_sha256: &str,
) -> Result<Vec<u8>, PodmanInstallError> {
    let bytes = fetcher.fetch(url).map_err(|message| PodmanInstallError::Fetch {
        url: url.to_string(),
        message,
    })?;
    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(PodmanInstallError::DigestMismatch {
            url: url.to_string(),
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    Ok(bytes)
}

fn install_into_temp_then_promote(
    bytes: &[u8],
    artifact: &PinnedArtifact,
    helper_blobs: &[(HelperArtifact, Vec<u8>)],
    host_arch: &str,
    tools_dir: &Path,
    final_dir: &Path,
) -> Result<InstalledPodman, PodmanInstallError> {
    std::fs::create_dir_all(tools_dir).map_err(|e| PodmanInstallError::Extract {
        message: e.to_string(),
    })?;

    let tmp_dir = unique_tmp_install_dir(tools_dir, artifact.version);
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    std::fs::create_dir_all(&tmp_dir).map_err(|e| PodmanInstallError::Extract {
        message: e.to_string(),
    })?;

    let result = (|| {
        extract_archive(bytes, artifact.format, artifact.strip_prefix, &tmp_dir)
            .map_err(|message| PodmanInstallError::Extract { message })?;

        let tmp_binary = tmp_dir.join(artifact.binary_rel_path);
        if !tmp_binary.is_file() {
            return Err(PodmanInstallError::BinaryMissing {
                expected: tmp_binary,
            });
        }
        ensure_executable(&tmp_binary)
            .map_err(|message| PodmanInstallError::Extract { message })?;
        let podman_bytes = std::fs::read(&tmp_binary).map_err(|e| PodmanInstallError::Extract {
            message: format!("could not read extracted podman for arch check: {e}"),
        })?;
        validate_native_arch(&podman_bytes, host_arch, "podman")?;
        verify_binary_runs(&tmp_binary)?;

        let tmp_helper_dir = tmp_dir.join(artifact.helper_binaries_rel_dir);
        let final_helper_dir = final_dir.join(artifact.helper_binaries_rel_dir);
        let mut helper_provenance: Vec<HelperProvenance> = Vec::new();
        if !helper_blobs.is_empty() {
            std::fs::create_dir_all(&tmp_helper_dir).map_err(|e| PodmanInstallError::Extract {
                message: e.to_string(),
            })?;
        }
        for (helper, helper_bytes) in helper_blobs {
            let dest = tmp_helper_dir.join(helper.name);
            std::fs::write(&dest, helper_bytes).map_err(|e| PodmanInstallError::Extract {
                message: format!("could not write helper `{}`: {e}", helper.name),
            })?;
            ensure_executable(&dest)
                .map_err(|message| PodmanInstallError::Extract { message })?;
            helper_provenance.push(HelperProvenance {
                name: helper.name.to_string(),
                sha256: helper.sha256.to_string(),
                source_url: helper.url.to_string(),
                path: final_helper_dir.join(helper.name).to_string_lossy().to_string(),
                required_arch: host_arch.to_string(),
            });
        }
        for helper in artifact.helpers {
            let staged = tmp_helper_dir.join(helper.name);
            if !is_executable_file(&staged) {
                return Err(PodmanInstallError::HelperMissing {
                    helper: helper.name.to_string(),
                });
            }
        }
        if !helper_blobs.is_empty() {
            write_containers_conf(&tmp_dir, &final_helper_dir)?;
        }

        let final_binary = final_dir.join(artifact.binary_rel_path);
        let provenance = PodmanProvenance {
            version: artifact.version.to_string(),
            sha256: artifact.sha256.to_string(),
            source_url: artifact.url.to_string(),
            binary_path: final_binary.to_string_lossy().to_string(),
            helper_binaries_dir: if helper_blobs.is_empty() {
                String::new()
            } else {
                final_helper_dir.to_string_lossy().to_string()
            },
            helpers: helper_provenance,
        };
        write_provenance(&tmp_dir, &provenance)?;
        Ok((final_binary, provenance))
    })();

    let (final_binary, provenance) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }
    };

    // Promote (preserves the previous install if the swap fails).
    promote_install_dir(&tmp_dir, final_dir)?;

    Ok(InstalledPodman {
        binary_path: final_binary,
        provenance,
    })
}

fn verify_binary_runs(binary: &Path) -> Result<(), PodmanInstallError> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| PodmanInstallError::Extract {
            message: format!(
                "extracted binary at '{}' could not be executed: {e}",
                binary.display()
            ),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(PodmanInstallError::Extract {
            message: format!(
                "extracted binary at '{}' exited with {} on `--version`: {}",
                binary.display(),
                output.status,
                stderr.trim()
            ),
        });
    }
    Ok(())
}

pub(crate) fn read_provenance(install_dir: &Path) -> Option<PodmanProvenance> {
    let path = install_dir.join(PROVENANCE_FILE);
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_provenance(
    install_dir: &Path,
    provenance: &PodmanProvenance,
) -> Result<(), PodmanInstallError> {
    let json =
        serde_json::to_vec_pretty(provenance).map_err(|e| PodmanInstallError::Provenance {
            message: e.to_string(),
        })?;
    std::fs::write(install_dir.join(PROVENANCE_FILE), json).map_err(|e| {
        PodmanInstallError::Provenance {
            message: e.to_string(),
        }
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

// ── Mach-O native-arch validation ────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinArch {
    X86_64,
    Arm64,
    Other,
}

fn host_bin_arch(host_arch: &str) -> Option<BinArch> {
    match host_arch {
        "aarch64" => Some(BinArch::Arm64),
        "x86_64" => Some(BinArch::X86_64),
        _ => None,
    }
}

fn arch_from_cputype(cputype: u32) -> BinArch {
    match cputype {
        0x0100_0007 => BinArch::X86_64,
        0x0100_000C => BinArch::Arm64,
        _ => BinArch::Other,
    }
}

fn macho_archs(bytes: &[u8]) -> Option<Vec<BinArch>> {
    if bytes.len() < 8 {
        return None;
    }
    let be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let le = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    match be(0) {
        0xCAFE_BABE | 0xCAFE_BABF => {
            let is_64 = be(0) == 0xCAFE_BABF;
            let entry = if is_64 { 32 } else { 20 };
            let n = be(4) as usize;
            let mut archs = Vec::new();
            let mut off = 8usize;
            for _ in 0..n {
                if off + 4 > bytes.len() {
                    break;
                }
                archs.push(arch_from_cputype(be(off)));
                off += entry;
            }
            Some(archs)
        }
        0xFEED_FACF => Some(vec![arch_from_cputype(be(4))]),
        0xCFFA_EDFE => Some(vec![arch_from_cputype(le(4))]),
        0xFEED_FACE => Some(vec![arch_from_cputype(be(4))]),
        0xCEFA_EDFE => Some(vec![arch_from_cputype(le(4))]),
        _ => None,
    }
}

fn validate_native_arch(
    bytes: &[u8],
    host_arch: &str,
    binary: &str,
) -> Result<(), PodmanInstallError> {
    let Some(want) = host_bin_arch(host_arch) else {
        return Ok(());
    };
    let Some(archs) = macho_archs(bytes) else {
        return Ok(());
    };
    if archs.contains(&want) {
        Ok(())
    } else {
        Err(PodmanInstallError::NotNativeArch {
            binary: binary.to_string(),
            host_arch: host_arch.to_string(),
        })
    }
}

// ── containers.conf generation ────────────────────────────────────────────────

fn write_containers_conf(install_dir: &Path, helper_dir: &Path) -> Result<(), PodmanInstallError> {
    let helper = toml_escape(&helper_dir.to_string_lossy());
    let conf = format!(
        "# Ato-managed Podman configuration — generated by ato, do not edit.\n\
         # Points `podman machine` at the gvproxy/vfkit helpers Ato installed\n\
         # alongside podman so the machine runtime works without Homebrew or\n\
         # system search paths.\n\
         [engine]\n\
         helper_binaries_dir = [\"{helper}\"]\n\
         \n\
         [machine]\n\
         provider = \"{provider}\"\n\
         # Podman defaults `rosetta = true` on Apple Silicon (applehv), which makes\n\
         # `podman machine start` set up a Rosetta guest share and so requires the\n\
         # user to install Rosetta on the host — a hidden prerequisite that breaks\n\
         # the clean-VM promise (it surfaces as a Rosetta install prompt and\n\
         # `vfkit exited unexpectedly with exit code 1` on a Rosetta-less VM).\n\
         # Ato disables it: the machine boots natively (arm64) with no host\n\
         # Rosetta. x86_64 Linux images are emulated in-guest rather than via\n\
         # host Rosetta.\n\
         rosetta = {rosetta}\n",
        helper = helper,
        provider = MACOS_MACHINE_PROVIDER,
        rosetta = MACOS_MACHINE_ROSETTA,
    );
    std::fs::write(install_dir.join(CONTAINERS_CONF_FILE), conf).map_err(|e| {
        PodmanInstallError::Provenance {
            message: format!("could not write {CONTAINERS_CONF_FILE}: {e}"),
        }
    })
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ── Helper completeness preflight ─────────────────────────────────────────────

/// Names of the `podman machine` helpers a pinned artifact for `(os, arch)`
/// requires but that are **missing** from an Ato-managed install's helper dir.
///
/// Returns empty when the runtime is complete, when `podman_bin` is not an
/// Ato-managed install, or when the target needs no helpers.
pub(crate) fn missing_helpers_for(podman_bin: &Path, os: &str, arch: &str) -> Vec<String> {
    let Some(artifact) = pinned_artifact(os, arch) else {
        return Vec::new();
    };
    if !is_ato_managed_install(podman_bin) {
        return Vec::new();
    }

    match artifact.format {
        ArtifactFormat::MacosPkg => {
            if artifact.pkg_helper_names.is_empty() {
                return Vec::new();
            }
            let Some(install_dir) = install_dir_from_pkg_binary(podman_bin, &artifact) else {
                return Vec::new();
            };
            let helper_dir = install_dir.join(artifact.helper_binaries_rel_dir);
            artifact
                .pkg_helper_names
                .iter()
                .filter(|&&name| !is_executable_file(&helper_dir.join(name)))
                .map(|&name| name.to_string())
                .collect()
        }
        ArtifactFormat::TarGz | ArtifactFormat::Zip => {
            if artifact.helpers.is_empty() {
                return Vec::new();
            }
            let Some(install_dir) = install_root(podman_bin, artifact.binary_rel_path) else {
                return Vec::new();
            };
            let helper_dir = install_dir.join(artifact.helper_binaries_rel_dir);
            artifact
                .helpers
                .iter()
                .filter(|h| !is_executable_file(&helper_dir.join(h.name)))
                .map(|h| h.name.to_string())
                .collect()
        }
    }
}

fn is_ato_managed_install(podman_bin: &Path) -> bool {
    capsule_core::common::paths::ato_tools_dir()
        .map(|tools| podman_bin.starts_with(&tools))
        .unwrap_or(false)
}

/// Recover the install root for a pkg-based install where the binary lives at
/// `<install_dir>/<helper_binaries_rel_dir>/podman`.
fn install_dir_from_pkg_binary(podman_bin: &Path, artifact: &PinnedArtifact) -> Option<PathBuf> {
    let mut dir = podman_bin.to_path_buf();
    // Pop the binary filename.
    if !dir.pop() {
        return None;
    }
    // Pop each component of helper_binaries_rel_dir.
    for _ in Path::new(artifact.helper_binaries_rel_dir).components() {
        if !dir.pop() {
            return None;
        }
    }
    Some(dir)
}

/// Recover the install root from a podman binary path by popping the
/// `binary_rel_path` components. Used for archive-based installs.
fn install_root(podman_bin: &Path, binary_rel_path: &str) -> Option<PathBuf> {
    let mut dir = podman_bin.to_path_buf();
    for _ in Path::new(binary_rel_path).components() {
        if !dir.pop() {
            return None;
        }
    }
    Some(dir)
}

// ── Platform helpers ──────────────────────────────────────────────────────────

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn ensure_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let mut perms = meta.permissions();
    let mode = perms.mode();
    if mode & 0o111 == 0 {
        perms.set_mode(mode | 0o755);
        std::fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

// ── Archive extraction (legacy, kept for Linux/Windows follow-up) ─────────────

fn extract_archive(
    bytes: &[u8],
    format: ArtifactFormat,
    strip_prefix: &str,
    dest: &Path,
) -> Result<(), String> {
    match format {
        ArtifactFormat::TarGz => extract_tar_gz(bytes, strip_prefix, dest),
        ArtifactFormat::Zip => extract_zip(bytes, strip_prefix, dest),
        ArtifactFormat::MacosPkg => {
            Err("extract_archive called with MacosPkg format; use install_from_pkg".to_string())
        }
    }
}

fn extract_tar_gz(bytes: &[u8], strip_prefix: &str, dest: &Path) -> Result<(), String> {
    let dec = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(dec);
    archive.set_preserve_permissions(true);
    let entries = archive.entries().map_err(|e| e.to_string())?;
    for entry in entries {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let Some(rel) = sanitize_entry(&path, strip_prefix) else {
            continue;
        };
        let out = dest.join(&rel);
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        entry.unpack(&out).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn extract_zip(bytes: &[u8], strip_prefix: &str, dest: &Path) -> Result<(), String> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(enclosed) = file.enclosed_name() else {
            continue;
        };
        let Some(rel) = sanitize_entry(&enclosed, strip_prefix) else {
            continue;
        };
        let out = dest.join(&rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out_file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut file, &mut out_file).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

fn sanitize_entry(path: &Path, strip_prefix: &str) -> Option<PathBuf> {
    use std::path::Component;
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return None,
        }
    }
    if strip_prefix.is_empty() {
        return Some(path.to_path_buf());
    }
    path.strip_prefix(strip_prefix)
        .ok()
        .map(|p| p.to_path_buf())
        .filter(|p| !p.as_os_str().is_empty())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct FakeFetcher {
        url: String,
        bytes: Vec<u8>,
    }

    impl PodmanArtifactFetcher for FakeFetcher {
        fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            if url == self.url {
                Ok(self.bytes.clone())
            } else {
                Err(format!("unexpected url: {url}"))
            }
        }
    }

    struct MapFetcher {
        responses: std::collections::HashMap<String, Vec<u8>>,
    }

    impl PodmanArtifactFetcher for MapFetcher {
        fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            self.responses
                .get(url)
                .cloned()
                .ok_or_else(|| format!("unexpected url: {url}"))
        }
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn artifact_with_helpers(
        archive: &[u8],
        gvproxy: &[u8],
        vfkit: &[u8],
    ) -> (PinnedArtifact, MapFetcher) {
        let archive_url = "https://example.test/podman.tar.gz";
        let gvproxy_url = "https://example.test/gvproxy";
        let vfkit_url = "https://example.test/vfkit";
        let helpers: &'static [HelperArtifact] = Box::leak(Box::new([
            HelperArtifact {
                name: "gvproxy",
                url: gvproxy_url,
                sha256: leak(sha256_hex(gvproxy)),
            },
            HelperArtifact {
                name: "vfkit",
                url: vfkit_url,
                sha256: leak(sha256_hex(vfkit)),
            },
        ]));
        let artifact = PinnedArtifact {
            version: "9.9.9-test",
            url: archive_url,
            sha256: leak(sha256_hex(archive)),
            format: ArtifactFormat::TarGz,
            binary_rel_path: "usr/bin/podman",
            strip_prefix: "",
            helpers,
            helper_binaries_rel_dir: "usr/bin",
            pkg_helper_names: &[],
        };
        let mut responses = std::collections::HashMap::new();
        responses.insert(archive_url.to_string(), archive.to_vec());
        responses.insert(gvproxy_url.to_string(), gvproxy.to_vec());
        responses.insert(vfkit_url.to_string(), vfkit.to_vec());
        (artifact, MapFetcher { responses })
    }

    fn tar_gz_with(inner_path: &str, contents: &[u8]) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_data(&mut header, inner_path, contents)
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&tar_bytes).unwrap();
        enc.finish().unwrap()
    }

    fn test_artifact_for(bytes: &[u8]) -> PinnedArtifact {
        let digest: &'static str = Box::leak(sha256_hex(bytes).into_boxed_str());
        PinnedArtifact {
            version: "9.9.9-test",
            url: "https://example.test/podman.tar.gz",
            sha256: digest,
            format: ArtifactFormat::TarGz,
            binary_rel_path: "usr/bin/podman",
            strip_prefix: "",
            helpers: &[],
            helper_binaries_rel_dir: "usr/bin",
            pkg_helper_names: &[],
        }
    }

    fn install_with_artifact<F: PodmanArtifactFetcher>(
        fetcher: &F,
        artifact: &PinnedArtifact,
        tools_dir: &Path,
    ) -> Result<InstalledPodman, PodmanInstallError> {
        install_with_artifact_on(fetcher, artifact, "ato-test-arch", tools_dir)
    }

    fn install_with_artifact_on<F: PodmanArtifactFetcher>(
        fetcher: &F,
        artifact: &PinnedArtifact,
        host_arch: &str,
        tools_dir: &Path,
    ) -> Result<InstalledPodman, PodmanInstallError> {
        let bytes = fetch_and_verify(fetcher, artifact.url, artifact.sha256)?;
        let mut helper_blobs: Vec<(HelperArtifact, Vec<u8>)> = Vec::new();
        for helper in artifact.helpers {
            let helper_bytes = fetch_and_verify(fetcher, helper.url, helper.sha256)?;
            validate_native_arch(&helper_bytes, host_arch, helper.name)?;
            helper_blobs.push((*helper, helper_bytes));
        }
        let final_dir = tools_dir.join(format!("podman-{}", artifact.version));
        install_into_temp_then_promote(&bytes, artifact, &helper_blobs, host_arch, tools_dir, &final_dir)
    }

    fn thin_macho(arch: BinArch) -> Vec<u8> {
        let cputype: u32 = match arch {
            BinArch::X86_64 => 0x0100_0007,
            BinArch::Arm64 => 0x0100_000C,
            BinArch::Other => 0x0000_0007,
        };
        let mut v = Vec::new();
        v.extend_from_slice(&[0xCF, 0xFA, 0xED, 0xFE]);
        v.extend_from_slice(&cputype.to_le_bytes());
        v.extend_from_slice(&[0u8; 24]);
        v
    }

    fn fat_macho(archs: &[BinArch]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        v.extend_from_slice(&(archs.len() as u32).to_be_bytes());
        for a in archs {
            let cputype: u32 = match a {
                BinArch::X86_64 => 0x0100_0007,
                BinArch::Arm64 => 0x0100_000C,
                BinArch::Other => 0x0000_0007,
            };
            v.extend_from_slice(&cputype.to_be_bytes());
            v.extend_from_slice(&[0u8; 16]);
        }
        v
    }

    fn podman_stub_script() -> &'static [u8] {
        b"#!/bin/sh\necho \"podman version 5.8.2\"\n"
    }

    // ── Strategy tests ────────────────────────────────────────────────────────

    #[test]
    fn strategies_prefer_brew_when_present() {
        let s = install_strategies(true, true);
        assert_eq!(s.first(), Some(&PodmanInstallStrategy::Homebrew));
        assert!(s.contains(&PodmanInstallStrategy::AtoManaged));
        assert!(s.contains(&PodmanInstallStrategy::ManualInstructions));
    }

    #[test]
    fn strategies_fall_through_to_ato_managed_without_brew() {
        let s = install_strategies(false, true);
        assert!(!s.contains(&PodmanInstallStrategy::Homebrew));
        assert_eq!(s.first(), Some(&PodmanInstallStrategy::AtoManaged));
        assert_eq!(s.last(), Some(&PodmanInstallStrategy::ManualInstructions));
    }

    #[test]
    fn strategies_always_offer_a_non_brew_option() {
        let s = install_strategies(false, false);
        assert_eq!(s, vec![PodmanInstallStrategy::ManualInstructions]);
    }

    // ── Archive-format installer tests (unchanged from prior PRs) ─────────────

    #[test]
    fn ato_managed_installer_extracts_verified_binary() {
        let bytes = tar_gz_with("usr/bin/podman", podman_stub_script());
        let artifact = test_artifact_for(&bytes);
        let fetcher = FakeFetcher {
            url: artifact.url.to_string(),
            bytes: bytes.clone(),
        };
        let tools = tempfile::tempdir().unwrap();
        let installed = install_with_artifact(&fetcher, &artifact, tools.path())
            .expect("install succeeds with matching digest");
        assert!(installed.binary_path.is_file());
        assert!(installed.binary_path.ends_with("usr/bin/podman"));
    }

    #[test]
    fn ato_managed_installer_promotes_atomically_and_leaves_no_temp_dir() {
        let bytes = tar_gz_with("usr/bin/podman", podman_stub_script());
        let artifact = test_artifact_for(&bytes);
        let fetcher = FakeFetcher {
            url: artifact.url.to_string(),
            bytes: bytes.clone(),
        };
        let tools = tempfile::tempdir().unwrap();
        install_with_artifact(&fetcher, &artifact, tools.path()).expect("install");
        let final_dir = tools.path().join("podman-9.9.9-test");
        assert!(final_dir.is_dir());
        let leftover_tmp: Vec<_> = std::fs::read_dir(tools.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("podman-9.9.9-test.tmp-")
            })
            .collect();
        assert!(leftover_tmp.is_empty(), "{leftover_tmp:?}");
    }

    #[cfg(unix)]
    #[test]
    fn ato_managed_installer_rejects_non_runnable_binary() {
        let bytes = tar_gz_with("usr/bin/podman", b"#!/bin/sh\nexit 7\n");
        let artifact = test_artifact_for(&bytes);
        let fetcher = FakeFetcher {
            url: artifact.url.to_string(),
            bytes: bytes.clone(),
        };
        let tools = tempfile::tempdir().unwrap();
        let err = install_with_artifact(&fetcher, &artifact, tools.path())
            .expect_err("non-runnable binary must fail the install");
        assert!(matches!(err, PodmanInstallError::Extract { .. }), "{err:?}");
        let final_dir = tools.path().join("podman-9.9.9-test");
        assert!(!final_dir.exists());
        let any_tmp = std::fs::read_dir(tools.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!any_tmp);
    }

    #[test]
    fn ato_managed_installer_rejects_digest_mismatch() {
        let bytes = tar_gz_with("usr/bin/podman", b"real");
        let mut artifact = test_artifact_for(&bytes);
        artifact.sha256 = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef0";
        let fetcher = FakeFetcher {
            url: artifact.url.to_string(),
            bytes,
        };
        let tools = tempfile::tempdir().unwrap();
        let err = install_with_artifact(&fetcher, &artifact, tools.path())
            .expect_err("digest mismatch must fail closed");
        assert!(matches!(err, PodmanInstallError::DigestMismatch { .. }), "{err:?}");
        let install_dir = tools.path().join("podman-9.9.9-test");
        assert!(!install_dir.join("usr/bin/podman").exists());
    }

    #[test]
    fn ato_managed_installer_records_version_digest_source() {
        let bytes = tar_gz_with("usr/bin/podman", podman_stub_script());
        let artifact = test_artifact_for(&bytes);
        let fetcher = FakeFetcher {
            url: artifact.url.to_string(),
            bytes: bytes.clone(),
        };
        let tools = tempfile::tempdir().unwrap();
        let installed = install_with_artifact(&fetcher, &artifact, tools.path()).expect("install");
        assert_eq!(installed.provenance.version, "9.9.9-test");
        assert_eq!(installed.provenance.sha256, sha256_hex(&bytes));
        assert_eq!(installed.provenance.source_url, artifact.url);
        let install_dir = tools.path().join("podman-9.9.9-test");
        let on_disk = read_provenance(&install_dir).expect("provenance file written");
        assert_eq!(on_disk, installed.provenance);
    }

    #[test]
    fn no_pinned_artifact_is_typed_error_not_brew() {
        let fetcher = FakeFetcher {
            url: String::new(),
            bytes: Vec::new(),
        };
        let tools = tempfile::tempdir().unwrap();
        let err = install_ato_managed_podman(&fetcher, "plan9", "sparc", tools.path())
            .expect_err("unknown target");
        assert!(matches!(err, PodmanInstallError::NoPinnedArtifact { .. }), "{err:?}");
        let msg = err.to_string();
        assert!(!msg.to_lowercase().contains("homebrew"), "{msg}");
        assert!(msg.contains("podman.io"), "{msg}");
    }

    #[test]
    fn sanitize_entry_rejects_traversal() {
        assert!(sanitize_entry(Path::new("../escape"), "").is_none());
        assert!(sanitize_entry(Path::new("/abs/path"), "").is_none());
        assert_eq!(
            sanitize_entry(Path::new("usr/bin/podman"), ""),
            Some(PathBuf::from("usr/bin/podman"))
        );
        assert_eq!(
            sanitize_entry(Path::new("podman-5.2.3/usr/bin/podman"), "podman-5.2.3"),
            Some(PathBuf::from("usr/bin/podman"))
        );
    }

    // ── Helper bundle tests (archive format) ──────────────────────────────────

    #[test]
    fn ato_managed_installer_bundles_helpers_and_writes_containers_conf() {
        let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
        let (artifact, fetcher) =
            artifact_with_helpers(&archive, b"#!/bin/sh\nexit 0\n", b"#!/bin/sh\nexit 0\n");
        let tools = tempfile::tempdir().unwrap();
        let installed = install_with_artifact(&fetcher, &artifact, tools.path())
            .expect("install with a complete helper bundle succeeds");
        let helper_dir = installed.binary_path.parent().unwrap();
        for helper in ["gvproxy", "vfkit"] {
            assert!(is_executable_file(&helper_dir.join(helper)), "{helper}");
        }
        let install_dir = tools.path().join("podman-9.9.9-test");
        let conf = std::fs::read_to_string(install_dir.join(CONTAINERS_CONF_FILE))
            .expect("containers.conf written");
        assert!(conf.contains("helper_binaries_dir"), "{conf}");
        assert!(conf.contains(MACOS_MACHINE_PROVIDER), "{conf}");
        assert!(conf.contains("rosetta = false"), "{conf}");
        assert_eq!(installed.provenance.helpers.len(), 2);
    }

    #[test]
    fn ato_managed_installer_rejects_helper_digest_mismatch_before_promotion() {
        let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
        let (mut artifact, fetcher) =
            artifact_with_helpers(&archive, b"real-gvproxy", b"real-vfkit");
        let helpers: &'static [HelperArtifact] = Box::leak(Box::new([
            HelperArtifact {
                name: "gvproxy",
                url: artifact.helpers[0].url,
                sha256: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef0",
            },
            artifact.helpers[1],
        ]));
        artifact.helpers = helpers;
        let tools = tempfile::tempdir().unwrap();
        let err = install_with_artifact(&fetcher, &artifact, tools.path())
            .expect_err("a bad helper digest must fail closed");
        assert!(matches!(err, PodmanInstallError::DigestMismatch { .. }), "{err:?}");
        assert!(!tools.path().join("podman-9.9.9-test").exists());
    }

    // ── Mach-O parser tests ───────────────────────────────────────────────────

    #[test]
    fn macho_parser_reads_thin_and_fat_archs() {
        assert_eq!(macho_archs(&thin_macho(BinArch::Arm64)), Some(vec![BinArch::Arm64]));
        assert_eq!(macho_archs(&thin_macho(BinArch::X86_64)), Some(vec![BinArch::X86_64]));
        assert_eq!(
            macho_archs(&fat_macho(&[BinArch::X86_64, BinArch::Arm64])),
            Some(vec![BinArch::X86_64, BinArch::Arm64])
        );
        assert_eq!(macho_archs(b"#!/bin/sh\nexit 0\n"), None);
    }

    #[test]
    fn validate_native_arch_rejects_non_native_macho_but_skips_scripts() {
        let x86 = thin_macho(BinArch::X86_64);
        assert!(matches!(
            validate_native_arch(&x86, "aarch64", "vfkit"),
            Err(PodmanInstallError::NotNativeArch { .. })
        ));
        let fat = fat_macho(&[BinArch::X86_64, BinArch::Arm64]);
        assert!(validate_native_arch(&fat, "aarch64", "vfkit").is_ok());
        assert!(validate_native_arch(&fat, "x86_64", "vfkit").is_ok());
        assert!(validate_native_arch(&thin_macho(BinArch::Arm64), "aarch64", "vfkit").is_ok());
        assert!(validate_native_arch(b"#!/bin/sh\n", "aarch64", "vfkit").is_ok());
        assert!(validate_native_arch(&x86, "riscv64", "vfkit").is_ok());
    }

    #[test]
    fn install_rejects_x86_only_helper_on_arm64_before_promotion() {
        let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
        let (artifact, fetcher) = artifact_with_helpers(
            &archive,
            &thin_macho(BinArch::X86_64),
            &fat_macho(&[BinArch::X86_64, BinArch::Arm64]),
        );
        let tools = tempfile::tempdir().unwrap();
        let err = install_with_artifact_on(&fetcher, &artifact, "aarch64", tools.path())
            .expect_err("x86_64-only helper on arm64 must be rejected");
        assert!(
            matches!(err, PodmanInstallError::NotNativeArch { ref binary, .. } if binary == "gvproxy"),
            "{err:?}"
        );
        assert!(err.to_string().to_lowercase().contains("rosetta"), "{err}");
        assert!(!tools.path().join("podman-9.9.9-test").exists());
    }

    #[test]
    fn install_accepts_universal_helpers_on_both_arm64_and_x86_64() {
        for host in ["aarch64", "x86_64"] {
            let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
            let (artifact, fetcher) = artifact_with_helpers(
                &archive,
                &fat_macho(&[BinArch::X86_64, BinArch::Arm64]),
                &fat_macho(&[BinArch::X86_64, BinArch::Arm64]),
            );
            let tools = tempfile::tempdir().unwrap();
            let installed = install_with_artifact_on(&fetcher, &artifact, host, tools.path())
                .unwrap_or_else(|e| panic!("universal helpers must install on {host}: {e:?}"));
            assert_eq!(installed.provenance.helpers[0].required_arch, host);
        }
    }

    // ── Pkg format: find_binary_in_tree ───────────────────────────────────────

    fn make_fake_pkg_tree(root: &Path, binaries: &[(&str, &[u8])]) {
        let payload_dir = root.join("io.podman.pkg").join("Payload").join("usr").join("bin");
        std::fs::create_dir_all(&payload_dir).unwrap();
        for (name, contents) in binaries {
            let path = payload_dir.join(name);
            std::fs::write(&path, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    #[test]
    fn find_binary_in_tree_locates_nested_binary() {
        let root = tempfile::tempdir().unwrap();
        make_fake_pkg_tree(
            root.path(),
            &[("podman", b"#!/bin/sh\n"), ("gvproxy", b"x"), ("vfkit", b"y")],
        );
        for name in ["podman", "gvproxy", "vfkit"] {
            let found = find_binary_in_tree(root.path(), name);
            assert!(found.is_some(), "{name} must be found in fake pkg tree");
            assert_eq!(found.unwrap().file_name().unwrap(), name);
        }
        assert!(
            find_binary_in_tree(root.path(), "absent").is_none(),
            "missing binary returns None"
        );
    }

    // ── pkg expansion destination handling (PR #584 regression) ───────────────

    // `pkgutil --expand-full` aborts with "File exists" if its destination is
    // pre-created. `prepare_expand_dest` must create only the parent and leave
    // `dest` itself absent.
    #[test]
    fn prepare_expand_dest_creates_parent_but_not_dest() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("tmp-install").join("pkg-expanded");

        prepare_expand_dest(&dest).unwrap();

        assert!(
            dest.parent().unwrap().is_dir(),
            "parent dir must exist for pkgutil to write into"
        );
        assert!(
            !dest.exists(),
            "dest must NOT be pre-created — pkgutil refuses an existing destination"
        );
    }

    // A stale `pkg-expanded` from a previous interrupted attempt must be cleared
    // (this is the exact clean-VM failure: re-running install hit "File exists").
    #[test]
    fn prepare_expand_dest_clears_stale_destination() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("tmp-install").join("pkg-expanded");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("leftover"), b"stale").unwrap();
        assert!(dest.exists());

        prepare_expand_dest(&dest).unwrap();

        assert!(
            !dest.exists(),
            "a pre-existing dest must be removed before expansion"
        );
        assert!(dest.parent().unwrap().is_dir());
    }

    // Each call must yield a distinct temp dir within one process, so a retry or
    // concurrent attempt never collides on the same path.
    #[test]
    fn unique_tmp_install_dir_is_distinct_per_call() {
        let root = tempfile::tempdir().unwrap();
        let a = unique_tmp_install_dir(root.path(), "5.8.2");
        let b = unique_tmp_install_dir(root.path(), "5.8.2");
        assert_ne!(a, b, "two calls must produce different temp dirs");
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("podman-5.8.2.tmp-"));
    }

    // A stale destination that is a regular file (not a dir) must also be cleared
    // — `exists()` + `remove_dir_all` alone would fail with "Not a directory".
    #[test]
    fn prepare_expand_dest_clears_stale_file() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("tmp-install").join("pkg-expanded");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"i am a file, not a dir").unwrap();

        prepare_expand_dest(&dest).unwrap();

        assert!(!dest.exists(), "a stale regular file at dest must be removed");
        assert!(dest.parent().unwrap().is_dir());
    }

    // A stale (even broken) symlink at dest must be unlinked — `exists()` follows
    // symlinks and misses broken ones, so this guards the symlink_metadata path.
    #[cfg(unix)]
    #[test]
    fn prepare_expand_dest_clears_stale_symlink() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("tmp-install").join("pkg-expanded");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        // Symlink to a non-existent target → broken symlink.
        std::os::unix::fs::symlink(root.path().join("does-not-exist"), &dest).unwrap();
        assert!(std::fs::symlink_metadata(&dest).is_ok(), "symlink entry exists");

        prepare_expand_dest(&dest).unwrap();

        assert!(
            std::fs::symlink_metadata(&dest).is_err(),
            "a stale (broken) symlink at dest must be unlinked"
        );
        assert!(dest.parent().unwrap().is_dir());
    }

    // ── expand_pkg_with_runner contract ───────────────────────────────────────

    // On a successful run: the runner sees an absent dest with an existing parent,
    // and the temp `.pkg` is cleaned up afterwards.
    #[test]
    fn expand_pkg_runner_sees_absent_dest_and_cleans_temp_on_success() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("tmp-install").join("pkg-expanded");

        let mut observed_dest_absent = false;
        let mut observed_parent_present = false;
        let mut observed_pkg_written = false;
        let res = expand_pkg_with_runner(b"fake pkg bytes", &dest, |pkg_file, d| {
            observed_dest_absent = !d.exists();
            observed_parent_present = d.parent().unwrap().is_dir();
            observed_pkg_written = pkg_file.is_file();
            Ok(ExpandRun { success: true, status: "exit status: 0".into(), stderr: String::new() })
        });

        assert!(res.is_ok());
        assert!(observed_dest_absent, "runner must see dest absent (pkgutil creates it)");
        assert!(observed_parent_present, "runner must see parent present");
        assert!(observed_pkg_written, "temp pkg must exist while runner runs");
        assert!(
            !dest.with_extension("tmp.pkg").exists(),
            "temp pkg must be removed after success"
        );
    }

    // A non-zero exit returns an error AND still removes the temp `.pkg`.
    #[test]
    fn expand_pkg_runner_nonzero_exit_errors_and_cleans_temp() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("tmp-install").join("pkg-expanded");

        let res = expand_pkg_with_runner(b"fake", &dest, |_pkg_file, _d| {
            Ok(ExpandRun {
                success: false,
                status: "exit status: 1".into(),
                stderr: "boom".into(),
            })
        });

        assert!(matches!(res, Err(PodmanInstallError::Extract { .. })));
        assert!(
            !dest.with_extension("tmp.pkg").exists(),
            "temp pkg must be removed even on non-zero exit"
        );
    }

    // The headline leak fix: a launch error (runner returns Err before any
    // explicit cleanup) must STILL remove the temp `.pkg`.
    #[test]
    fn expand_pkg_runner_launch_error_still_cleans_temp() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("tmp-install").join("pkg-expanded");

        let res = expand_pkg_with_runner(b"fake", &dest, |_pkg_file, _d| {
            Err(PodmanInstallError::Extract {
                message: "could not be launched".into(),
            })
        });

        assert!(matches!(res, Err(PodmanInstallError::Extract { .. })));
        assert!(
            !dest.with_extension("tmp.pkg").exists(),
            "temp pkg must be removed even when the runner cannot launch the command"
        );
    }

    // ── promote_install_dir preserves the previous install ─────────────────────

    // A successful promotion moves tmp into place and leaves no temp/backup dirs.
    #[test]
    fn promote_install_dir_swaps_and_leaves_no_residue() {
        let root = tempfile::tempdir().unwrap();
        let tmp = root.path().join("podman-5.8.2.tmp-1-0");
        let final_dir = root.path().join("podman-5.8.2");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("marker"), b"new").unwrap();

        promote_install_dir(&tmp, &final_dir).unwrap();

        assert!(final_dir.join("marker").is_file(), "new install is live");
        assert!(!tmp.exists(), "temp dir consumed by rename");
        // No leftover backup siblings.
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert!(leftovers.is_empty(), "no backup dir should remain");
    }

    // Promoting over an existing install replaces it; the old contents are gone
    // and the new contents are live (no empty-window data loss).
    #[test]
    fn promote_install_dir_replaces_existing_install() {
        let root = tempfile::tempdir().unwrap();
        let tmp = root.path().join("podman-5.8.2.tmp-1-1");
        let final_dir = root.path().join("podman-5.8.2");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("new-marker"), b"new").unwrap();
        std::fs::create_dir_all(&final_dir).unwrap();
        std::fs::write(final_dir.join("old-marker"), b"old").unwrap();

        promote_install_dir(&tmp, &final_dir).unwrap();

        assert!(final_dir.join("new-marker").is_file(), "new contents live");
        assert!(!final_dir.join("old-marker").exists(), "old contents replaced");
        assert!(!tmp.exists());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak-"))
            .collect();
        assert!(leftovers.is_empty(), "backup removed after successful swap");
    }

    // ── Stale install detection ───────────────────────────────────────────────

    #[test]
    fn stale_remote_zip_install_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let stale = PodmanProvenance {
            version: "5.2.3".to_string(),
            sha256: "abc".to_string(),
            source_url: "https://github.com/containers/podman/releases/download/v5.2.3/podman-remote-release-darwin_arm64.zip".to_string(),
            binary_path: String::new(),
            helper_binaries_dir: String::new(),
            helpers: Vec::new(),
        };
        let json = serde_json::to_vec_pretty(&stale).unwrap();
        std::fs::write(dir.path().join(PROVENANCE_FILE), json).unwrap();
        assert!(is_stale_remote_zip_install(dir.path()));
    }

    #[test]
    fn pkg_derived_install_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = PodmanProvenance {
            version: "5.8.2".to_string(),
            sha256: "abc".to_string(),
            source_url: "https://github.com/podman-container-tools/podman/releases/download/v5.8.2/podman-installer-macos-arm64.pkg".to_string(),
            binary_path: String::new(),
            helper_binaries_dir: String::new(),
            helpers: Vec::new(),
        };
        let json = serde_json::to_vec_pretty(&fresh).unwrap();
        std::fs::write(dir.path().join(PROVENANCE_FILE), json).unwrap();
        assert!(!is_stale_remote_zip_install(dir.path()));
    }

    // ── Pinned artifact metadata ──────────────────────────────────────────────

    #[test]
    fn pinned_macos_targets_use_macos_pkg_format() {
        for arch in ["aarch64", "x86_64"] {
            let a = pinned_artifact("macos", arch).expect("macos artifact pinned");
            assert_eq!(a.version, PINNED_PODMAN_VERSION);
            assert!(a.url.starts_with("https://"), "{arch}: url must be https");
            assert!(
                a.url.ends_with(".pkg"),
                "{arch}: macOS artifact must be a .pkg, got: {}",
                a.url
            );
            assert!(
                a.url.contains("podman-container-tools"),
                "{arch}: must use podman-container-tools releases, got: {}",
                a.url
            );
            assert!(
                a.helpers.is_empty(),
                "{arch}: MacosPkg must have no separate helper downloads"
            );
            assert_eq!(a.format, ArtifactFormat::MacosPkg, "{arch}");
            assert!(a.pkg_helper_names.contains(&"gvproxy"), "{arch}");
            assert!(a.pkg_helper_names.contains(&"vfkit"), "{arch}");
            assert!(!a.sha256.is_empty(), "{arch}: sha256 must not be empty");
        }
    }

    #[test]
    fn pkg_binary_not_found_error_is_typed_and_not_brew() {
        let err = PodmanInstallError::PkgBinaryNotFound {
            name: "vfkit".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("vfkit"), "{msg}");
        assert!(!msg.to_lowercase().contains("homebrew"), "{msg}");
    }

    #[test]
    fn toml_escape_handles_quotes_and_backslashes() {
        assert_eq!(toml_escape("/Users/a b/tools"), "/Users/a b/tools");
        assert_eq!(toml_escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(toml_escape(r"a\b"), r"a\\b");
    }

    // ── Real network smoke tests (macOS, ignored by default) ──────────────────

    /// Real network smoke: download the pinned pkg, digest-verify, expand,
    /// extract and run `podman --version`, verify helpers land with native arch.
    ///
    /// Run:  cargo test -p ato-cli --lib -- --ignored real_pkg_install
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[ignore = "real network download; run manually on macOS arm64 (SHA256s are pinned)"]
    #[test]
    fn real_pkg_install_downloads_and_runs() {
        let tools = tempfile::tempdir().unwrap();
        let installed =
            install_ato_managed_podman(&ReqwestArtifactFetcher, "macos", "aarch64", tools.path())
                .expect("real install (download + digest verify + pkg expand + run) succeeds");
        assert!(installed.binary_path.is_file());
        let out = std::process::Command::new(&installed.binary_path)
            .arg("--version")
            .output()
            .expect("run extracted podman --version");
        assert!(out.status.success(), "podman --version must exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(PINNED_PODMAN_VERSION),
            "podman --version output should mention {PINNED_PODMAN_VERSION}: {stdout}"
        );
        let install_dir = tools.path().join(format!("podman-{PINNED_PODMAN_VERSION}"));
        let helper_dir = install_dir.join("bin");
        for helper in ["gvproxy", "vfkit"] {
            let path = helper_dir.join(helper);
            assert!(is_executable_file(&path), "{helper} must be installed");
            let bytes = std::fs::read(&path).expect("read helper");
            let archs = macho_archs(&bytes).expect("helper is a Mach-O");
            assert!(
                archs.contains(&BinArch::Arm64),
                "{helper} must contain a native arm64 slice: {archs:?}"
            );
        }
        let conf = install_dir.join(CONTAINERS_CONF_FILE);
        let conf_text = std::fs::read_to_string(&conf).expect("containers.conf written");
        assert!(conf_text.contains("helper_binaries_dir"), "{conf_text}");
        assert!(conf_text.contains(MACOS_MACHINE_PROVIDER), "{conf_text}");
        assert!(conf_text.contains("rosetta = false"), "{conf_text}");
        assert!(
            missing_helpers_for(&installed.binary_path, "macos", "aarch64").is_empty(),
            "a real install must report no missing helpers"
        );
    }
}
