//! The `SnapshotBackend` seam.
//!
//! Decision (plan §4): snapshot/restore is a **separate trait**, not grafted
//! onto `capsule`'s `RuntimeHandle`. `RuntimeHandle` is a minimal observe/kill
//! handle over a PID/container; build-time capture + run-time rehydrate + host
//! device/network setup is a different lifecycle. Restore instead *produces* a
//! [`RestoredSession`] carrying everything a later adapter (M6) needs to expose
//! it as a `RuntimeHandle` (metrics/kill/teardown reuse the session machinery)
//! — that adapter also adds the `RuntimeMetadata::MicroVm` variant, so it is
//! deliberately out of this seam.
//!
//! The trait is synchronous: the Fake backend and the CapsuleFS round-trip it
//! drives need no async, so the seam (and its A1 E2E test) stays simple. A real
//! VMM backend that needs async does its own runtime internally.

use std::path::PathBuf;

use capsule::foundation::install_lifecycle::{RunnerClassId, RunnerClassMismatch};
use capsulefs::CasStore;

use crate::manifest::{
    NoSecretProof, ReadyStateManifest, RestoreContract, SanitizerContract,
};

/// What a backend can do on this host, from a probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilities {
    /// Backend id (`"firecracker"`, `"fake"`).
    pub backend_id: String,
    /// Whether the backend can build/restore on this host right now.
    pub available: bool,
    /// Why it is unavailable (e.g. "/dev/kvm not present"), when `!available`.
    pub reason: Option<String>,
    /// Host architecture as the backend sees it.
    pub arch: String,
    /// Whether `/dev/kvm` is present (microVM backends need it).
    pub kvm_present: bool,
    /// VMM version, when known.
    pub vmm_version: Option<String>,
}

/// Raw layer bytes handed to a build. The caller assembles these from the
/// frozen build outputs; the backend chunks and content-addresses them.
#[derive(Debug, Clone, Default)]
pub struct BuildLayers {
    pub rootfs: Vec<u8>,
    pub runtime: Option<Vec<u8>>,
    pub dependency: Option<Vec<u8>>,
    pub app: Option<Vec<u8>>,
    /// VMM VM state captured after boot-to-readiness.
    pub vmstate: Vec<u8>,
    /// Guest memory image captured after boot-to-readiness.
    pub memory: Vec<u8>,
}

/// Inputs to [`SnapshotBackend::build_ready_state`].
pub struct BuildReadyStateInput<'a> {
    /// CapsuleFS store the layers are written into.
    pub store: &'a CasStore,
    /// `blake3:<hex>` of the originating capsule manifest.
    pub capsule_manifest_hash: String,
    /// Restore-compatibility class this snapshot is built for (plan §5).
    pub runner_class: Option<RunnerClassId>,
    /// The raw layer bytes (captured with NO secret / user data present).
    pub layers: BuildLayers,
    /// How a restored session reaches readiness.
    pub restore_contract: RestoreContract,
    /// Post-resume sanitizer steps.
    pub sanitizer_contract: SanitizerContract,
    /// Declared secret names to scan the sealed layers for (the no-secret gate).
    pub declared_secret_markers: Vec<String>,
}

/// Result of a successful build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReadyStateReceipt {
    /// The sealed artifact manifest.
    pub manifest: ReadyStateManifest,
    /// Total bytes written across all layers.
    pub sealed_bytes: u64,
    /// The no-secret scan proof (also embedded in the manifest).
    pub no_secret_proof: NoSecretProof,
}

/// Inputs to [`SnapshotBackend::restore`].
pub struct RestoreReadyStateInput<'a> {
    /// CapsuleFS store the layers are read from.
    pub store: &'a CasStore,
    /// The sealed artifact.
    pub manifest: ReadyStateManifest,
    /// Ephemeral writable overlay root for this session (destroyed on stop).
    pub overlay_root: PathBuf,
    /// The candidate restore host's runner class. If both this and the
    /// manifest's class are present they MUST match (fail-closed).
    pub host_runner_class: Option<RunnerClassId>,
}

/// A restored, running (or restorable) session. The data a later adapter wraps
/// into a `capsule` `RuntimeHandle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredSession {
    /// Stable session id.
    pub session_id: String,
    /// Backend that restored it.
    pub backend_id: String,
    /// Host port the app is reachable on, once exposed.
    pub guest_port: Option<u16>,
    /// Writable overlay root for this session.
    pub overlay_root: PathBuf,
    /// Bytes rehydrated from the layers.
    pub restored_bytes: u64,
}

impl RestoredSession {
    /// The id a `RuntimeHandle` adapter would report.
    pub fn id(&self) -> &str {
        &self.session_id
    }
}

/// Result of a successful restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReceipt {
    /// The restored session.
    pub session: RestoredSession,
    /// Id of the artifact that was restored.
    pub ready_state_manifest_id: String,
}

/// Result of a successful teardown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeardownReceipt {
    /// Id of the session that was torn down.
    pub session_id: String,
    /// Whether the disposable overlay was removed.
    pub overlay_removed: bool,
}

/// Inspection of a sealed artifact without restoring it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInspection {
    /// Artifact id.
    pub manifest_id: String,
    /// Backend kind.
    pub backend_kind: String,
    /// Present layer names.
    pub layers: Vec<String>,
    /// Total layer bytes.
    pub total_bytes: u64,
    /// Whether every referenced chunk is present in the store.
    pub all_chunks_present: bool,
}

/// Backend errors.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// The backend cannot operate on this host (e.g. no `/dev/kvm`).
    #[error("snapshot backend '{backend}' unsupported on this host: {reason}")]
    Unsupported { backend: String, reason: String },

    /// Restore was attempted on a host of the wrong runner class (fail-closed).
    #[error(transparent)]
    RunnerClassMismatch(#[from] RunnerClassMismatch),

    /// The no-secret gate found a secret in the sealed layers (fail-closed).
    #[error("no-secret gate failed: {0:?}")]
    SecretFoundInSnapshot(Vec<String>),

    /// A CapsuleFS operation failed.
    #[error(transparent)]
    CapsuleFs(#[from] capsulefs::CapsuleFsError),

    /// Underlying I/O failure.
    #[error("snapshot io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Build + restore of warm Ready-State capsule state.
///
/// A Ready-State-capable runner advertises a `SnapshotBackend` plus its
/// `runner_class_id`; placement matches a capsule's `runner_class_id` to such a
/// runner. When no backend is available, `ato run` uses the legacy cold path —
/// this trait is purely additive and a cold run never calls it.
pub trait SnapshotBackend: Send + Sync {
    /// Stable backend id, e.g. `"firecracker"`.
    fn id(&self) -> &str;

    /// What this backend can do on this host right now.
    fn probe(&self) -> BackendCapabilities;

    /// Boot-capture-seal: chunk the layers into CapsuleFS, scan for secrets,
    /// and produce a [`ReadyStateManifest`] + receipt. The caller guarantees the
    /// layers were captured with no secret / user data present; this method
    /// enforces it with the no-secret gate and fails closed on any finding.
    fn build_ready_state(
        &self,
        input: BuildReadyStateInput<'_>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError>;

    /// Inspect a sealed artifact without restoring it.
    fn inspect(
        &self,
        store: &CasStore,
        manifest: &ReadyStateManifest,
    ) -> Result<SnapshotInspection, SnapshotError>;

    /// Rehydrate a session from a sealed artifact: verify the runner class
    /// (fail-closed on mismatch), read the layers back, create a disposable
    /// overlay, and return a [`RestoredSession`].
    fn restore(
        &self,
        input: RestoreReadyStateInput<'_>,
    ) -> Result<RestoreReceipt, SnapshotError>;

    /// Tear down a restored session: kill the VM and destroy its overlay.
    fn stop(&self, session: RestoredSession) -> Result<TeardownReceipt, SnapshotError>;
}
