//! Native desktop realization: materialize the pinned artifact, verify its
//! signature policy, apply the mandatory OS policy, launch, and hand the
//! declared logical Ports to the common observation pipeline.
//!
//! The selected Derivation's descriptor was already validated for every
//! declared D. This module is the Adapter side of the boundary: what it does
//! is host-specific and its outcomes are execution evidence, never a Contract
//! verdict. In particular it never sets `fully_satisfied`; the common verifier
//! decides that from the observations it runs.
//!
//! The shared part (artifact extraction and the materialized-tree digest)
//! lives here so the Windows Adapter reuses exactly that behaviour. The macOS
//! Seatbelt/signature/process-group implementation lives in [`macos`].

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
pub(crate) mod artifact;

#[cfg(target_os = "macos")]
pub(crate) mod macos;

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

/// Everything the platform Adapter needs to start one selected native D.
///
/// Every target constructs this struct; only the platform Adapter that admits
/// the route reads all of its fields.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct NativeStart<'a> {
    /// The exact artifact bytes the bundle's verified closure carries.
    pub artifact_bytes: Vec<u8>,
    pub descriptor: &'a NativeDescriptor,
    pub ports: &'a [ValidatedNativePort],
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

#[cfg(not(target_os = "macos"))]
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
