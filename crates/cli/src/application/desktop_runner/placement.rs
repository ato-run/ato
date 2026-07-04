//! Desktop Runner placement *decision* layer (#838).
//!
//! This is the seam between the capability probe ([`super::probe`] /
//! [`DesktopRunnerFacts`]) and the run path ([`super::execute`]). It turns the
//! low-level [`matching::select_placement`] result into a
//! [`DesktopPlacementDecision`] — a serializable receipt/log record that says
//! *what the host would do* and *why*.
//!
//! **Decision vs execution.** A `local_cold_oci_candidate` decision means a
//! local cold-OCI backend is available **and** the executor branch for that
//! substrate is wired ([`super::cold_oci::run`]) — so `is_executable_now` is
//! `true` for that placement. A `suggest_managed_runner` /
//! `ready_state_restore_unsupported_local` decision is a *recommendation*, never
//! an automatic dispatch — `is_executable_now` is `false`. The decision is
//! produced both when the Desktop Runner is explicitly considered (the
//! `ato doctor desktop-runner` diagnostic) and inside the `ato run` placement
//! gate; `ato doctor` never executes workloads, but a candidate decision it
//! renders is still executable by the run path.

use serde::Serialize;

use capsule::foundation::install_lifecycle::RunnerClassFacts;

use super::facts::{DesktopRunnerFacts, LocalBackendBlocker, PROVIDER_KIND_DESKTOP};
use super::matching::{DesktopPlacement, select_placement};

/// Stable `placement` kind labels (snake_case, for receipts/logs).
pub(crate) mod kind {
    pub(crate) const LOCAL_COLD_OCI_CANDIDATE: &str = "local_cold_oci_candidate";
    pub(crate) const READY_STATE_RESTORE_UNSUPPORTED_LOCAL: &str =
        "ready_state_restore_unsupported_local";
    pub(crate) const SUGGEST_MANAGED_RUNNER: &str = "suggest_managed_runner";
    pub(crate) const READY_STATE_RESTORE: &str = "ready_state_restore";
}

/// A Desktop Runner placement decision — the receipt/log record. Pure data; no
/// side effects, no execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DesktopPlacementDecision {
    pub(crate) provider_kind: String,
    pub(crate) host_os: String,
    pub(crate) host_arch: String,
    pub(crate) ready_state_enabled: bool,
    pub(crate) artifact_class_present: bool,
    /// One of [`kind`].
    pub(crate) placement: String,
    /// The local backend substrate, when the decision targets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) backend_substrate: Option<String>,
    /// Human-readable rationale (safe to log/show).
    pub(crate) reason: String,
    /// Structured reasons the host has no local cold-OCI backend, surfaced from
    /// [`matching::DesktopPlacement::SuggestManagedRunner`] when the decision
    /// was reached via `cold_or_managed`. Empty for decisions whose cause is not
    /// a missing backend (e.g. Ready-State-without-artifact, class mismatch).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) local_backend_blockers: Vec<LocalBackendBlocker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<String>,
    /// Whether the placement can be executed **right now** by the wired
    /// executor. `true` for `local_cold_oci_candidate` (the cold-OCI executor
    /// branch exists for both Apple Containerization and Podman substrates);
    /// `false` for managed-runner suggestions and Ready-State-unsupported
    /// decisions (no local executor for those paths yet). This is the placement
    /// contract invariant: a candidate is only returned when the executor is
    /// wired.
    pub(crate) is_executable_now: bool,
}

impl DesktopPlacementDecision {
    /// True when the decision recommends a managed runner (explicit, not auto).
    pub(crate) fn suggests_managed_runner(&self) -> bool {
        self.placement == kind::SUGGEST_MANAGED_RUNNER
            || self.placement == kind::READY_STATE_RESTORE_UNSUPPORTED_LOCAL
    }
}

/// Decide a Desktop Runner placement from explicit inputs (pure).
pub(crate) fn decide(
    facts: &DesktopRunnerFacts,
    host_class: &RunnerClassFacts,
    artifact_class: Option<&RunnerClassFacts>,
    ready_state_enabled: bool,
) -> DesktopPlacementDecision {
    let (placement, backend_substrate, reason, blockers, is_executable_now) =
        match select_placement(facts, host_class, artifact_class, ready_state_enabled) {
            DesktopPlacement::ReadyStateRestore { backend_substrate } => (
                kind::READY_STATE_RESTORE,
                Some(backend_substrate.clone()),
                format!(
                    "exact RunnerClass match; Ready-State restore permitted on {backend_substrate} \
                     (local restore executor is not wired yet)"
                ),
                Vec::new(),
                false, // Ready-State restore executor is not wired yet.
            ),
            DesktopPlacement::ColdOciLocal { backend_substrate } => (
                kind::LOCAL_COLD_OCI_CANDIDATE,
                Some(backend_substrate.clone()),
                format!(
                    "local cold-OCI candidate on {backend_substrate}; explicit Desktop Runner \
                     execution is available"
                ),
                Vec::new(),
                true, // The cold-OCI executor branch is wired for this substrate.
            ),
            DesktopPlacement::ReadyStateRestoreUnsupportedLocal { reason } => (
                kind::READY_STATE_RESTORE_UNSUPPORTED_LOCAL,
                None,
                reason,
                Vec::new(),
                false,
            ),
            DesktopPlacement::SuggestManagedRunner { reason, blockers } => {
                (kind::SUGGEST_MANAGED_RUNNER, None, reason, blockers, false)
            }
        };

    DesktopPlacementDecision {
        provider_kind: PROVIDER_KIND_DESKTOP.to_string(),
        host_os: facts.host_os.clone(),
        host_arch: facts.host_arch.clone(),
        ready_state_enabled,
        artifact_class_present: artifact_class.is_some(),
        placement: placement.to_string(),
        backend_substrate,
        reason,
        local_backend_blockers: blockers,
        diagnostics: facts.diagnostics.clone(),
        is_executable_now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::desktop_runner::macos::{MacosProbeInputs, build_macos_facts};

    fn supported_inputs() -> MacosProbeInputs {
        MacosProbeInputs {
            host_arch: "aarch64".into(),
            product_version: Some("26.0".into()),
            is_apple_silicon: true,
            container_path: Some("/usr/local/bin/container".into()),
            container_version: Some("container 0.1.0".into()),
            container_service_running: false,
            podman_binary_present: false,
            podman_version: None,
            podman_machine: super::super::macos::PodmanMachineProbe::default(),
        }
    }

    fn linux_x86_64_fc() -> RunnerClassFacts {
        RunnerClassFacts {
            os: "linux".into(),
            arch: "x86_64".into(),
            kernel_abi_class: "linux-6.1".into(),
            vmm: "firecracker".into(),
            vmm_version: "1.7.0".into(),
            snapshot_format: "fc-v2".into(),
            cpu_template: Some("T2".into()),
            cpu_features: vec![],
            guest_kernel_id: "blake3:kern".into(),
            rootfs_base_id: "blake3:rootfs".into(),
            device_profile: "virtio-blk+virtio-net+vsock".into(),
            cgroup: "v2".into(),
            network_model: "tap-nat".into(),
        }
    }

    #[test]
    fn supported_mac_ready_state_disabled_is_local_cold_oci_candidate_and_executable() {
        let facts = build_macos_facts(&supported_inputs(), "0.7.0");
        let host_class = RunnerClassFacts::from_host();
        let d = decide(&facts, &host_class, None, false);
        assert_eq!(d.placement, kind::LOCAL_COLD_OCI_CANDIDATE);
        assert_eq!(
            d.backend_substrate.as_deref(),
            Some(super::super::facts::SUBSTRATE_APPLE_CONTAINERIZATION)
        );
        assert!(
            d.is_executable_now,
            "cold-OCI candidate means executor is wired — placement contract invariant"
        );
        assert!(!d.suggests_managed_runner());
    }

    #[test]
    fn supported_mac_ready_state_enabled_suggests_managed_runner() {
        let facts = build_macos_facts(&supported_inputs(), "0.7.0");
        let host_class = RunnerClassFacts::from_host();
        // No specific artifact: Ready-State on but nothing sealed locally.
        let d = decide(&facts, &host_class, None, true);
        assert_eq!(d.placement, kind::SUGGEST_MANAGED_RUNNER);
        assert!(d.suggests_managed_runner());
        assert!(
            d.reason.to_lowercase().contains("restore") || d.reason.contains("managed runner"),
            "reason should explain the local Ready-State gap: {}",
            d.reason
        );
        assert!(!d.is_executable_now);
    }

    #[test]
    fn ready_state_enabled_matching_artifact_no_local_restore_is_unsupported_local() {
        let facts = build_macos_facts(&supported_inputs(), "0.7.0");
        let host_class = RunnerClassFacts::from_host();
        // Artifact class == host class ⇒ exact match, but no restore backend (M2).
        let artifact = host_class.clone();
        let d = decide(&facts, &host_class, Some(&artifact), true);
        assert_eq!(d.placement, kind::READY_STATE_RESTORE_UNSUPPORTED_LOCAL);
        assert!(d.suggests_managed_runner());
        assert!(!d.is_executable_now);
    }

    #[test]
    fn no_container_suggests_managed_runner_with_diagnostics() {
        let mut i = supported_inputs();
        i.container_path = None;
        i.container_version = None;
        let facts = build_macos_facts(&i, "0.7.0");
        let host_class = RunnerClassFacts::from_host();
        let d = decide(&facts, &host_class, None, false);
        assert_eq!(d.placement, kind::SUGGEST_MANAGED_RUNNER);
        assert!(
            d.diagnostics.iter().any(|x| x.contains("container")),
            "{:?}",
            d.diagnostics
        );
        assert!(!d.is_executable_now);
    }

    #[test]
    fn old_macos_suggests_managed_runner_with_version_diagnostic() {
        let mut i = supported_inputs();
        i.product_version = Some("15.7.4".into());
        let facts = build_macos_facts(&i, "0.7.0");
        let host_class = RunnerClassFacts::from_host();
        let d = decide(&facts, &host_class, None, false);
        assert_eq!(d.placement, kind::SUGGEST_MANAGED_RUNNER);
        assert!(d.diagnostics.iter().any(|x| x.contains("macOS 26")));
    }

    #[test]
    fn foreign_firecracker_artifact_is_not_cold_started_on_macos() {
        let facts = build_macos_facts(&supported_inputs(), "0.7.0");
        let host_class = RunnerClassFacts::from_host();
        // A linux/x86_64/firecracker artifact must never cold-start locally.
        let d = decide(&facts, &host_class, Some(&linux_x86_64_fc()), true);
        assert_eq!(d.placement, kind::SUGGEST_MANAGED_RUNNER);
        assert_ne!(d.placement, kind::LOCAL_COLD_OCI_CANDIDATE);
        assert!(d.suggests_managed_runner());
    }

    #[test]
    fn decision_serializes_with_executable_now_true_for_cold_oci() {
        let facts = build_macos_facts(&supported_inputs(), "0.7.0");
        let host_class = RunnerClassFacts::from_host();
        let d = decide(&facts, &host_class, None, false);
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"is_executable_now\":true"), "{json}");
        assert!(json.contains("\"provider_kind\":\"desktop\""), "{json}");
    }
}
