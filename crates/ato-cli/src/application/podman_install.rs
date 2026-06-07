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

use sha2::{Digest, Sha256};

/// Pinned Podman version Ato installs when no Homebrew/system copy exists.
///
/// Bump this together with the per-OS/arch entries in [`pinned_artifact`].
pub(crate) const PINNED_PODMAN_VERSION: &str = "5.2.3";

/// File name of the provenance manifest written next to an Ato-managed install.
const PROVENANCE_FILE: &str = "ato-podman-provenance.json";

/// A pinned, digest-verified Podman release artifact for one OS/arch.
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
}

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
        }),
        ("macos", "x86_64") => Some(PinnedArtifact {
            version: PINNED_PODMAN_VERSION,
            url: "https://github.com/containers/podman/releases/download/v5.2.3/podman-remote-release-darwin_amd64.zip",
            // From the official Podman v5.2.3 `shasums` (amd64).
            sha256: "6a7ef2eb934e7b5f002bcc662314fd43013f9452edb2be0889d23da8e201f514",
            format: ArtifactFormat::Zip,
            binary_rel_path: "usr/bin/podman",
            strip_prefix: "podman-5.2.3",
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

    let bytes = fetcher
        .fetch(artifact.url)
        .map_err(|message| PodmanInstallError::Fetch {
            url: artifact.url.to_string(),
            message,
        })?;

    // Verify BEFORE touching disk: an unverified archive is never extracted.
    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(artifact.sha256) {
        return Err(PodmanInstallError::DigestMismatch {
            url: artifact.url.to_string(),
            expected: artifact.sha256.to_string(),
            actual,
        });
    }

    let install_dir = tools_dir.join(format!("podman-{}", artifact.version));
    // Clean any prior partial install so extraction is deterministic.
    if install_dir.exists() {
        std::fs::remove_dir_all(&install_dir).map_err(|e| PodmanInstallError::Extract {
            message: format!("could not clear existing install dir: {e}"),
        })?;
    }
    std::fs::create_dir_all(&install_dir).map_err(|e| PodmanInstallError::Extract {
        message: e.to_string(),
    })?;

    extract_archive(&bytes, artifact.format, artifact.strip_prefix, &install_dir)
        .map_err(|message| PodmanInstallError::Extract { message })?;

    let binary_path = install_dir.join(artifact.binary_rel_path);
    if !binary_path.is_file() {
        return Err(PodmanInstallError::BinaryMissing {
            expected: binary_path,
        });
    }
    ensure_executable(&binary_path).map_err(|message| PodmanInstallError::Extract { message })?;

    let provenance = PodmanProvenance {
        version: artifact.version.to_string(),
        sha256: artifact.sha256.to_string(),
        source_url: artifact.url.to_string(),
        binary_path: binary_path.to_string_lossy().to_string(),
    };
    write_provenance(&install_dir, &provenance)?;

    Ok(InstalledPodman {
        binary_path,
        provenance,
    })
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
        }
    }

    /// Run [`install_ato_managed_podman`]'s body against an explicit artifact,
    /// bypassing the OS/arch table so tests are host-independent.
    fn install_with_artifact<F: PodmanArtifactFetcher>(
        fetcher: &F,
        artifact: &PinnedArtifact,
        tools_dir: &Path,
    ) -> Result<InstalledPodman, PodmanInstallError> {
        let bytes = fetcher
            .fetch(artifact.url)
            .map_err(|message| PodmanInstallError::Fetch {
                url: artifact.url.to_string(),
                message,
            })?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(artifact.sha256) {
            return Err(PodmanInstallError::DigestMismatch {
                url: artifact.url.to_string(),
                expected: artifact.sha256.to_string(),
                actual,
            });
        }
        let install_dir = tools_dir.join(format!("podman-{}", artifact.version));
        std::fs::create_dir_all(&install_dir).unwrap();
        extract_archive(&bytes, artifact.format, artifact.strip_prefix, &install_dir)
            .map_err(|message| PodmanInstallError::Extract { message })?;
        let binary_path = install_dir.join(artifact.binary_rel_path);
        if !binary_path.is_file() {
            return Err(PodmanInstallError::BinaryMissing {
                expected: binary_path,
            });
        }
        ensure_executable(&binary_path)
            .map_err(|message| PodmanInstallError::Extract { message })?;
        let provenance = PodmanProvenance {
            version: artifact.version.to_string(),
            sha256: artifact.sha256.to_string(),
            source_url: artifact.url.to_string(),
            binary_path: binary_path.to_string_lossy().to_string(),
        };
        write_provenance(&install_dir, &provenance)?;
        Ok(InstalledPodman {
            binary_path,
            provenance,
        })
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
        let bytes = tar_gz_with("usr/bin/podman", b"#!/bin/sh\necho podman\n");
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
        let bytes = tar_gz_with("usr/bin/podman", b"podman-binary");
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

    #[test]
    fn pinned_macos_targets_have_urls_and_digests() {
        for arch in ["aarch64", "x86_64"] {
            let a = pinned_artifact("macos", arch).expect("macos artifact pinned");
            assert_eq!(a.version, PINNED_PODMAN_VERSION);
            assert!(a.url.starts_with("https://"));
            assert_eq!(a.sha256.len(), 64, "digest must be a 64-hex SHA256");
        }
    }
}
