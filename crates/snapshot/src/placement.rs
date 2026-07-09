//! Backend placement contract (requirements §0.5).
//!
//! A capsule declares **what it needs** ([`BackendRequirements`]) — never which
//! backend. Placement then picks a backend whose [`BackendCapabilities`] satisfy
//! the requirements ([`matches`]) and that can seal-before-bind
//! ([`ready_state_safe`]). This is the seam that keeps Firecracker a *reference*
//! rather than the answer: QEMU/Kata (or any future backend) slot in by
//! advertising the right capabilities, with no caller change.
//!
//! Matching is intentionally minimal for M0/M1: enum facets use exact equality
//! (no `vm ⊇ microvm` subsumption), and bool facets constrain only when
//! `Some(true)` (`Some(false)`/`None` = unconstrained). A later milestone can add
//! subsumption without changing callers.

use serde::{Deserialize, Serialize};

use crate::backend::{
    BackendCapabilities, DeviceProfile, FilesystemModel, GpuMode, IsolationBoundary, SnapshotKind,
};

/// What a capsule requires of a backend. Every facet is optional; a `None`
/// facet is unconstrained. Serde-derived so a future `capsule.toml [requires]`
/// table can deserialize straight into it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendRequirements {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_kind: Option<SnapshotKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_snapshot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_model: Option<FilesystemModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_profile: Option<DeviceProfile>,
    /// GPU posture. **Only `Some(Passthrough)` constrains placement** (and no
    /// M0/M1 backend is passthrough-capable, so it fails closed). `External` GPU
    /// is provisioned post-restore by the external-capability resolver, NOT the
    /// snapshot backend, so it must NOT filter backend placement; `None`/`External`
    /// are treated as unconstrained here. See [`matches`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_mode: Option<GpuMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci_native: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation_boundary: Option<IsolationBoundary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposable_overlay: Option<bool>,
}

/// Why placement could not select a backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlacementError {
    /// No available backend satisfies the requirements.
    #[error("no available backend satisfies the capsule's requirements: {0}")]
    NoMatchingBackend(String),
    /// A backend matched but cannot seal-before-bind — refused for Ready-State.
    #[error(
        "backend '{backend_id}' matches the requirements but does not support seal-before-bind; \
         refusing to use it for a Ready-State capsule (safety invariant)"
    )]
    SealBeforeBindUnsupported { backend_id: String },
}

/// `requires ⊆ capabilities`. Enum facets must equal the requirement when set;
/// bool facets constrain only when `Some(true)`. Does **not** itself enforce the
/// seal-before-bind gate (see [`ready_state_safe`]).
pub fn matches(req: &BackendRequirements, cap: &BackendCapabilities) -> bool {
    req.snapshot_kind.is_none_or(|k| k == cap.snapshot_kind)
        && req.filesystem_model.is_none_or(|f| f == cap.filesystem_model)
        && req.device_profile.is_none_or(|d| d == cap.device_profile)
        // GPU: only an in-VM `Passthrough` requirement constrains placement.
        // `External` GPU is a post-restore external-capability binding (not a
        // backend capability), and `None` runs anywhere — neither filters, so an
        // external-GPU capsule still places on a normal microVM backend.
        && (req.gpu_mode != Some(GpuMode::Passthrough) || cap.gpu_mode == GpuMode::Passthrough)
        && req
            .isolation_boundary
            .is_none_or(|i| i == cap.isolation_boundary)
        && (!matches!(req.memory_snapshot, Some(true)) || cap.memory_snapshot)
        && (!matches!(req.oci_native, Some(true)) || cap.oci_native)
        && (!matches!(req.disposable_overlay, Some(true)) || cap.supports_disposable_overlay)
}

/// The hard Ready-State safety invariant: a backend may seal layers before any
/// user binding. Exposed standalone for callers/diagnostics.
pub fn ready_state_safe(cap: &BackendCapabilities) -> bool {
    cap.supports_seal_before_bind
}

/// Select the first backend that is available, matches `req`, **and** passes the
/// seal-before-bind gate; returns its index into `candidates`. If a matching,
/// available backend exists but only fails the seal gate, returns
/// [`PlacementError::SealBeforeBindUnsupported`]; otherwise
/// [`PlacementError::NoMatchingBackend`].
pub fn select_ready_state_backend(
    req: &BackendRequirements,
    candidates: &[BackendCapabilities],
) -> Result<usize, PlacementError> {
    let mut seal_blocked: Option<String> = None;
    for (idx, cap) in candidates.iter().enumerate() {
        if cap.available && matches(req, cap) {
            if ready_state_safe(cap) {
                return Ok(idx);
            } else if seal_blocked.is_none() {
                seal_blocked = Some(cap.backend_id.clone());
            }
        }
    }
    match seal_blocked {
        Some(backend_id) => Err(PlacementError::SealBeforeBindUnsupported { backend_id }),
        None => Err(PlacementError::NoMatchingBackend(format!("{req:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A microVM backend (Fake/Firecracker shape): block fs, no GPU, sealable.
    fn microvm_caps(id: &str, available: bool) -> BackendCapabilities {
        BackendCapabilities {
            backend_id: id.to_string(),
            available,
            reason: None,
            arch: "test".to_string(),
            kvm_present: false,
            vmm_version: None,
            snapshot_kind: SnapshotKind::MicroVm,
            memory_snapshot: true,
            filesystem_model: FilesystemModel::Block,
            device_profile: DeviceProfile::Minimal,
            gpu_mode: GpuMode::None,
            oci_native: false,
            isolation_boundary: IsolationBoundary::MicroVm,
            supports_seal_before_bind: true,
            supports_disposable_overlay: true,
            supports_uffd_mem_backend: false,
            uffd_reason: Some("test fixture".to_string()),
            binding: Default::default(),
        }
    }

    /// An OCI-native backend (Kata shape): no snapshot, oci_native, NOT sealable.
    fn oci_caps(id: &str, available: bool) -> BackendCapabilities {
        BackendCapabilities {
            backend_id: id.to_string(),
            available,
            reason: None,
            arch: "test".to_string(),
            kvm_present: false,
            vmm_version: None,
            snapshot_kind: SnapshotKind::None,
            memory_snapshot: false,
            filesystem_model: FilesystemModel::Virtiofs,
            device_profile: DeviceProfile::Virtiofs,
            gpu_mode: GpuMode::None,
            oci_native: true,
            isolation_boundary: IsolationBoundary::MicroVm,
            supports_seal_before_bind: false,
            supports_disposable_overlay: true,
            supports_uffd_mem_backend: false,
            uffd_reason: Some("test fixture".to_string()),
            binding: Default::default(),
        }
    }

    #[test]
    fn empty_requirements_match_any_backend() {
        let req = BackendRequirements::default();
        assert!(matches(&req, &microvm_caps("fake", true)));
        assert!(matches(&req, &oci_caps("kata", true)));
    }

    #[test]
    fn microvm_requirement_matches_microvm_backend() {
        let req = BackendRequirements {
            snapshot_kind: Some(SnapshotKind::MicroVm),
            ..Default::default()
        };
        assert!(matches(&req, &microvm_caps("fake", true)));
        assert!(matches(&req, &microvm_caps("firecracker", false)));
    }

    #[test]
    fn microvm_requirement_rejects_oci_native_backend() {
        let req = BackendRequirements {
            snapshot_kind: Some(SnapshotKind::MicroVm),
            ..Default::default()
        };
        assert!(!matches(&req, &oci_caps("kata", true)));
    }

    #[test]
    fn oci_native_requirement_matches_only_oci_backend() {
        let req = BackendRequirements {
            oci_native: Some(true),
            ..Default::default()
        };
        assert!(matches(&req, &oci_caps("kata", true)));
        assert!(!matches(&req, &microvm_caps("firecracker", true)));
    }

    #[test]
    fn external_gpu_requirement_does_not_filter_placement() {
        // THE fix: an external-GPU capsule (GPU provisioned post-restore) must
        // still place on a normal microVM backend whose gpu_mode is None.
        let req_external = BackendRequirements {
            snapshot_kind: Some(SnapshotKind::MicroVm),
            gpu_mode: Some(GpuMode::External),
            ..Default::default()
        };
        let fake = microvm_caps("fake", true); // gpu_mode: None
        assert!(matches(&req_external, &fake));
        assert_eq!(select_ready_state_backend(&req_external, &[fake]), Ok(0));
        // None is likewise unconstrained.
        let req_none = BackendRequirements {
            gpu_mode: Some(GpuMode::None),
            ..Default::default()
        };
        assert!(matches(&req_none, &microvm_caps("fake", true)));
    }

    #[test]
    fn passthrough_gpu_requirement_finds_no_m0_backend() {
        // Only Passthrough constrains; no M0/M1 backend is passthrough-capable,
        // so it fails closed (a passthrough capsule is Ready-State-ineligible
        // anyway, gated earlier).
        let req_pass = BackendRequirements {
            gpu_mode: Some(GpuMode::Passthrough),
            ..Default::default()
        };
        assert!(!matches(&req_pass, &microvm_caps("fake", true)));
        let mut passthrough = microvm_caps("qemu", true);
        passthrough.gpu_mode = GpuMode::Passthrough;
        assert!(matches(&req_pass, &passthrough));
    }

    #[test]
    fn bool_requirement_only_constrains_when_true() {
        let cap = microvm_caps("fake", true); // memory_snapshot = true
        let req_false = BackendRequirements {
            memory_snapshot: Some(false),
            ..Default::default()
        };
        assert!(matches(&req_false, &cap), "Some(false) is unconstrained");
        let mut no_mem = cap.clone();
        no_mem.memory_snapshot = false;
        let req_true = BackendRequirements {
            memory_snapshot: Some(true),
            ..Default::default()
        };
        assert!(!matches(&req_true, &no_mem));
    }

    #[test]
    fn select_picks_first_available_matching_safe_backend() {
        let req = BackendRequirements {
            snapshot_kind: Some(SnapshotKind::MicroVm),
            ..Default::default()
        };
        let candidates = vec![
            microvm_caps("firecracker", false),
            microvm_caps("fake", true),
        ];
        assert_eq!(select_ready_state_backend(&req, &candidates), Ok(1));
    }

    #[test]
    fn select_skips_unavailable_backend() {
        let req = BackendRequirements {
            snapshot_kind: Some(SnapshotKind::MicroVm),
            ..Default::default()
        };
        let candidates = vec![microvm_caps("firecracker", false)];
        assert!(matches!(
            select_ready_state_backend(&req, &candidates),
            Err(PlacementError::NoMatchingBackend(_))
        ));
    }

    #[test]
    fn seal_before_bind_gate_rejects_unsafe_backend() {
        let req = BackendRequirements::default();
        // available + matches (empty req) but cannot seal -> refused.
        let candidates = vec![oci_caps("kata", true)];
        match select_ready_state_backend(&req, &candidates) {
            Err(PlacementError::SealBeforeBindUnsupported { backend_id }) => {
                assert_eq!(backend_id, "kata");
            }
            other => panic!("expected SealBeforeBindUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn select_skips_unsafe_and_picks_safe() {
        let req = BackendRequirements::default();
        let candidates = vec![oci_caps("kata", true), microvm_caps("fake", true)];
        assert_eq!(select_ready_state_backend(&req, &candidates), Ok(1));
    }

    #[test]
    fn no_matching_backend_errors() {
        let req = BackendRequirements {
            filesystem_model: Some(FilesystemModel::Virtiofs),
            ..Default::default()
        };
        // only a block-fs sealable backend -> no match (and it IS sealable, so
        // not a seal error).
        let candidates = vec![microvm_caps("fake", true)];
        assert!(matches!(
            select_ready_state_backend(&req, &candidates),
            Err(PlacementError::NoMatchingBackend(_))
        ));
    }

    #[test]
    fn requirements_round_trip_through_json() {
        let req = BackendRequirements {
            snapshot_kind: Some(SnapshotKind::MicroVm),
            gpu_mode: Some(GpuMode::None),
            oci_native: Some(false),
            ..Default::default()
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: BackendRequirements = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
        // Empty default serializes to {} (all facets skipped).
        assert_eq!(
            serde_json::to_string(&BackendRequirements::default()).unwrap(),
            "{}"
        );
    }

    #[test]
    fn microvm_uses_the_contract_token_not_snake_case() {
        // requirements §0.5 token is "microvm", not snake_case "micro_vm".
        let req = BackendRequirements {
            snapshot_kind: Some(SnapshotKind::MicroVm),
            isolation_boundary: Some(IsolationBoundary::MicroVm),
            ..Default::default()
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"microvm\""), "{json}");
        assert!(!json.contains("micro_vm"), "{json}");
    }
}
