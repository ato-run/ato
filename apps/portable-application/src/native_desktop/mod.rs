//! Native desktop realization: acquire the declared artifact, verify its
//! signature policy, apply the mandatory OS policy, launch, and hand the
//! declared logical Ports to the common observation pipeline.
//!
//! The selected Derivation's descriptor was already validated for every
//! declared D. This module is the Adapter side of the boundary: what it does
//! is host-specific and its outcomes are execution evidence, never a Contract
//! verdict. In particular it never sets `fully_satisfied`; the common verifier
//! decides that from the observations it runs.
//!
//! Acquisition is a separate stage from extraction, signature verification and
//! launch, and each one names its own failures. The three artifact sources of
//! the descriptor are handled here:
//!
//! * `bundled` — the blob the verified bundle closure already carries;
//! * `remote` — fetched through the remote module with the declared size
//!   and digest;
//! * `source` — fetched and built through the source-build module with the
//!   declared toolchain, lock file and fixed command.
//!
//! The shared part (artifact extraction and the materialized-tree digest)
//! lives in the artifact module so the Windows Adapter reuses exactly that
//! behaviour. The macOS Seatbelt/signature/process-group implementation lives
//! in the macos module.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ato_formation::native_desktop::NativeDescriptor;
use ato_runtime_attempt::realize::{RealizeFailure, Realized};

use crate::ValidatedNativePort;

// Only the macOS adapter materializes an artifact today. Gating the module
// keeps every other target free of helpers that have no caller there; the
// Windows Adapter is fail-closed before extraction and will use the same code
// once its mandatory policy is enforceable.
#[cfg(target_os = "macos")]
mod app_bundle;
#[cfg(target_os = "macos")]
pub(crate) mod artifact;
#[cfg(target_os = "macos")]
mod dmg;

#[cfg(target_os = "macos")]
pub(crate) mod macos;

/// Streaming acquisition of a digest-pinned remote artifact.
#[cfg(any(target_os = "macos", test))]
pub(crate) mod remote;
/// The isolated, content-addressed build of a source route.
#[cfg(target_os = "macos")]
pub(crate) mod source_build;
/// The declared terminal exchanges of a Terminal surface. Pure framing tests
/// run on every host; native execution remains macOS-only.
#[cfg(any(target_os = "macos", test))]
pub(crate) mod terminal;
/// The managed toolchain a source build needs: policy, layout and installer.
#[cfg(any(target_os = "macos", test))]
pub(crate) mod toolchain;
#[cfg(target_os = "windows")]
pub(crate) mod windows;
/// Safe unpacking of a `zip` remote artifact, from its central directory.
#[cfg(target_os = "macos")]
pub(crate) mod zip;

/// Whether this Run may install a managed toolchain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ToolchainConsent {
    /// The user approved installing under `@@ATO_HOME`.
    Approved,
    /// The user was asked and did not approve.
    #[default]
    Denied,
    /// There is no one to ask (a non-interactive Run).
    Unavailable,
}

/// A named native-adapter refusal. The code is stable and user-facing; the
/// detail names what could not be enforced.
#[derive(Debug, thiserror::Error)]
#[error("{code}: {detail}")]
pub(crate) struct NativeRefusal {
    pub code: &'static str,
    pub detail: String,
}

pub(crate) fn refusal(code: &'static str, detail: impl Into<String>) -> RealizeFailure {
    RealizeFailure::Execution(anyhow::Error::new(NativeRefusal {
        code,
        detail: detail.into(),
    }))
}

/// Turn a materialization error into a named refusal.
///
/// A packed format this Adapter cannot unpack is a different fact from a
/// corrupted artifact, so the code the extractor chose is preserved.
#[cfg(target_os = "macos")]
pub(crate) fn artifact_refusal(error: anyhow::Error) -> NativeRefusal {
    let detail = format!("{error:#}");
    let code = if detail.starts_with("native_format_extractor_unavailable") {
        "native_format_extractor_unavailable"
    } else {
        "native_artifact_invalid"
    };
    NativeRefusal { code, detail }
}

/// Where the bytes to materialize are for this Run.
pub(crate) enum NativeArtifactPayload {
    /// A packed archive: the bundle's own blob, or a verified remote fetch on
    /// disk.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Archive(NativeArchiveSource),
    /// A staged directory a source build produced in this Run. The declared
    /// entry is already laid out inside it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    BuiltTree(PathBuf),
    /// An installed, digest-verified distribution rechecked under its format policy.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    InstalledArchive(PathBuf),
    /// A generated macOS wrapper around an unchanged verified source executable.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    WindowApp { root: PathBuf, entry: String },
}

/// Where one archive's bytes live.
pub(crate) enum NativeArchiveSource {
    /// The bundle closure's blob, already verified by the delivery path.
    /// Other native hosts currently refuse admission before unpacking it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Memory(Vec<u8>),
    /// A file this Run fetched and verified.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    File(PathBuf),
}

/// What the acquisition stage established, for the receipt.
///
/// This is evidence about one Run's materialization: it never enters K, D or
/// the selected route's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeProvenance {
    /// `bundled`, `remote` or `source`.
    pub kind: &'static str,
    /// The digest of the artifact this Run materialized: the bundle blob, the
    /// fetched archive, or the built file.
    pub artifact_ref: String,
    /// The upstream URL the Run fetched, when it fetched one.
    pub fetched_from: Option<String>,
    /// The commit a source build pinned.
    pub source_commit: Option<String>,
    /// `hit` or `miss` for a source build.
    pub build_cache: Option<String>,
    /// The toolchain a source build used.
    pub toolchain: Option<String>,
    /// The fixed command a source build ran.
    pub build_argv: Option<String>,
}

#[cfg(target_os = "macos")]
impl NativeProvenance {
    /// What the acquisition stage did, in one line for the receipt.
    pub fn materialization(&self) -> String {
        match self.kind {
            "remote" => format!(
                "digest-pinned upstream artifact verified from cache or downloaded from {}; Ato did not \
                 redistribute the artifact",
                self.fetched_from.as_deref().unwrap_or("(unnamed)")
            ),
            "source" => format!(
                "source artifact from commit {} with {} ({}), build cache {}",
                self.source_commit.as_deref().unwrap_or("(unknown)"),
                self.toolchain.as_deref().unwrap_or("(unknown toolchain)"),
                self.build_argv.as_deref().unwrap_or("(unknown command)"),
                self.build_cache.as_deref().unwrap_or("miss")
            ),
            _ => "the artifact is a blob in the verified bundle closure".to_owned(),
        }
    }
}

/// The artifact a Run acquired and how, ready for the launch stage.
pub(crate) struct AcquiredArtifact {
    pub payload: NativeArtifactPayload,
    pub provenance: NativeProvenance,
}

/// Acquire the selected route's artifact.
///
/// The three sources are handled here and nowhere else, so the launch stage
/// only materializes what this returns. A bundled artifact is the bytes the
/// verified closure already carried; a remote reference is fetched with its
/// declared size and digest; a source build provisions its pinned toolchain
/// (only with approval), fetches the pinned tarball and builds it in the
/// sandbox.
///
/// A platform without the native Adapter refuses the non-bundled sources
/// explicitly rather than pretending to acquire them.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn acquire(
    descriptor: &NativeDescriptor,
    bundled: Option<Vec<u8>>,
    runtime_root: &Path,
    home: &Path,
    consent: ToolchainConsent,
) -> Result<AcquiredArtifact, RealizeFailure> {
    use ato_formation::native_desktop::NativeArtifactSource;

    match &descriptor.source {
        NativeArtifactSource::Bundled { artifact } => {
            let bytes = bundled.ok_or_else(|| {
                refusal(
                    "native_artifact_invalid",
                    "the verified bundle closure did not carry the declared artifact blob",
                )
            })?;
            Ok(AcquiredArtifact {
                payload: NativeArtifactPayload::Archive(NativeArchiveSource::Memory(bytes)),
                provenance: NativeProvenance {
                    kind: "bundled",
                    artifact_ref: artifact.clone(),
                    fetched_from: None,
                    source_commit: None,
                    build_cache: None,
                    toolchain: None,
                    build_argv: None,
                },
            })
        }
        NativeArtifactSource::Remote(remote) => {
            let transport = remote::ReqwestFetcher::default();
            let fetched = remote::fetch_cached(
                &remote.url,
                &remote.sha256,
                remote.bytes,
                Some(remote.bytes),
                &home.join("cache/native-artifacts"),
                &transport,
            )
            .map_err(|error| {
                let code = match &error {
                    remote::FetchFailure::NotFound(_) => "native_fetch_not_found",
                    remote::FetchFailure::DigestMismatch { .. } => "native_fetch_digest_mismatch",
                    remote::FetchFailure::SizeMismatch { .. } => "native_fetch_size_mismatch",
                    remote::FetchFailure::UrlInvalid(_) => "native_fetch_url_invalid",
                    remote::FetchFailure::Unavailable(_) => "native_fetch_unavailable",
                };
                refusal(code, error.to_string())
            })?;
            Ok(AcquiredArtifact {
                payload: if descriptor.local_window {
                    NativeArtifactPayload::InstalledArchive(
                        artifact::cached_archive_tree(
                            &fetched.path,
                            descriptor.format,
                            &fetched.sha256,
                            home,
                        )
                        .map_err(|error| {
                            refusal(
                                "native_install_materialization_failed",
                                format!("{error:#}"),
                            )
                        })?,
                    )
                } else {
                    NativeArtifactPayload::Archive(NativeArchiveSource::File(fetched.path))
                },
                provenance: NativeProvenance {
                    kind: "remote",
                    artifact_ref: fetched.sha256,
                    fetched_from: Some(remote.url.clone()),
                    source_commit: None,
                    build_cache: None,
                    toolchain: None,
                    build_argv: None,
                },
            })
        }
        NativeArtifactSource::Source(build) => {
            let target = descriptor.arch.rust_target(descriptor.os).ok_or_else(|| {
                refusal(
                    "native_toolchain_platform_unsupported",
                    "the declared os/arch has no Rust target triple",
                )
            })?;
            let transport = remote::ReqwestFetcher::default();
            let toolchain_fetcher = toolchain::ArtifactToolchainFetcher { inner: &transport };
            let toolchain = toolchain::provide(
                home,
                &build.toolchain,
                Some(target),
                consent,
                &toolchain::RustupProvisioner,
                &toolchain_fetcher,
            )
            .map_err(|error| {
                let code = match &error {
                    toolchain::ToolchainFailure::ConsentRequired { .. } => {
                        "native_toolchain_consent_required"
                    }
                    toolchain::ToolchainFailure::ConsentUnavailable { .. } => {
                        "native_toolchain_consent_unavailable"
                    }
                    toolchain::ToolchainFailure::PlatformUnsupported => {
                        "native_toolchain_platform_unsupported"
                    }
                    toolchain::ToolchainFailure::Install(_) => "native_toolchain_install_failed",
                    toolchain::ToolchainFailure::Incomplete(_) => "native_toolchain_incomplete",
                };
                refusal(code, error.to_string())
            })?;
            let source_archive = remote::fetch_cached(
                &build.url,
                &build.sha256,
                remote::MAX_NATIVE_SOURCE_BYTES,
                None,
                &home.join("cache/native-artifacts"),
                &transport,
            )
            .map_err(|error| {
                let code = match &error {
                    remote::FetchFailure::NotFound(_) => "native_fetch_not_found",
                    remote::FetchFailure::DigestMismatch { .. } => "native_fetch_digest_mismatch",
                    remote::FetchFailure::SizeMismatch { .. } => "native_fetch_size_mismatch",
                    remote::FetchFailure::UrlInvalid(_) => "native_fetch_url_invalid",
                    remote::FetchFailure::Unavailable(_) => "native_fetch_unavailable",
                };
                refusal(code, error.to_string())
            })?
            .path;
            let built = source_build::build(
                build,
                &descriptor.entry,
                home,
                runtime_root,
                target,
                &toolchain,
                &source_archive,
                &source_build::SandboxedCargo,
            )
            .map_err(|error| {
                let code = match &error {
                    source_build::BuildFailure::EntryInvalid(_) => "native_build_entry_invalid",
                    source_build::BuildFailure::ArchiveInvalid(_) => {
                        "native_build_source_archive_invalid"
                    }
                    source_build::BuildFailure::CommitMismatch { .. } => {
                        "native_build_source_commit_mismatch"
                    }
                    source_build::BuildFailure::CargoLockMismatch { .. } => {
                        "native_build_cargo_lock_mismatch"
                    }
                    source_build::BuildFailure::OutputMissing(_) => "native_build_output_missing",
                    source_build::BuildFailure::SandboxUnavailable(_) => {
                        "native_build_sandbox_unavailable"
                    }
                    source_build::BuildFailure::Failed(_) => "native_build_failed",
                    source_build::BuildFailure::Io(_) => "native_build_io",
                };
                refusal(code, error.to_string())
            })?;
            eprintln!(
                "Native source build cache: {} ({})",
                built.cache.as_str(),
                built.cache_key
            );
            Ok(AcquiredArtifact {
                payload: if descriptor.local_window {
                    let (root, entry) =
                        app_bundle::materialize(&built.entry, &built.output_digest, home).map_err(
                            |error| {
                                refusal(
                                    "native_install_materialization_failed",
                                    format!("{error:#}"),
                                )
                            },
                        )?;
                    NativeArtifactPayload::WindowApp { root, entry }
                } else {
                    NativeArtifactPayload::BuiltTree(built.root)
                },
                provenance: NativeProvenance {
                    kind: "source",
                    artifact_ref: built.output_digest,
                    fetched_from: Some(build.url.clone()),
                    source_commit: Some(build.commit.clone()),
                    build_cache: Some(built.cache.as_str().to_owned()),
                    toolchain: Some(built.toolchain),
                    build_argv: Some(built.build_argv),
                },
            })
        }
    }
}

/// Acquire on a platform without the native Adapter: only a bundled artifact
/// exists, and the launch stage refuses it anyway.
#[cfg(not(target_os = "macos"))]
pub(crate) fn acquire(
    descriptor: &NativeDescriptor,
    bundled: Option<Vec<u8>>,
    _runtime_root: &Path,
    _home: &Path,
    _consent: ToolchainConsent,
) -> Result<AcquiredArtifact, RealizeFailure> {
    use ato_formation::native_desktop::NativeArtifactSource;

    match &descriptor.source {
        NativeArtifactSource::Bundled { artifact } => {
            let bytes = bundled.ok_or_else(|| {
                refusal(
                    "native_artifact_invalid",
                    "the verified bundle closure did not carry the declared artifact blob",
                )
            })?;
            Ok(AcquiredArtifact {
                payload: NativeArtifactPayload::Archive(NativeArchiveSource::Memory(bytes)),
                provenance: NativeProvenance {
                    kind: "bundled",
                    artifact_ref: artifact.clone(),
                    fetched_from: None,
                    source_commit: None,
                    build_cache: None,
                    toolchain: None,
                    build_argv: None,
                },
            })
        }
        NativeArtifactSource::Remote(_) | NativeArtifactSource::Source(_) => Err(refusal(
            "native_adapter_unavailable",
            "a remote or source-built native artifact needs the native Adapter, which this build \
             does not have for this platform",
        )),
    }
}

/// Everything the platform Adapter needs to start one selected native D.
///
/// Every target constructs this struct; only the platform Adapter that admits
/// the route reads all of its fields.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct NativeStart<'a> {
    /// The artifact bytes this Run acquired for the selected route.
    pub payload: NativeArtifactPayload,
    /// What the acquisition stage established, recorded as evidence.
    pub provenance: NativeProvenance,
    pub descriptor: &'a NativeDescriptor,
    pub ports: &'a [ValidatedNativePort],
    /// The Terminal surface this route drives, when it declares one. Such a
    /// route has no ports: the Adapter owns the PTY.
    pub terminal: Option<&'a crate::ValidatedNativeTerminal>,
    /// The materialized read-only workspace tree.
    pub workspace: PathBuf,
    /// This Run's runtime scratch root.
    pub runtime_root: &'a Path,
    /// `(state slot id, host path)` for every declared slot. The host path is
    /// the Run-scoped writable directory; the runtime is the only writer.
    pub state_paths: Vec<(String, PathBuf)>,
    /// Binding values and any other runtime inputs to place in the child env.
    pub environment: &'a BTreeMap<String, String>,
    /// The explicit local override decision, when the caller made one.
    pub local_override: bool,
    /// The route declares TCP egress in `requirements.network`.
    pub declares_egress: bool,
}

/// Start the selected native Derivation on this host.
#[cfg(target_os = "macos")]
pub(crate) fn start(request: NativeStart<'_>) -> Result<Realized, RealizeFailure> {
    macos::start(request)
}

#[cfg(target_os = "windows")]
pub(crate) fn start(request: NativeStart<'_>) -> Result<Realized, RealizeFailure> {
    windows::start(request)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn start(_request: NativeStart<'_>) -> Result<Realized, RealizeFailure> {
    Err(refusal(
        "native_adapter_unavailable",
        "this build has no native desktop Adapter for this platform",
    ))
}

/// Resolve the declared entry inside the extracted artifact.
///
/// The descriptor validator already refused absolute and traversing values;
/// this proves the file is really there, is a regular file and is executable,
/// and that the resolved target did not leave the root through a filesystem
/// link.
#[cfg(target_os = "macos")]
pub(crate) fn resolve_entry(artifact_root: &Path, entry: &str) -> Result<PathBuf, NativeRefusal> {
    let joined = artifact_root.join(entry);
    let canonical = joined.canonicalize().map_err(|error| NativeRefusal {
        code: "native_entry_missing",
        detail: format!("the declared entry `{entry}` is not in the artifact: {error}"),
    })?;
    let root = artifact_root
        .canonicalize()
        .map_err(|error| NativeRefusal {
            code: "native_artifact_invalid",
            detail: format!("the artifact root is not readable: {error}"),
        })?;
    if !canonical.starts_with(&root) {
        return Err(NativeRefusal {
            code: "native_entry_escapes_artifact",
            detail: format!("the declared entry `{entry}` resolves outside the artifact root"),
        });
    }
    let metadata = std::fs::metadata(&canonical).map_err(|error| NativeRefusal {
        code: "native_entry_missing",
        detail: format!("the declared entry `{entry}` is not readable: {error}"),
    })?;
    if !metadata.is_file() {
        return Err(NativeRefusal {
            code: "native_entry_not_a_file",
            detail: format!("the declared entry `{entry}` is not a regular file"),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(NativeRefusal {
                code: "native_entry_not_executable",
                detail: format!("the declared entry `{entry}` is not executable after extraction"),
            });
        }
    }
    Ok(canonical)
}

#[cfg(target_os = "macos")]
mod window;
