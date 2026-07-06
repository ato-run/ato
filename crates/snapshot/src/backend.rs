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
// The single shared GpuMode lives in capsule (snapshot depends on capsule), so the
// placement contract and the manifest-level GPU judgment agree on one type.
pub use capsule::foundation::types::ready_state::GpuMode;
use capsulefs::CasStore;
use serde::{Deserialize, Serialize};

use crate::manifest::{
    NoSecretProof, ReadyStateManifest, RestoreContract, SanitizerContract,
};

/// What kind of state a backend captures (plan §4 / requirements §0.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotKind {
    /// No snapshot capability (default).
    #[default]
    None,
    /// Process-level checkpoint (e.g. CRIU).
    Process,
    /// MicroVM full memory + device snapshot (Firecracker). Token `microvm`
    /// (matches the requirements §0.5 contract token, not snake_case's
    /// `micro_vm`).
    #[serde(rename = "microvm")]
    MicroVm,
    /// Full-VM snapshot (QEMU/Cloud Hypervisor).
    FullVm,
}

/// How a backend exposes the filesystem to the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemModel {
    /// Block device (virtio-blk) — read-only base + overlay (default).
    #[default]
    Block,
    /// virtio-fs shared filesystem (filesystem-heavy / live mounts).
    Virtiofs,
    /// Host bind mount.
    Bind,
    /// Explicit copy-in / copy-out transfer only.
    CopyInOut,
}

/// Device richness the backend supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceProfile {
    /// Minimal microVM device set: virtio-blk + virtio-net + vsock (default).
    #[default]
    Minimal,
    /// Adds virtio-fs.
    Virtiofs,
    /// VFIO passthrough capable (PCI).
    Vfio,
    /// GPU provided out-of-band as an external capability.
    GpuExternal,
}

/// Isolation boundary the backend runs the workload in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationBoundary {
    /// Host process (default).
    #[default]
    Process,
    /// Sandboxed process (Landlock/seccomp/bwrap).
    Sandbox,
    /// MicroVM. Token `microvm` (see [`SnapshotKind::MicroVm`]).
    #[serde(rename = "microvm")]
    MicroVm,
    /// Full VM.
    Vm,
}

/// What a backend can do on this host, from a probe.
///
/// This is the **placement contract** (requirements §0.5): a capsule declares
/// [`BackendRequirements`](crate::placement::BackendRequirements) and placement
/// selects a backend whose capabilities satisfy them — capsules never name a
/// backend. It is a transient probe result (never serialized), so it carries no
/// serde derive. The first six fields are the original probe; the nine that
/// follow are the additive contract facets.
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
    /// What kind of state this backend captures.
    pub snapshot_kind: SnapshotKind,
    /// Whether it can snapshot guest memory.
    pub memory_snapshot: bool,
    /// Filesystem model it exposes.
    pub filesystem_model: FilesystemModel,
    /// Device richness it supports.
    pub device_profile: DeviceProfile,
    /// How it handles GPU (never `Passthrough` for a snapshot — see
    /// [`ensure_gpu_not_in_snapshot`]).
    pub gpu_mode: GpuMode,
    /// Whether it runs OCI/container-native workloads.
    pub oci_native: bool,
    /// Isolation boundary it provides.
    pub isolation_boundary: IsolationBoundary,
    /// Whether it can seal layers before any user binding — the hard
    /// Ready-State safety invariant. A backend that is `false` here is never
    /// selected for a Ready-State capsule (see
    /// [`ready_state_safe`](crate::placement::ready_state_safe)).
    pub supports_seal_before_bind: bool,
    /// Whether it gives each session a disposable writable overlay.
    pub supports_disposable_overlay: bool,
    /// Whether this backend can drive a Firecracker `Uffd` snapshot `mem_backend`
    /// (lazy page faulting via a page-server) on this host. **U0 scope** is
    /// x86_64 + `/dev/kvm` + Firecracker ≥ the version whose swagger declares the
    /// `Uffd` backend type + kernel `userfaultfd`; everything else is `false`.
    /// This is a truthful capability probe only — no restore path uses it yet
    /// (see `docs/ready-state/uffd-mem-backend.md`).
    pub supports_uffd_mem_backend: bool,
    /// Why UFFD is unsupported, when `!supports_uffd_mem_backend` (introspectable
    /// reason, e.g. `"aarch64 not in U0 scope (x86_64 only)"`, `"firecracker
    /// 0.25.2 < 1.0.0"`, `"userfaultfd disabled on host"`).
    pub uffd_reason: Option<String>,
    /// L2 (#912): whether this backend/host can run the BindingLease preview flow
    /// (vsock guest-agent + tmpfs delivery + stop-scrub + no-secret scan). The run gate
    /// fails a binding-required preview closed when `supports_binding_lease` is false.
    pub binding: BindingCapabilities,
}

/// L2 (#912): the placement capabilities a binding-required Ready-State restore needs.
/// All `false` by default (the safe baseline); a backend's probe fills the ones it
/// truthfully supports on this host. `supports_binding_lease` is the gate boolean.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingCapabilities {
    /// The backend is Firecracker (the only microVM backend with the binding flow).
    pub supports_firecracker: bool,
    /// The host has an AF_VSOCK transport (`/dev/vhost-vsock`).
    pub supports_vsock: bool,
    /// A guest-agent can be packaged into the rootfs + reached over vsock.
    pub supports_guest_agent: bool,
    /// The full binding-lease delivery flow is available (vsock + guest-agent, x86_64).
    /// This is the gate the run path checks.
    pub supports_binding_lease: bool,
    /// `ato stop` can scrub guest bindings over vsock before teardown.
    pub supports_stop_scrub: bool,
    /// The reusable no-secret scanner is available as a release gate (L4).
    pub supports_no_secret_scan: bool,
}

impl BindingCapabilities {
    /// A human-readable reason the binding-lease flow is unavailable, or `None` when
    /// `supports_binding_lease` is true.
    pub fn unavailable_reason(&self) -> Option<String> {
        if self.supports_binding_lease {
            return None;
        }
        let mut missing = Vec::new();
        if !self.supports_firecracker {
            missing.push("firecracker backend");
        }
        if !self.supports_vsock {
            missing.push("host vsock (/dev/vhost-vsock)");
        }
        if !self.supports_guest_agent {
            missing.push("guest-agent");
        }
        Some(format!("binding-lease unsupported on this host: missing {}", missing.join(", ")))
    }
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

/// v1.2 PR 3d: build-side inputs for a **supervisor** capsule (a `[secrets.*]`
/// rootfs whose init is the guest-agent, built by `derive_supervisor_build_spec`).
/// Present ⇒ the backend must drive the supervisor build protocol before sealing:
/// deliver a PLACEHOLDER lease per binding over vsock (the agent starts the
/// workload only at bound-ready, so health is unreachable without this), wait for
/// health, then `StopWorkload` + `Revoke` every placeholder so the snapshot is
/// taken with the workload down and the tmpfs bindings scrubbed. Names only —
/// the placeholder values are generated inside the backend and never stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SupervisorBindings {
    /// The binding names the guest rootfs requires (its agent argv), verbatim.
    pub binding_names: Vec<String>,
    /// v1.6 (ato#983) Slice 2: this build's durable state volumes (from
    /// `ServiceBuildSpec.volumes`, flattened across services), if any. Each is
    /// attached as a writable, non-root Firecracker drive — see
    /// [`crate::state_volume`]. Empty ⇒ no behavior change (no drives beyond
    /// rootfs are attached, identical to before this slice).
    pub state_volumes: Vec<crate::state_volume::DurableVolumeSpec>,
    /// v1.6 (ato#983) Slice 2: the stable identity a durable volume's backing
    /// file/lock path is keyed on (owner + capsule instance, NOT any
    /// session/run/execution id — see `state_volume::volume_path`). Required
    /// (fail-closed) whenever `state_volumes` is non-empty; a path could
    /// otherwise not be recomputed identically at the next restore.
    pub state_owner_scope: Option<String>,
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
    /// The declared Ato Execution Identity to stamp into the sealed manifest
    /// (`manifest.execution_id`), verbatim. Compute it with
    /// `capsule::engine::execution_graph::ReadyStateDeclaredEnvelope::declared_execution_id`
    /// from declared, host-independent facts only — NEVER from a job id, artifact hash,
    /// timestamp, or builder-host state (that would be a build-job identity, not an
    /// execution identity). `None` ⇒ the sealed manifest carries no execution id and a
    /// registry builder fails closed at `artifact_metadata`.
    pub execution_id: Option<String>,
    /// v1.2 PR 3d: `Some` ⇒ this rootfs is a supervisor build (agent-as-init) and
    /// the backend drives placeholder-deliver → health → StopWorkload → Revoke
    /// before the snapshot. `None` = the unchanged no-binding path.
    pub supervisor: Option<SupervisorBindings>,
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
    /// The candidate restore host's runner class. Fail-closed semantics: when
    /// the manifest pins a class, this MUST be present **and** equal — an
    /// unknown host class (`None`) is rejected, not waved through. When the
    /// manifest pins no class (e.g. host detection has not landed yet), the gate
    /// is a no-op.
    pub host_runner_class: Option<RunnerClassId>,
    /// U11 (#878): opt-in UFFD **local** preview. When `true`, a backend that
    /// supports it restores memory via the UFFD local-CAS demand path instead of
    /// the eager File rehydrate. Default `false` = the unchanged File path. The
    /// caller only sets this on a supported host for a no-binding capsule (else
    /// the backend fails closed).
    pub uffd_preview: bool,
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
    /// PID of the live VMM process serving this session, when the backend spawns
    /// one (Firecracker). `None` for backends with no serving process (Fake). The
    /// long-lived-serving run gate stamps this into the on-disk session record so
    /// a later `ato stop` (fresh backend, empty in-memory registry) can reap it.
    pub vmm_pid: Option<i32>,
    /// Phase 8a-HW (#912): the Firecracker vsock host UDS for this restored session,
    /// when the vsock device is enabled (`ATO_FC_VSOCK`). The host reaches the guest-
    /// agent by connecting here (`CONNECT <port>`). `None` when vsock is off (the
    /// default) — no binding delivery path.
    pub vsock_uds: Option<PathBuf>,
    /// Track E (#912): the HOST-REACHABLE `ip:port` where the restored workload
    /// accepts connections — backend-authoritative, because only the backend knows
    /// its exposure topology (Firecracker serves on the tap guest IP, e.g.
    /// `172.16.0.2:8080`, NOT host loopback). A proxy fronting this session must
    /// dial exactly this address. `None` when the backend has no live listener
    /// (Fake) — an honest caller then exposes nothing rather than guessing.
    pub workload_addr: Option<String>,
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

    /// The snapshot pins a runner class but the restore host's class is
    /// unknown. Unknown is **not** compatible — a snapshot is only restorable
    /// on a host proven to match its build class, so this fails closed.
    #[error(
        "restore host runner class is unknown but the snapshot requires '{expected}'; \
         refusing to restore (unknown != compatible)"
    )]
    MissingHostRunnerClass { expected: RunnerClassId },

    /// The no-secret gate found a declared secret marker in the sealed layers
    /// (fail-closed). Carries the verbatim markers (the caller already holds
    /// these values).
    #[error("no-secret gate failed: {0:?}")]
    SecretFoundInSnapshot(Vec<String>),

    /// The heuristic no-secret scanner flagged likely secrets in the sealed
    /// layers (provider-key prefixes, secret-named env, high-entropy tokens).
    /// Display prints only the count — the findings never carry the raw value.
    #[error("no-secret scanner flagged {} finding(s) in the sealed layers", .0.len())]
    SecretScanFindings(Vec<crate::scanner::SecretFinding>),

    /// A GPU requirement that would seal GPU device state into the snapshot was
    /// rejected (fail-closed). GPU execution is supported via a post-restore
    /// external capability / GPU runner class — GPU *state* is never snapshotted.
    #[error(
        "GPU state is not snapshottable: a '{gpu_mode}' GPU requirement cannot be sealed into a \
         Ready-State snapshot; provision the GPU as a post-restore external capability or use a \
         GPU runner class"
    )]
    GpuStateNotSnapshottable { gpu_mode: String },

    /// A backend runtime failure (VMM API error, boot/restore timeout, network
    /// setup, snapshot create/load). Distinct from `Unsupported` (which means
    /// the backend can't run here at all) — this is an operational failure of an
    /// available backend.
    #[error("snapshot backend '{backend}' error: {reason}")]
    Backend { backend: String, reason: String },

    /// A CapsuleFS operation failed.
    #[error(transparent)]
    CapsuleFs(#[from] capsulefs::CapsuleFsError),

    /// Underlying I/O failure.
    #[error("snapshot io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Fail-closed guard: a build must never seal GPU device state into a snapshot.
///
/// `None`/`External` are snapshot-safe (no GPU state, or GPU provisioned
/// post-restore). `Passthrough` (an in-VM GPU) is rejected — the capsule must
/// use a GPU runner class or an `[external.*]` GPU capability instead. Called at
/// the build orchestration seam, before [`SnapshotBackend::build_ready_state`].
pub fn ensure_gpu_not_in_snapshot(gpu_mode: GpuMode) -> Result<(), SnapshotError> {
    match gpu_mode {
        GpuMode::None | GpuMode::External => Ok(()),
        GpuMode::Passthrough => Err(SnapshotError::GpuStateNotSnapshottable {
            gpu_mode: "passthrough".to_string(),
        }),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_guard_allows_none_and_external() {
        assert!(ensure_gpu_not_in_snapshot(GpuMode::None).is_ok());
        assert!(ensure_gpu_not_in_snapshot(GpuMode::External).is_ok());
    }

    #[test]
    fn gpu_guard_rejects_passthrough() {
        let err = ensure_gpu_not_in_snapshot(GpuMode::Passthrough).unwrap_err();
        match &err {
            SnapshotError::GpuStateNotSnapshottable { gpu_mode } => {
                assert_eq!(gpu_mode, "passthrough");
            }
            other => panic!("expected GpuStateNotSnapshottable, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("not snapshottable") && msg.contains("passthrough"), "{msg}");
    }

    #[test]
    fn gpu_guard_rejects_in_vm_gpu_manifest() {
        use capsule::types::CapsuleManifest;
        const BASE: &str = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"

[targets.app.readiness_probe]
type = "http"
path = "/health"
"#;
        // In-VM GPU (vram requirement, no external binding) -> Passthrough -> rejected.
        let in_vm = CapsuleManifest::from_toml(&format!(
            "{BASE}\n[requirements]\nvram_min = \"8GB\"\n"
        ))
        .expect("parse");
        assert!(ensure_gpu_not_in_snapshot(in_vm.gpu_mode()).is_err());

        // External GPU capability -> External -> allowed.
        let external = CapsuleManifest::from_toml(&format!(
            "{BASE}\n[external.gpu]\ntype = \"gpu\"\n"
        ))
        .expect("parse");
        assert!(ensure_gpu_not_in_snapshot(external.gpu_mode()).is_ok());
    }

    #[test]
    fn binding_capabilities_reason_names_the_missing_piece() {
        // default (all false) ⇒ unsupported, reason lists the missing pieces.
        let none = BindingCapabilities::default();
        let r = none.unavailable_reason().unwrap();
        assert!(r.contains("firecracker") && r.contains("vsock") && r.contains("guest-agent"), "{r}");

        // supported ⇒ no reason.
        let ok = BindingCapabilities {
            supports_firecracker: true,
            supports_vsock: true,
            supports_guest_agent: true,
            supports_binding_lease: true,
            supports_stop_scrub: true,
            supports_no_secret_scan: true,
        };
        assert!(ok.unavailable_reason().is_none());

        // firecracker + guest-agent but NO host vsock ⇒ unsupported, names vsock only.
        let no_vsock = BindingCapabilities { supports_firecracker: true, supports_guest_agent: true, ..Default::default() };
        let r = no_vsock.unavailable_reason().unwrap();
        assert!(r.contains("vsock") && !r.contains("firecracker backend"), "{r}");
    }
}
