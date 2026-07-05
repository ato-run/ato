//! Ready-State Capsule artifact types (plan §2.1).
//!
//! A [`ReadyStateManifest`] is the sealed, content-addressed description of a
//! warm/booted capsule: which CapsuleFS layers reassemble it, how to restore
//! it, how to sanitize the clone after resume, and a proof that no secret was
//! sealed in. The layer *bytes* live in the CapsuleFS CAS; the manifest holds
//! the [`BlobManifest`] refs.
//!
//! Invariant (enforced by the build flow, see [`crate::SnapshotBackend::build_ready_state`]):
//! the `vmstate` and `memory` layers are captured **before** any secret/user
//! binding, so a sealed artifact is reusable across any host of the same
//! `runner_class_id` and carries no secret.

use capsule::foundation::install_lifecycle::RunnerClassId;
use capsulefs::{BlobManifest, HotsetProfile};
use serde::{Deserialize, Serialize};

/// Schema tag for the Ready-State manifest wire format.
pub const READY_STATE_SCHEMA: &str = "ato.ready-state/v1";

/// The sealed Ready-State artifact manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyStateManifest {
    /// Always [`READY_STATE_SCHEMA`].
    pub schema: String,
    /// `blake3:<hex>` of the originating capsule manifest (opaque here — the
    /// caller computes it from `capsule.toml`).
    pub capsule_manifest_hash: String,
    /// Phase 8a-HW (#912): whether the snapshot was built with a Firecracker vsock
    /// device (the guest-agent binding channel). The artifact self-describes this so
    /// restore preps the vsock UDS without an env flag. `false` for artifacts built
    /// without vsock (the default).
    #[serde(default)]
    pub has_vsock: bool,
    /// Restore-compatibility class the snapshot was built for (plan §5). `None`
    /// until host detection lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_class_id: Option<RunnerClassId>,
    /// Declared execution id facet, if known (opaque digest string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// CapsuleFS layer refs (each a [`BlobManifest`]).
    pub layers: ReadyStateLayers,
    /// Ordered chunk prefetch profile recorded at build time.
    #[serde(default)]
    pub hotset_profile: HotsetProfile,
    /// Which backend sealed this and its format/template.
    pub snapshot_backend: SnapshotBackendInfo,
    /// How to bring a restored session to readiness.
    pub restore_contract: RestoreContract,
    /// Post-resume sanitizer steps and where each runs.
    pub sanitizer_contract: SanitizerContract,
    /// Proof that the sealed layers contain no secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_secret_proof: Option<NoSecretProof>,
    /// Opaque id of the build receipt that produced this artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_receipt_id: Option<String>,
    /// v1.2 PR 3d: supervisor-build facts (names + advisory hardening only, never a
    /// value). `None` for no-binding artifacts — additive, default-safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_build: Option<SupervisorBuildReceipt>,
}

/// v1.2 PR 3d: what the supervisor build drive did, recorded into the sealed
/// manifest. Binding NAMES and advisory hardening facts only — the placeholder
/// values are generated per build, delivered over vsock, revoked before the
/// snapshot, and never stored or logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorBuildReceipt {
    /// The binding names the build drive delivered placeholders for.
    pub binding_names: Vec<String>,
    /// Whether the build-boot kernel cmdline carried the page-hygiene args
    /// (`init_on_free=1 init_on_alloc=1 page_poison=1`). Restore inherits the
    /// build cmdline, so this describes the artifact's whole lifetime.
    pub page_hygiene_boot_args: bool,
    /// ADVISORY (kernel-dependent, #947 finding): whether the revoked placeholder
    /// values were absent from the sealed mem+vmstate bytes. `Some(false)` does NOT
    /// gate — a kernel without `init_on_free` support leaves freed pages intact.
    /// `None` = not evaluated (e.g. the Fake backend, which boots nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder_absent_from_seal: Option<bool>,
}

impl ReadyStateManifest {
    /// Content-addressed id of this manifest: `blake3:<hex>` over the JCS
    /// canonical form (structural-id family).
    pub fn id(&self) -> String {
        let canonical = serde_jcs::to_vec(self)
            .expect("ReadyStateManifest is always JCS-canonicalizable");
        format!("blake3:{}", blake3::hash(&canonical).to_hex())
    }

    /// Total bytes across all present layers.
    pub fn total_layer_bytes(&self) -> u64 {
        self.layers.iter().map(|(_, m)| m.total_len).sum()
    }
}

/// CapsuleFS refs for each Ready-State layer. The bytes live in CAS; these are
/// the [`BlobManifest`] ref-lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyStateLayers {
    /// Read-only base rootfs image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rootfs: Option<BlobManifest>,
    /// Language runtime / interpreter layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<BlobManifest>,
    /// Resolved dependencies / build output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency: Option<BlobManifest>,
    /// Application source / build output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<BlobManifest>,
    /// VMM VM state file (device + CPU + vcpu state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vmstate: Option<BlobManifest>,
    /// Guest memory image (page-chunked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<BlobManifest>,
}

impl ReadyStateLayers {
    /// Iterate present layers as `(name, manifest)`.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &BlobManifest)> {
        [
            ("rootfs", self.rootfs.as_ref()),
            ("runtime", self.runtime.as_ref()),
            ("dependency", self.dependency.as_ref()),
            ("app", self.app.as_ref()),
            ("vmstate", self.vmstate.as_ref()),
            ("memory", self.memory.as_ref()),
        ]
        .into_iter()
        .filter_map(|(name, m)| m.map(|m| (name, m)))
    }
}

/// Which backend sealed the artifact, and the format/template it pins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotBackendInfo {
    /// Backend id, e.g. `"firecracker"` or `"fake"`.
    pub kind: String,
    /// Backend version string.
    pub version: String,
    /// Snapshot format version, e.g. `"fc-v2"`.
    pub snapshot_format_version: String,
    /// CPU template used (plan §5), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_template: Option<String>,
}

/// How a restored session reaches readiness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreContract {
    /// Expected time from LoadSnapshot to first healthy response (SLO).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_ready_ms: Option<u32>,
    /// Guest ports the app exposes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    /// Healthcheck path, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<String>,
}

/// Ordered post-resume sanitizer steps and where each runs (plan §8.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizerContract {
    /// Sanitizer steps, applied in order before a restored session is exposed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<SanitizerStep>,
}

/// One sanitizer action and the layer responsible for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizerStep {
    /// What is sanitized, e.g. `"session_id_regenerate"`, `"entropy_reseed"`,
    /// `"network_reconnect"`.
    pub step: String,
    /// Which layer performs it.
    pub layer: SanitizerLayer,
}

/// Where a sanitizer step executes (plan §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizerLayer {
    /// In-guest agent over vsock.
    GuestAgent,
    /// Host-side runtime (device/network/port/overlay setup).
    Host,
    /// Both host and guest cooperate.
    HostAndGuest,
    /// Application-level hook.
    App,
}

/// Proof that no secret was sealed into the artifact (plan §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoSecretProof {
    /// Scanner version that produced this proof.
    pub scanner_version: String,
    /// Names of the layers scanned.
    pub scanned_layers: Vec<String>,
    /// Blocking findings that fail the build closed (declared markers, provider
    /// key prefixes, secret-named env). Empty when clean.
    #[serde(default)]
    pub findings: Vec<String>,
    /// Non-blocking advisories that do NOT fail the build — high-entropy token
    /// runs, which false-positive on lockfile hashes / minified assets / binary
    /// blobs in real dependency/app layers. Surfaced for review, not gating.
    #[serde(default)]
    pub advisories: Vec<String>,
    /// Verdict, e.g. `"clean"`.
    pub verdict: String,
    /// Per-layer scan coverage: what was synchronously fail-closed-checked vs
    /// advisory, scanned vs reused from the content-addressed cache, and whether
    /// the advisory scan was budget-capped. Keeps the security contract honest
    /// after large opaque layers are cached/deferred. Additive (`serde(default)`).
    #[serde(default)]
    pub coverage: Vec<LayerScanCoverage>,
}

/// What was actually checked for one sealed layer (see [`NoSecretProof::coverage`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerScanCoverage {
    /// Layer name (rootfs/runtime/dependency/app/vmstate/memory).
    pub layer: String,
    /// The layer's content hash (the scan-cache key).
    pub content_hash: String,
    pub scanner_version: String,
    pub policy_version: String,
    /// Declared-marker scan ran on the full layer bytes (always true).
    pub declared_checked: bool,
    /// Heuristic passes run synchronously as a build-FAILING gate (app/dependency).
    pub blocking_checks: Vec<String>,
    /// Heuristic passes run as advisory only (large opaque layers; entropy everywhere).
    pub advisory_checks: Vec<String>,
    /// `"full"` or `"budget_capped"` (advisory scan bounded by the byte budget).
    pub coverage: String,
    /// `"scanned"` or `"cache_hit"` (advisory result reused from the scan cache).
    pub source: String,
}

impl NoSecretProof {
    /// Whether the **blocking gate** passed (no declared markers, no app/dependency
    /// provider/env). NOTE: `verdict == "clean"` means *blocking-gate clean* — not
    /// that every byte of every layer was heuristically inspected. Advisory
    /// scans of large opaque layers may be `budget_capped` (see
    /// [`coverage`](Self::coverage)); consult `coverage` for the full picture.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && self.verdict == "clean"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsulefs::{CasStore, ChunkingKind, LayerKind, store_blob};

    fn manifest_with_layers() -> (tempfile::TempDir, ReadyStateManifest) {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let rootfs = store_blob(&store, LayerKind::Rootfs, b"rootfs-bytes", ChunkingKind::ContentDefined).unwrap();
        let memory = store_blob(
            &store,
            LayerKind::Memory,
            &vec![3u8; 100_000],
            ChunkingKind::PageAligned { page_size: 64 * 1024 },
        )
        .unwrap();
        let m = ReadyStateManifest {
            schema: READY_STATE_SCHEMA.into(),
            capsule_manifest_hash: "blake3:cap".into(),
            has_vsock: false,
            runner_class_id: None,
            execution_id: None,
            layers: ReadyStateLayers {
                rootfs: Some(rootfs),
                memory: Some(memory),
                ..Default::default()
            },
            hotset_profile: HotsetProfile::default(),
            snapshot_backend: SnapshotBackendInfo {
                kind: "fake".into(),
                version: "0.1.0".into(),
                snapshot_format_version: "fake-v1".into(),
                cpu_template: None,
            },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        };
        (dir, m)
    }

    #[test]
    fn id_is_stable_and_content_sensitive() {
        let (_d, m) = manifest_with_layers();
        let id = m.id();
        assert!(id.starts_with("blake3:"));
        assert_eq!(id, m.id());
        let mut other = m.clone();
        other.capsule_manifest_hash = "blake3:different".into();
        assert_ne!(id, other.id());
    }

    #[test]
    fn round_trips_through_json() {
        let (_d, m) = manifest_with_layers();
        let json = serde_json::to_string(&m).unwrap();
        let back: ReadyStateManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.id(), m.id());
    }

    #[test]
    fn iter_lists_only_present_layers_and_sums_bytes() {
        let (_d, m) = manifest_with_layers();
        let names: Vec<_> = m.layers.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["rootfs", "memory"]);
        assert_eq!(
            m.total_layer_bytes(),
            m.layers.iter().map(|(_, b)| b.total_len).sum::<u64>()
        );
        assert!(m.total_layer_bytes() > 100_000);
    }
}
