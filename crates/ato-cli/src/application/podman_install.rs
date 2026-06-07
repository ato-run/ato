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
//! ## Updating the pinned release
//!
//! To bump the Podman version, update [`PINNED_PODMAN_VERSION`] and the matching
//! per-OS/arch entries in [`pinned_artifact`] (URL + `sha256`). Obtain the
//! digest from the official Podman release `shasums` file. The digest is the
//! security anchor — never leave it blank or guess it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Pinned Podman version Ato installs when no Homebrew/system copy exists.
///
/// Bump this together with the per-OS/arch entries in [`pinned_artifact`].
pub(crate) const PINNED_PODMAN_VERSION: &str = "5.2.3";

/// File name of the provenance manifest written next to an Ato-managed install.
const PROVENANCE_FILE: &str = "ato-podman-provenance.json";

/// A pinned, digest-verified Podman release artifact for one OS/arch.
///
/// The `podman` CLI archive is **not** a complete macOS Podman *machine*
/// runtime: `podman machine init/start` additionally needs helper binaries
/// (`gvproxy` for networking, `vfkit` as the Apple Hypervisor VM provider).
/// A working `podman --version` is therefore *not* sufficient — a clean macOS
/// VM fails with `could not find "gvproxy"` unless the helpers are installed
/// too. So a pinned artifact carries its required [`helpers`](Self::helpers),
/// which Ato downloads, digest-verifies, and places in
/// [`helper_binaries_rel_dir`](Self::helper_binaries_rel_dir) alongside the
/// `podman` binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PinnedArtifact {
    /// Podman version (matches [`PINNED_PODMAN_VERSION`]).
    pub version: &'static str,
    /// Direct download URL of the release archive.
    pub url: &'static str,
    /// Lowercase hex SHA256 of the archive at `url`. The security anchor.
    pub sha256: &'static str,
    /// Archive format, so the extractor knows how to unpack.
    pub format: ArtifactFormat,
    /// Path of the `podman` binary *inside* the extracted archive, relative to
    /// the install dir after `strip_prefix` is applied.
    pub binary_rel_path: &'static str,
    /// Leading path component to strip from every archive entry (release
    /// archives wrap everything in a top-level `podman-<ver>/` dir). Empty to
    /// strip nothing.
    pub strip_prefix: &'static str,
    /// Helper binaries `podman machine` requires on this OS/arch (e.g.
    /// `gvproxy`, `vfkit` on macOS). Each is downloaded + digest-verified and
    /// installed into [`helper_binaries_rel_dir`](Self::helper_binaries_rel_dir).
    /// Empty when the target needs no extra helpers.
    pub helpers: &'static [HelperArtifact],
    /// Directory (relative to the install dir) the helper binaries are placed
    /// in *and* that the generated `containers.conf` points Podman at via
    /// `[engine] helper_binaries_dir`. Kept next to the `podman` binary so a
    /// single dir holds the whole runtime.
    pub helper_binaries_rel_dir: &'static str,
}

/// A pinned, digest-verified `podman machine` helper binary (a single
/// executable, not an archive). Downloaded and verified exactly like the main
/// artifact, then written into the install's helper dir under [`name`](Self::name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HelperArtifact {
    /// File name the helper is installed as (the name Podman searches for, e.g.
    /// `"gvproxy"`, `"vfkit"`).
    pub name: &'static str,
    /// Direct download URL of the helper binary.
    pub url: &'static str,
    /// Lowercase hex SHA256 of the helper at `url`. The security anchor.
    pub sha256: &'static str,
}

// ── Pinned macOS machine helpers (the versions Podman v5.2.3 itself bundles) ──
//
// Source of the versions (authoritative): Podman's own macOS `.pkg` definition
// at the release tag pins these. Verify with:
//
//   curl -sL https://raw.githubusercontent.com/containers/podman/v5.2.3/contrib/pkginstaller/Makefile \
//     | grep -E 'GVPROXY_VERSION|VFKIT_VERSION'
//   # => GVPROXY_VERSION ?= 0.7.5   VFKIT_VERSION ?= 0.5.1
//
// We pin the same versions so the Ato machine runs the combination Podman
// expects. Both ship as **universal** (x86_64 + arm64) Mach-O binaries, so one
// set serves both Mac architectures.
//
// Digests were obtained by downloading each asset and hashing it. Re-verify a
// pin (or a bump) with, e.g.:
//
//   curl -sL -o gvproxy https://github.com/containers/gvisor-tap-vsock/releases/download/v0.7.5/gvproxy-darwin
//   curl -sL -o vfkit   https://github.com/crc-org/vfkit/releases/download/v0.5.1/vfkit
//   shasum -a 256 gvproxy vfkit
//
// vfkit uses the **signed** `vfkit` asset (not `vfkit-unsigned`): it carries the
// `com.apple.security.virtualization` entitlement vfkit needs to boot a VM —
// confirm with `codesign -d --entitlements - vfkit`. The unsigned variant that
// Podman re-signs during packaging would be Gatekeeper-blocked as a standalone
// download. gvproxy needs no special entitlement.

/// gvproxy 0.7.5 (universal darwin), the network helper.
const GVPROXY_DARWIN_URL: &str =
    "https://github.com/containers/gvisor-tap-vsock/releases/download/v0.7.5/gvproxy-darwin";
const GVPROXY_DARWIN_SHA256: &str =
    "ca881d38963456bdf56b596bc2d76dfa72b565e701acf584d749a1543915f800";

/// vfkit 0.5.1 (universal darwin, **signed**), the Apple Hypervisor VM provider.
const VFKIT_DARWIN_URL: &str =
    "https://github.com/crc-org/vfkit/releases/download/v0.5.1/vfkit";
const VFKIT_DARWIN_SHA256: &str =
    "6adf8ab2fb0a3b7e7d778554bdc4ae8a8d9e8f984cebffd4e0c8ff8ea5f08447";

/// The macOS `podman machine` helper bundle (shared by both architectures).
const MACOS_PODMAN_HELPERS: &[HelperArtifact] = &[
    HelperArtifact {
        name: "gvproxy",
        url: GVPROXY_DARWIN_URL,
        sha256: GVPROXY_DARWIN_SHA256,
    },
    HelperArtifact {
        name: "vfkit",
        url: VFKIT_DARWIN_URL,
        sha256: VFKIT_DARWIN_SHA256,
    },
];

/// Podman machine provider Ato pins on macOS. `applehv` (Apple Hypervisor, via
/// `vfkit`) is the default on modern macOS and needs no extra packages beyond
/// `gvproxy` + `vfkit`; pinning it keeps the Ato machine off the `libkrun`
/// (`krunkit`) path so the bundled helpers are sufficient.
const MACOS_MACHINE_PROVIDER: &str = "applehv";

/// Whether the Ato machine enables Podman's Rosetta guest share. `false`:
/// Rosetta must never be a hidden prerequisite of an Ato-managed runtime (it
/// would prompt for a host Rosetta install on a clean Apple Silicon VM and fail
/// vfkit when declined). See [`write_containers_conf`].
const MACOS_MACHINE_ROSETTA: bool = false;

/// File name of the Ato-generated Podman config written next to an install. It
/// points Podman at the bundled helper dir so `podman machine` finds `gvproxy`
/// and `vfkit` without relying on Homebrew/system search paths.
const CONTAINERS_CONF_FILE: &str = "containers.conf";

/// Supported release-archive formats.
///
/// `TarGz` is the format Linux/Windows managed installs will use (a documented
/// follow-up); the pinned macOS artifacts ship as `Zip`. Kept here so the
/// extractor already handles both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactFormat {
    #[cfg_attr(not(test), allow(dead_code))]
    TarGz,
    Zip,
}

/// Resolve the pinned artifact for an `(os, arch)` pair, or `None` when Ato has
/// no pinned managed install for that target.
///
/// `os` / `arch` use Rust's `std::env::consts` spelling (`"macos"`,
/// `"aarch64"`, `"x86_64"`, `"windows"`).
///
/// The macOS digests below are the real SHA256s from Podman's official v5.2.3
/// `shasums`; the arm64 archive was downloaded and its digest + internal layout
/// (`podman-5.2.3/usr/bin/podman`) verified locally. A mismatch still fails
/// closed (never runs an unverified binary). A manual clean-VM smoke of the full
/// auto-install is still worthwhile, but the framework + verified digests are in
/// place.
pub(crate) fn pinned_artifact(os: &str, arch: &str) -> Option<PinnedArtifact> {
    match (os, arch) {
        // The darwin release zips wrap everything in a top-level
        // `podman-<version>/` directory (`podman-5.2.3/usr/bin/podman`, confirmed
        // against the real archive), so `strip_prefix` drops that wrapper and
        // `binary_rel_path` is the path beneath it.
        ("macos", "aarch64") => Some(PinnedArtifact {
            version: PINNED_PODMAN_VERSION,
            url: "https://github.com/containers/podman/releases/download/v5.2.3/podman-remote-release-darwin_arm64.zip",
            // From the official Podman v5.2.3 `shasums`; the downloaded archive's
            // SHA256 + internal layout were verified to match this value locally (arm64).
            sha256: "1449ceb220907ca94407ca3a2a7d5d7909602657d3f5ea9cab26e4dd7c366b69",
            format: ArtifactFormat::Zip,
            binary_rel_path: "usr/bin/podman",
            strip_prefix: "podman-5.2.3",
            // The remote-client zip ships only `podman`; `podman machine` needs
            // gvproxy + vfkit, so Ato installs them next to it.
            helpers: MACOS_PODMAN_HELPERS,
            helper_binaries_rel_dir: "usr/bin",
        }),
        ("macos", "x86_64") => Some(PinnedArtifact {
            version: PINNED_PODMAN_VERSION,
            url: "https://github.com/containers/podman/releases/download/v5.2.3/podman-remote-release-darwin_amd64.zip",
            // From the official Podman v5.2.3 `shasums` (amd64).
            sha256: "6a7ef2eb934e7b5f002bcc662314fd43013f9452edb2be0889d23da8e201f514",
            format: ArtifactFormat::Zip,
            binary_rel_path: "usr/bin/podman",
            strip_prefix: "podman-5.2.3",
            helpers: MACOS_PODMAN_HELPERS,
            helper_binaries_rel_dir: "usr/bin",
        }),
        // Windows/Linux keep their existing instruction-based path (they do not
        // hard-require Homebrew today). Ato-managed installs for those targets
        // are a documented follow-up.
        _ => None,
    }
}

/// The available Podman install strategies, in the order Ato should try them.
///
/// `brew_present` gates the Homebrew strategy: it is only offered when a usable
/// `brew` already exists. Ato never installs Homebrew itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PodmanInstallStrategy {
    /// Install via an already-present Homebrew (`brew install podman`).
    Homebrew,
    /// Download + digest-verify + extract a pinned release into `~/.ato/tools`.
    AtoManaged,
    /// Typed, actionable last-resort instructions (never "install Homebrew").
    ManualInstructions,
}

/// Ordered strategies for the given host. `brew_present` is whether a usable
/// `brew` binary already exists. `managed_available` is whether a pinned
/// managed artifact exists for the host OS/arch.
///
/// Pure, so the policy is unit-testable. Invariant: Homebrew is only ever
/// offered when already present, and never as the *only* option — the
/// Ato-managed and/or manual strategies always follow so a brew-less host can
/// still make progress.
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

/// Typed failures from the Ato-managed installer. Each carries an actionable
/// message; none ever instructs the user to install Homebrew.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PodmanInstallError {
    /// No pinned managed artifact exists for this OS/arch.
    NoPinnedArtifact { os: String, arch: String },
    /// The network fetch failed.
    Fetch { url: String, message: String },
    /// The downloaded artifact's digest did not match the pinned digest. Fail
    /// closed: the binary is never extracted or executed.
    DigestMismatch {
        url: String,
        expected: String,
        actual: String,
    },
    /// Extracting / writing the archive failed.
    Extract { message: String },
    /// The expected podman binary was not present in the extracted archive.
    BinaryMissing { expected: PathBuf },
    /// A required `podman machine` helper binary (e.g. `gvproxy`, `vfkit`) was
    /// not present/executable in the install after staging. The install is
    /// rejected **before promotion** — an incomplete runtime is never published.
    /// This is an Ato packaging/runtime issue, not a user Homebrew/git issue.
    HelperMissing { helper: String },
    /// A bundled binary (podman or a helper) is a Mach-O that does **not**
    /// contain a native slice for the host architecture, so running it would
    /// require Rosetta on Apple Silicon — a hidden prerequisite Ato must never
    /// introduce. Rejected **before promotion**. Packaging issue, not a user
    /// issue.
    NotNativeArch {
        binary: String,
        host_arch: String,
    },
    /// Writing the provenance manifest failed.
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
            Self::DigestMismatch {
                url,
                expected,
                actual,
            } => write!(
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
            Self::HelperMissing { helper } => write!(
                f,
                "Ato-managed Podman is incomplete: required helper binary `{helper}` was not \
                 found. This is an Ato packaging/runtime setup issue, not a user Homebrew/git \
                 issue."
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

/// Network seam for fetching a release artifact. Implemented by the real HTTP
/// client in production and by a fake in tests so the installer is exercised
/// without touching the network.
pub(crate) trait PodmanArtifactFetcher {
    /// Download `url`, returning the raw bytes.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Production fetcher backed by the existing blocking `reqwest` client.
pub(crate) struct ReqwestArtifactFetcher;

impl PodmanArtifactFetcher for ReqwestArtifactFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("ato-cli/", env!("CARGO_PKG_VERSION")))
            // Bound the network so a dead/slow connection surfaces a typed
            // Fetch error instead of hanging Runtime Setup indefinitely. The
            // archive is ~25MB; 300s total leaves headroom on slow links while
            // a 15s connect timeout fails fast when there is no route.
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(url).send().map_err(|e| e.to_string())?;
        let resp = resp.error_for_status().map_err(|e| e.to_string())?;
        let bytes = resp.bytes().map_err(|e| e.to_string())?;
        Ok(bytes.to_vec())
    }
}

/// Provenance of an Ato-managed Podman install, written as JSON next to the
/// extracted binary so the install is auditable (version + digest + source).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PodmanProvenance {
    pub version: String,
    /// Lowercase hex SHA256 of the verified source archive.
    pub sha256: String,
    /// Source URL the archive was downloaded from.
    pub source_url: String,
    /// Path of the installed podman binary.
    pub binary_path: String,
    /// Absolute directory the bundled `podman machine` helpers were installed
    /// into and that the generated `containers.conf` points Podman at. Empty for
    /// targets that need no helpers. `#[serde(default)]` keeps older provenance
    /// files (written before the bundle existed) readable.
    #[serde(default)]
    pub helper_binaries_dir: String,
    /// Provenance of each bundled helper (name + verified digest + source).
    #[serde(default)]
    pub helpers: Vec<HelperProvenance>,
}

/// Provenance of one bundled `podman machine` helper binary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HelperProvenance {
    pub name: String,
    /// Lowercase hex SHA256 of the verified helper binary.
    pub sha256: String,
    /// Source URL the helper was downloaded from.
    pub source_url: String,
    /// Path of the installed helper binary.
    pub path: String,
    /// Host architecture the helper was validated to contain a native Mach-O
    /// slice for (`std::env::consts::ARCH` spelling). `#[serde(default)]` keeps
    /// older provenance files readable.
    #[serde(default)]
    pub required_arch: String,
}

/// Result of a successful Ato-managed install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledPodman {
    /// Absolute path to the installed, digest-verified podman binary.
    pub binary_path: PathBuf,
    pub provenance: PodmanProvenance,
}

/// Download, digest-verify, extract, and record an Ato-managed Podman install
/// for `(os, arch)` under `tools_dir` (normally `~/.ato/tools`).
///
/// Fails closed on a digest mismatch — the archive is verified *before* any
/// extraction, so an unverified binary is never written or run.
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

    let bytes = fetch_and_verify(fetcher, artifact.url, artifact.sha256)?;

    // Fetch + digest-verify every required helper BEFORE touching disk too, so
    // the whole machine runtime is fail-closed: a bad/missing helper aborts the
    // install before any byte is written, never producing a half-runtime that
    // `podman --version` would wrongly call "ready".
    let mut helper_blobs: Vec<(HelperArtifact, Vec<u8>)> = Vec::with_capacity(artifact.helpers.len());
    for helper in artifact.helpers {
        let helper_bytes = fetch_and_verify(fetcher, helper.url, helper.sha256)?;
        // Native-arch gate (fail-closed): a helper that is a Mach-O without the
        // host's slice would run under Rosetta — never install it.
        validate_native_arch(&helper_bytes, arch, helper.name)?;
        helper_blobs.push((*helper, helper_bytes));
    }

    let install_dir = tools_dir.join(format!("podman-{}", artifact.version));

    // Extract into a temp sibling dir, validate (binary present + runnable +
    // native arch, helpers present + executable), write containers.conf +
    // provenance, then atomically rename into place. Any failure removes the
    // temp dir so a partial install is never left behind. The atomic rename
    // means observers only ever see a fully-validated install dir.
    install_into_temp_then_promote(&bytes, &artifact, &helper_blobs, arch, tools_dir, &install_dir)
}

/// Download `url` and verify its SHA256 matches `expected_sha256` (fail-closed:
/// a mismatch is a hard error before the bytes are used for anything).
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

/// Perform the disk-mutating half of an Ato-managed install atomically.
///
/// Extracts into `tools_dir/podman-<version>.tmp-<pid>`, validates the binary is
/// present and runnable (`<binary> --version` exits 0), writes provenance into
/// the temp dir, then renames the temp dir over `final_dir`. On any failure the
/// temp dir is removed, leaving no partial install.
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

    let tmp_dir = tools_dir.join(format!(
        "podman-{}.tmp-{}",
        artifact.version,
        std::process::id()
    ));
    // Clear a stale temp dir from a prior crashed run before reusing the name.
    if tmp_dir.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    std::fs::create_dir_all(&tmp_dir).map_err(|e| PodmanInstallError::Extract {
        message: e.to_string(),
    })?;

    // From here on, every early return must clean up the temp dir.
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
        // The podman binary must be native too — a non-native podman would also
        // pull in Rosetta. Validate before running it.
        let podman_bytes = std::fs::read(&tmp_binary).map_err(|e| PodmanInstallError::Extract {
            message: format!("could not read extracted podman for arch check: {e}"),
        })?;
        validate_native_arch(&podman_bytes, host_arch, "podman")?;
        // Prove the extracted binary actually runs on this host before we
        // promote it. A non-zero exit (or spawn failure) fails the install.
        verify_binary_runs(&tmp_binary)?;

        // Stage the machine helpers (gvproxy/vfkit) into the install's helper
        // dir, then validate every required helper is present + executable. A
        // missing/non-executable helper rejects the install BEFORE promotion —
        // an incomplete machine runtime is never published.
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
        // The runtime is only as complete as its helpers: every helper the
        // artifact declares must now exist and be executable in the helper dir.
        for helper in artifact.helpers {
            let staged = tmp_helper_dir.join(helper.name);
            if !is_executable_file(&staged) {
                return Err(PodmanInstallError::HelperMissing {
                    helper: helper.name.to_string(),
                });
            }
        }

        // Point Podman at the bundled helper dir (and pin the VM provider) via
        // an Ato-owned containers.conf, so `podman machine` finds gvproxy/vfkit
        // without depending on Homebrew/system search paths. Written only when
        // the target actually ships helpers.
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
            // No partial install left behind.
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e);
        }
    };

    // Promote atomically: drop a stale final dir immediately before the rename
    // so the window where neither dir is in place is as small as possible.
    if final_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(final_dir) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(PodmanInstallError::Extract {
                message: format!("could not clear existing install dir: {e}"),
            });
        }
    }
    if let Err(e) = std::fs::rename(&tmp_dir, final_dir) {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err(PodmanInstallError::Extract {
            message: format!("could not promote install dir: {e}"),
        });
    }

    Ok(InstalledPodman {
        binary_path: final_binary,
        provenance,
    })
}

/// Run `<binary> --version` and require a clean (exit 0) run. Proves the
/// extracted binary is actually executable on this host before promotion.
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

/// Read the provenance manifest for an Ato-managed install dir, if present.
/// Used for auditing an existing install (version/digest/source).
#[cfg_attr(not(test), allow(dead_code))]
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

/// A CPU architecture we care about for native-execution validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinArch {
    X86_64,
    Arm64,
    /// Some other / unrecognized Mach-O cputype.
    Other,
}

/// Map the host arch string (`std::env::consts::ARCH` spelling) to the arch a
/// native binary must contain. `None` for arches we don't gate.
fn host_bin_arch(host_arch: &str) -> Option<BinArch> {
    match host_arch {
        "aarch64" => Some(BinArch::Arm64),
        "x86_64" => Some(BinArch::X86_64),
        _ => None,
    }
}

fn arch_from_cputype(cputype: u32) -> BinArch {
    // CPU_TYPE_X86_64 = 0x0100_0007, CPU_TYPE_ARM64 = CPU_TYPE_ARM(12) | ABI64.
    match cputype {
        0x0100_0007 => BinArch::X86_64,
        0x0100_000C => BinArch::Arm64,
        _ => BinArch::Other,
    }
}

/// Architectures present in a Mach-O image (thin or fat/universal).
///
/// Returns `None` when `bytes` is **not** a Mach-O at all (a shell script, an
/// ELF, etc.) — callers then skip the native-arch gate, since it only applies
/// to macOS Mach-O binaries. A minimal header parser: no external `lipo`/Xcode
/// CLT dependency, so a clean VM needs no developer tooling.
fn macho_archs(bytes: &[u8]) -> Option<Vec<BinArch>> {
    if bytes.len() < 8 {
        return None;
    }
    let be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let le = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    // The on-disk first word read big-endian distinguishes every variant.
    match be(0) {
        // Fat / universal: magic + cputype fields are big-endian. FAT_MAGIC(_64).
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
        // Thin 64-bit, big-endian image (MH_MAGIC_64): cputype is big-endian.
        0xFEED_FACF => Some(vec![arch_from_cputype(be(4))]),
        // Thin 64-bit, little-endian image (MH_CIGAM_64) — the normal arm64 /
        // x86_64 layout on Apple: cputype is little-endian.
        0xCFFA_EDFE => Some(vec![arch_from_cputype(le(4))]),
        // Thin 32-bit (BE / LE). Neither arm64 nor x86_64, but recognize them so
        // a 32-bit binary is treated as a Mach-O (and thus arch-gated), not skipped.
        0xFEED_FACE => Some(vec![arch_from_cputype(be(4))]),
        0xCEFA_EDFE => Some(vec![arch_from_cputype(le(4))]),
        _ => None,
    }
}

/// Reject a bundled binary that is a Mach-O lacking a native slice for
/// `host_arch` (it would run under Rosetta). Non-Mach-O inputs and un-gated
/// host arches pass — the gate only constrains real macOS binaries.
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

/// Write the Ato-owned `containers.conf` that points Podman at the bundled
/// helper dir and pins the macOS VM provider. `helper_dir` is the **final**
/// (post-promotion) absolute path so the config is valid the instant the
/// install dir is renamed into place.
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

/// Escape a string for a TOML basic string (only `\` and `"` need handling for
/// the filesystem paths we emit).
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Names of the `podman machine` helpers a pinned artifact for `(os, arch)`
/// requires but that are **missing** (absent or non-executable) from an
/// Ato-managed install's helper dir.
///
/// Returns empty when the runtime is complete, when the target needs no
/// helpers, or when `podman_bin` is not an Ato-managed install — Ato only
/// polices the layout it owns; a Homebrew/system Podman is trusted to bring its
/// own helpers. This is the preflight that turns the clean-VM
/// `could not find "gvproxy"` failure into an actionable, typed error *before*
/// `podman machine init` runs.
pub(crate) fn missing_helpers_for(podman_bin: &Path, os: &str, arch: &str) -> Vec<String> {
    let Some(artifact) = pinned_artifact(os, arch) else {
        return Vec::new();
    };
    if artifact.helpers.is_empty() || !is_ato_managed_install(podman_bin) {
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

/// Whether `podman_bin` is an Ato-managed install (lives under `~/.ato/tools`).
fn is_ato_managed_install(podman_bin: &Path) -> bool {
    capsule_core::common::paths::ato_tools_dir()
        .map(|tools| podman_bin.starts_with(&tools))
        .unwrap_or(false)
}

/// Recover the install root from a podman binary path by popping the
/// `binary_rel_path` components (e.g. `usr/bin/podman`) off the end.
fn install_root(podman_bin: &Path, binary_rel_path: &str) -> Option<PathBuf> {
    let mut dir = podman_bin.to_path_buf();
    for _ in Path::new(binary_rel_path).components() {
        if !dir.pop() {
            return None;
        }
    }
    Some(dir)
}

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

/// Extract `bytes` (a `.tar.gz` or `.zip`) into `dest`, stripping a leading
/// `strip_prefix` path component from every entry. Rejects path-traversal
/// (`..`/absolute) entries.
fn extract_archive(
    bytes: &[u8],
    format: ArtifactFormat,
    strip_prefix: &str,
    dest: &Path,
) -> Result<(), String> {
    match format {
        ArtifactFormat::TarGz => extract_tar_gz(bytes, strip_prefix, dest),
        ArtifactFormat::Zip => extract_zip(bytes, strip_prefix, dest),
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

/// Strip `strip_prefix` from `path` and reject traversal. Returns `None` for
/// entries that escape (`..`/absolute) or that lie outside `strip_prefix`.
fn sanitize_entry(path: &Path, strip_prefix: &str) -> Option<PathBuf> {
    use std::path::Component;
    // Reject any traversal or absolute components outright.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Fake fetcher that returns canned bytes for a URL and never touches the
    /// network.
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

    /// Fetcher that serves canned bytes for several URLs (the main archive plus
    /// each helper), for the machine-runtime-bundle tests.
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

    /// Leak `s` into a `&'static str` so it can live in a `&'static` artifact
    /// field. Tests only.
    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    /// Build a test artifact + a [`MapFetcher`] for a podman archive plus two
    /// helper binaries, with digests that match the canned bytes. Returns the
    /// artifact and the fetcher.
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
        };
        let mut responses = std::collections::HashMap::new();
        responses.insert(archive_url.to_string(), archive.to_vec());
        responses.insert(gvproxy_url.to_string(), gvproxy.to_vec());
        responses.insert(vfkit_url.to_string(), vfkit.to_vec());
        (artifact, MapFetcher { responses })
    }

    /// Build a tar.gz containing a single executable file at `inner_path`.
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

    /// A pinned artifact whose URL/digest match `bytes`, for tests. Uses the
    /// tar.gz format with the binary at `usr/bin/podman` (no strip prefix).
    fn test_artifact_for(bytes: &[u8]) -> PinnedArtifact {
        // Leak the digest so it can live in a `&'static str` field. Tests only.
        let digest: &'static str = Box::leak(sha256_hex(bytes).into_boxed_str());
        PinnedArtifact {
            version: "9.9.9-test",
            url: "https://example.test/podman.tar.gz",
            sha256: digest,
            format: ArtifactFormat::TarGz,
            binary_rel_path: "usr/bin/podman",
            strip_prefix: "",
            // The base test artifact needs no helpers; bundle tests use
            // [`artifact_with_helpers`].
            helpers: &[],
            helper_binaries_rel_dir: "usr/bin",
        }
    }

    /// Run [`install_ato_managed_podman`]'s body against an explicit artifact,
    /// bypassing the OS/arch table so tests are host-independent. Mirrors the
    /// real flow: verify the archive, then fetch + verify each helper, then run
    /// the atomic temp-then-promote path.
    fn install_with_artifact<F: PodmanArtifactFetcher>(
        fetcher: &F,
        artifact: &PinnedArtifact,
        tools_dir: &Path,
    ) -> Result<InstalledPodman, PodmanInstallError> {
        // Use an un-gated host arch by default so script-stub fixtures (not
        // Mach-O) are unaffected; arch-specific tests call the variant below.
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
        // Exercise the real atomic temp-then-promote path (with exec validation)
        // so tests cover what production runs.
        install_into_temp_then_promote(&bytes, artifact, &helper_blobs, host_arch, tools_dir, &final_dir)
    }

    /// Bytes of a minimal **thin** Mach-O header for `arch` — just enough for
    /// the header parser (magic + cputype). Not a runnable binary; helpers are
    /// never executed, so this exercises the arch gate hermetically.
    fn thin_macho(arch: BinArch) -> Vec<u8> {
        let cputype: u32 = match arch {
            BinArch::X86_64 => 0x0100_0007,
            BinArch::Arm64 => 0x0100_000C,
            BinArch::Other => 0x0000_0007,
        };
        let mut v = Vec::new();
        // MH_CIGAM_64 on disk (little-endian image): bytes CF FA ED FE.
        v.extend_from_slice(&[0xCF, 0xFA, 0xED, 0xFE]);
        v.extend_from_slice(&cputype.to_le_bytes()); // cputype, little-endian
        v.extend_from_slice(&[0u8; 24]); // pad out a plausible header
        v
    }

    /// Bytes of a minimal **fat/universal** Mach-O header covering `archs`.
    fn fat_macho(archs: &[BinArch]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes()); // FAT_MAGIC
        v.extend_from_slice(&(archs.len() as u32).to_be_bytes());
        for a in archs {
            let cputype: u32 = match a {
                BinArch::X86_64 => 0x0100_0007,
                BinArch::Arm64 => 0x0100_000C,
                BinArch::Other => 0x0000_0007,
            };
            v.extend_from_slice(&cputype.to_be_bytes()); // cputype (big-endian)
            v.extend_from_slice(&[0u8; 16]); // cpusubtype/offset/size/align
        }
        v
    }

    /// Bytes of a tiny `podman` stub that exits 0 on `--version`, so the new
    /// exec-validation passes hermetically. On unix this is a `#!/bin/sh`
    /// script; the extractor marks it executable from the tar mode (0o755).
    fn podman_stub_script() -> &'static [u8] {
        b"#!/bin/sh\necho \"podman version 5.2.3\"\n"
    }

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
        // brew absent => never offered; managed comes first, manual last.
        assert!(!s.contains(&PodmanInstallStrategy::Homebrew));
        assert_eq!(s.first(), Some(&PodmanInstallStrategy::AtoManaged));
        assert_eq!(s.last(), Some(&PodmanInstallStrategy::ManualInstructions));
    }

    #[test]
    fn strategies_always_offer_a_non_brew_option() {
        // Even with neither brew nor a managed artifact, manual remains so a
        // brew-less host is never told to install Homebrew.
        let s = install_strategies(false, false);
        assert_eq!(s, vec![PodmanInstallStrategy::ManualInstructions]);
    }

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

        // Final dir exists; no `.tmp-*` sibling is left behind.
        let final_dir = tools.path().join("podman-9.9.9-test");
        assert!(final_dir.is_dir(), "final install dir must exist");
        let leftover_tmp: Vec<_> = std::fs::read_dir(tools.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("podman-9.9.9-test.tmp-")
            })
            .collect();
        assert!(
            leftover_tmp.is_empty(),
            "no temp install dir may survive a successful install: {leftover_tmp:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ato_managed_installer_rejects_non_runnable_binary() {
        // A binary that exits non-zero on `--version` must fail the install and
        // leave nothing behind (no final dir, no temp dir).
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
        assert!(!final_dir.exists(), "no partial install dir may remain");
        let any_tmp = std::fs::read_dir(tools.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!any_tmp, "no temp install dir may remain after failure");
    }

    #[test]
    fn ato_managed_installer_rejects_digest_mismatch() {
        let bytes = tar_gz_with("usr/bin/podman", b"real");
        let mut artifact = test_artifact_for(&bytes);
        // Pin a digest that does NOT match the bytes.
        artifact.sha256 = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef0";
        let fetcher = FakeFetcher {
            url: artifact.url.to_string(),
            bytes,
        };
        let tools = tempfile::tempdir().unwrap();
        let err = install_with_artifact(&fetcher, &artifact, tools.path())
            .expect_err("digest mismatch must fail closed");
        assert!(
            matches!(err, PodmanInstallError::DigestMismatch { .. }),
            "{err:?}"
        );
        // Nothing extracted: no podman binary anywhere under the tools dir.
        let install_dir = tools.path().join("podman-9.9.9-test");
        assert!(
            !install_dir.join("usr/bin/podman").exists(),
            "unverified binary must never be written"
        );
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

        // Returned provenance carries version + digest + source.
        assert_eq!(installed.provenance.version, "9.9.9-test");
        assert_eq!(installed.provenance.sha256, sha256_hex(&bytes));
        assert_eq!(installed.provenance.source_url, artifact.url);

        // And it is persisted to disk for audit.
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
        assert!(
            matches!(err, PodmanInstallError::NoPinnedArtifact { .. }),
            "{err:?}"
        );
        let msg = err.to_string();
        assert!(
            !msg.to_lowercase().contains("homebrew"),
            "must not mention Homebrew: {msg}"
        );
        assert!(
            msg.contains("podman.io"),
            "should point at podman.io: {msg}"
        );
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

    /// Real network smoke: download the pinned darwin/arm64 artifact with the
    /// PRODUCTION fetcher, digest-verify, extract, and run the extracted
    /// `podman --version`. Ignored by default (needs network + macOS arm64); run
    /// with `cargo test -p ato-cli --lib -- --ignored real_ato_managed_install`.
    /// Uses a throwaway tools dir, never the user's real `~/.ato`.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[ignore = "real network download; run manually"]
    #[test]
    fn real_ato_managed_install_downloads_verifies_and_runs() {
        let tools = tempfile::tempdir().unwrap();
        let installed =
            install_ato_managed_podman(&ReqwestArtifactFetcher, "macos", "aarch64", tools.path())
                .expect("real install (download + digest verify + extract + run) succeeds");
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

        // The machine runtime is only complete with its helpers: a real install
        // must also have downloaded + verified gvproxy and vfkit next to podman,
        // and written a containers.conf pointing Podman at them.
        let install_dir = tools.path().join(format!("podman-{PINNED_PODMAN_VERSION}"));
        let helper_dir = install_dir.join("usr/bin");
        for helper in ["gvproxy", "vfkit"] {
            let path = helper_dir.join(helper);
            assert!(
                is_executable_file(&path),
                "real install must bundle an executable {helper}"
            );
            // Real evidence the pinned helpers are native arm64 (NOT Intel-only,
            // so no Rosetta) — the #578 concern, checked against the actual bytes.
            let bytes = std::fs::read(&path).expect("read helper");
            let archs = macho_archs(&bytes).expect("helper is a Mach-O");
            assert!(
                archs.contains(&BinArch::Arm64),
                "{helper} must contain a native arm64 slice: {archs:?}"
            );
        }
        let conf = install_dir.join(CONTAINERS_CONF_FILE);
        let conf_text = std::fs::read_to_string(&conf).expect("containers.conf written");
        assert!(
            conf_text.contains("helper_binaries_dir"),
            "containers.conf must set helper_binaries_dir: {conf_text}"
        );
        assert!(
            conf_text.contains(MACOS_MACHINE_PROVIDER),
            "containers.conf must pin the machine provider: {conf_text}"
        );
        // And the resolved-binary preflight sees a complete runtime.
        assert!(
            missing_helpers_for(&installed.binary_path, "macos", "aarch64").is_empty(),
            "a real install must report no missing helpers"
        );
    }

    #[test]
    fn pinned_macos_targets_have_urls_and_digests() {
        for arch in ["aarch64", "x86_64"] {
            let a = pinned_artifact("macos", arch).expect("macos artifact pinned");
            assert_eq!(a.version, PINNED_PODMAN_VERSION);
            assert!(a.url.starts_with("https://"));
            assert_eq!(a.sha256.len(), 64, "digest must be a 64-hex SHA256");
            // The `podman` archive is not a complete machine runtime: each macOS
            // target must pin gvproxy + vfkit with real digests so the bundle is
            // verifiable and fail-closed.
            let names: Vec<&str> = a.helpers.iter().map(|h| h.name).collect();
            assert!(names.contains(&"gvproxy"), "must bundle gvproxy: {names:?}");
            assert!(names.contains(&"vfkit"), "must bundle vfkit: {names:?}");
            for helper in a.helpers {
                assert!(helper.url.starts_with("https://"), "{}", helper.name);
                assert_eq!(
                    helper.sha256.len(),
                    64,
                    "helper {} digest must be 64-hex SHA256",
                    helper.name
                );
            }
            assert!(
                !a.helper_binaries_rel_dir.is_empty(),
                "helper dir must be set when helpers are pinned"
            );
        }
    }

    #[test]
    fn ato_managed_installer_bundles_helpers_and_writes_containers_conf() {
        let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
        let (artifact, fetcher) =
            artifact_with_helpers(&archive, b"#!/bin/sh\nexit 0\n", b"#!/bin/sh\nexit 0\n");
        let tools = tempfile::tempdir().unwrap();
        let installed = install_with_artifact(&fetcher, &artifact, tools.path())
            .expect("install with a complete helper bundle succeeds");

        // Helpers landed next to podman and are executable.
        let helper_dir = installed.binary_path.parent().unwrap();
        for helper in ["gvproxy", "vfkit"] {
            assert!(
                is_executable_file(&helper_dir.join(helper)),
                "{helper} must be installed + executable"
            );
        }
        // containers.conf points Podman at the (final) helper dir + provider.
        let install_dir = tools.path().join("podman-9.9.9-test");
        let conf = std::fs::read_to_string(install_dir.join(CONTAINERS_CONF_FILE))
            .expect("containers.conf written");
        assert!(conf.contains("helper_binaries_dir"), "{conf}");
        assert!(
            conf.contains(&helper_dir.to_string_lossy().to_string()),
            "containers.conf must reference the install helper dir: {conf}"
        );
        assert!(conf.contains(MACOS_MACHINE_PROVIDER), "{conf}");
        // Rosetta must be disabled so a clean Apple Silicon VM is never prompted
        // to install Rosetta (the #578 clean-VM failure).
        assert!(
            conf.contains("rosetta = false"),
            "containers.conf must disable Rosetta: {conf}"
        );

        // Provenance records the helper digests + dir for audit.
        assert_eq!(installed.provenance.helpers.len(), 2);
        assert_eq!(
            installed.provenance.helper_binaries_dir,
            helper_dir.to_string_lossy().to_string()
        );
        let on_disk = read_provenance(&install_dir).expect("provenance file");
        assert_eq!(on_disk, installed.provenance);
    }

    #[test]
    fn ato_managed_installer_rejects_helper_digest_mismatch_before_promotion() {
        let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
        let (mut artifact, fetcher) =
            artifact_with_helpers(&archive, b"real-gvproxy", b"real-vfkit");
        // Corrupt the gvproxy digest so its download fails closed.
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
        assert!(
            matches!(err, PodmanInstallError::DigestMismatch { .. }),
            "{err:?}"
        );
        // No partial install: neither the final dir nor a temp dir survives.
        assert!(!tools.path().join("podman-9.9.9-test").exists());
        let any_tmp = std::fs::read_dir(tools.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!any_tmp, "no temp install dir may remain after a failed helper");
    }

    #[test]
    fn macho_parser_reads_thin_and_fat_archs() {
        assert_eq!(macho_archs(&thin_macho(BinArch::Arm64)), Some(vec![BinArch::Arm64]));
        assert_eq!(macho_archs(&thin_macho(BinArch::X86_64)), Some(vec![BinArch::X86_64]));
        assert_eq!(
            macho_archs(&fat_macho(&[BinArch::X86_64, BinArch::Arm64])),
            Some(vec![BinArch::X86_64, BinArch::Arm64])
        );
        // A shell script (the helper-stub shape) is not a Mach-O → None (skipped).
        assert_eq!(macho_archs(b"#!/bin/sh\nexit 0\n"), None);
    }

    #[test]
    fn validate_native_arch_rejects_non_native_macho_but_skips_scripts() {
        // x86_64-only Mach-O on an arm64 host → rejected (would need Rosetta).
        let x86 = thin_macho(BinArch::X86_64);
        assert!(matches!(
            validate_native_arch(&x86, "aarch64", "vfkit"),
            Err(PodmanInstallError::NotNativeArch { .. })
        ));
        // Universal passes on both hosts.
        let fat = fat_macho(&[BinArch::X86_64, BinArch::Arm64]);
        assert!(validate_native_arch(&fat, "aarch64", "vfkit").is_ok());
        assert!(validate_native_arch(&fat, "x86_64", "vfkit").is_ok());
        // Native thin passes.
        assert!(validate_native_arch(&thin_macho(BinArch::Arm64), "aarch64", "vfkit").is_ok());
        // Non-Mach-O (script) is not gated.
        assert!(validate_native_arch(b"#!/bin/sh\n", "aarch64", "vfkit").is_ok());
        // Un-gated host arch is not constrained.
        assert!(validate_native_arch(&x86, "riscv64", "vfkit").is_ok());
    }

    #[test]
    fn install_rejects_x86_only_helper_on_arm64_before_promotion() {
        // The #578-hypothesized failure mode: a helper that is x86_64-only on an
        // arm64 host must be rejected BEFORE promotion (it would pull in Rosetta),
        // with a typed packaging error — never a generic failure.
        let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
        let (artifact, fetcher) = artifact_with_helpers(
            &archive,
            &thin_macho(BinArch::X86_64), // gvproxy: x86_64-only
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
        // Fail-closed: no promotion, no temp dir.
        assert!(!tools.path().join("podman-9.9.9-test").exists());
        let any_tmp = std::fs::read_dir(tools.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".tmp-"));
        assert!(!any_tmp, "no temp install dir may remain after a non-native helper");
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

    #[cfg(unix)]
    #[test]
    fn helper_completeness_check_flags_a_removed_helper() {
        // Install a complete bundle, then delete gvproxy to simulate an
        // incomplete runtime. The same present-and-executable predicate the
        // preflight uses must then flag gvproxy as missing. (Host-independent:
        // `missing_helpers_for` itself keys off the real `~/.ato/tools`, so we
        // exercise its predicate against an explicit helper dir here.)
        let archive = tar_gz_with("usr/bin/podman", podman_stub_script());
        let (artifact, fetcher) =
            artifact_with_helpers(&archive, b"#!/bin/sh\nexit 0\n", b"#!/bin/sh\nexit 0\n");
        let tools = tempfile::tempdir().unwrap();
        let installed = install_with_artifact(&fetcher, &artifact, tools.path()).expect("install");
        let helper_dir = installed.binary_path.parent().unwrap().to_path_buf();
        // Complete runtime → nothing missing.
        assert!(
            artifact
                .helpers
                .iter()
                .all(|h| is_executable_file(&helper_dir.join(h.name)))
        );
        std::fs::remove_file(helper_dir.join("gvproxy")).unwrap();
        let missing: Vec<&str> = artifact
            .helpers
            .iter()
            .filter(|h| !is_executable_file(&helper_dir.join(h.name)))
            .map(|h| h.name)
            .collect();
        assert_eq!(missing, vec!["gvproxy"]);
    }

    #[test]
    fn missing_helpers_for_ignores_non_managed_podman() {
        // A Homebrew/system podman path (not under ~/.ato/tools) is trusted: the
        // preflight returns no missing helpers regardless of what's beside it.
        let missing = missing_helpers_for(Path::new("/opt/homebrew/bin/podman"), "macos", "aarch64");
        assert!(missing.is_empty(), "non-managed podman must not be policed: {missing:?}");
    }

    #[test]
    fn toml_escape_handles_quotes_and_backslashes() {
        assert_eq!(toml_escape("/Users/a b/tools"), "/Users/a b/tools");
        assert_eq!(toml_escape(r#"a"b"#), r#"a\"b"#);
        assert_eq!(toml_escape(r"a\b"), r"a\\b");
    }
}
