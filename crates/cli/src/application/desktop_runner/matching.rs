//! Desktop Runner placement matching (#838, M0): decide what a Desktop Runner
//! host may do for a given run, fail-closed.
//!
//! Two layers:
//!
//! 1. [`class_compatible`] — the Ready-State **exact-class** gate. It reuses
//!    [`RunnerClassFacts::ensure_compatible`] (the same fail-closed contract the
//!    snapshot restore Prepare gate uses), so a macOS aarch64 host can never be
//!    told to restore a `linux`/`x86_64`/`firecracker` artifact.
//! 2. [`select_placement`] — combines class compatibility, the host's
//!    [`DesktopRunnerFacts`], and the Ready-State flag into a [`DesktopPlacement`].
//!    M0–M2 never restore (no backend `supports_ready_state_restore`), so a
//!    Ready-State run resolves to an **explicit** managed-runner suggestion
//!    rather than a silent cold fallback or a wrong-class restore.
//!
//! Consumed by the placement-decision layer ([`super::placement`], MacBook M2),
//! which the developer diagnostic surfaces — no execution happens here.

use capsule::foundation::install_lifecycle::{RunnerClassFacts, RunnerClassMismatch};

use super::facts::{DesktopRunnerFacts, LocalBackendBlocker, ReadyStateKind};

/// What a placement decision resolves to for a Desktop Runner host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesktopPlacement {
    /// Exact RunnerClass match **and** a local backend can restore — a
    /// Ready-State restore is permitted. (Unreachable in M0–M2: no backend sets
    /// `supports_ready_state_restore`.)
    ReadyStateRestore { backend_substrate: String },
    /// No Ready-State restore; start the capsule cold on a local backend.
    ColdOciLocal { backend_substrate: String },
    /// The artifact's class matches this host but no local backend can restore
    /// Ready-State yet, and Ready-State was explicitly enabled — so we will not
    /// silently cold-start. An explicit managed-runner handoff is the answer.
    ReadyStateRestoreUnsupportedLocal { reason: String },
    /// No safe local path. An **explicit** managed-runner handoff — the reason
    /// is meant to be logged and shown, never a silent degrade. `blockers`
    /// carries the structured per-precondition reasons the host has no local
    /// cold-OCI backend (empty when the handoff is for a non-backend cause such
    /// as a Ready-State-without-artifact request or a class mismatch).
    SuggestManagedRunner {
        reason: String,
        blockers: Vec<LocalBackendBlocker>,
    },
}

/// The Ready-State exact-class gate: may an artifact built for `artifact_class`
/// be restored on a host of `host_class`?
///
/// Thin, deliberate wrapper over [`RunnerClassFacts::ensure_compatible`]
/// (`self` = built-for/expected, arg = candidate host) so the Desktop Runner
/// uses the *same* contract as the snapshot restore gate — divergence here would
/// be a correctness hole. Returns a typed [`RunnerClassMismatch`] (never a bare
/// bool) so "unknown" can't be mistaken for "compatible".
pub(crate) fn class_compatible(
    artifact_class: &RunnerClassFacts,
    host_class: &RunnerClassFacts,
) -> Result<(), RunnerClassMismatch> {
    artifact_class.ensure_compatible(host_class)
}

/// The local cold backend a placement would use, if any. Honors the substrate
/// preference order (Apple Containerization before Podman) via
/// [`DesktopRunnerFacts::preferred_local_cold_backend`].
fn local_cold_backend(host: &DesktopRunnerFacts) -> Option<&super::facts::BackendCapability> {
    host.preferred_local_cold_backend()
        .filter(|b| b.ready_state_kind == ReadyStateKind::ColdOci)
}

/// Offer the local cold backend, else suggest a managed runner (explicit). The
/// managed-runner suggestion carries the host's structured
/// [`LocalBackendBlocker`]s so the caller can name every missing precondition
/// (Apple silicon / macOS 26+ / `container`) instead of a single generic line.
fn cold_or_managed(host: &DesktopRunnerFacts) -> DesktopPlacement {
    match local_cold_backend(host) {
        Some(b) => DesktopPlacement::ColdOciLocal {
            backend_substrate: b.substrate.clone(),
        },
        None => DesktopPlacement::SuggestManagedRunner {
            reason: format!(
                "no local Desktop Runner backend on {}/{}; use a managed runner",
                host.host_os, host.host_arch
            ),
            blockers: host.local_backend_blockers.clone(),
        },
    }
}

/// Decide a placement for a Desktop Runner host.
///
/// - `artifact_class = Some(_)` means a Ready-State artifact was requested:
///   - class **mismatch** → explicit managed-runner suggestion (never restore a
///     wrong-class artifact, never silently cold-start it).
///   - class **match**, a backend can restore → [`DesktopPlacement::ReadyStateRestore`].
///   - class **match**, no backend can restore (M0) → if Ready-State is disabled,
///     fall back to local cold OCI; if enabled, explicit managed-runner suggestion
///     (the user opted into Ready-State, so don't silently degrade).
/// - `artifact_class = None` means a cold run:
///   - Ready-State **enabled** but no artifact → explicit managed-runner suggestion.
///   - Ready-State **disabled** → local cold OCI, else managed-runner suggestion.
pub(crate) fn select_placement(
    host: &DesktopRunnerFacts,
    host_class: &RunnerClassFacts,
    artifact_class: Option<&RunnerClassFacts>,
    ready_state_enabled: bool,
) -> DesktopPlacement {
    match artifact_class {
        Some(artifact) => match class_compatible(artifact, host_class) {
            Ok(()) => {
                if let Some(b) = host
                    .backends
                    .iter()
                    .find(|b| b.supports_ready_state_restore)
                {
                    DesktopPlacement::ReadyStateRestore {
                        backend_substrate: b.substrate.clone(),
                    }
                } else if ready_state_enabled {
                    DesktopPlacement::ReadyStateRestoreUnsupportedLocal {
                        reason:
                            "RunnerClass matches but no local Desktop Runner backend can restore \
                             Ready-State yet (cold-OCI only); use a managed runner"
                                .to_string(),
                    }
                } else {
                    cold_or_managed(host)
                }
            }
            Err(mismatch) => DesktopPlacement::SuggestManagedRunner {
                reason: format!(
                    "Ready-State artifact built for a different runner class \
                     (first divergent field: {}); not restorable on {}/{} — use a managed runner",
                    mismatch.first_divergent_field, host.host_os, host.host_arch
                ),
                blockers: Vec::new(),
            },
        },
        None => {
            if ready_state_enabled {
                DesktopPlacement::SuggestManagedRunner {
                    reason:
                        "Ready-State is enabled but this Desktop Runner cannot restore locally \
                         (no sealed artifact for this host class); use a managed runner"
                            .to_string(),
                    blockers: Vec::new(),
                }
            } else {
                cold_or_managed(host)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::desktop_runner::macos::{MacosProbeInputs, build_macos_facts};

    /// A linux/aarch64/firecracker runner class (a plausible Ready-State class).
    fn linux_aarch64_fc() -> RunnerClassFacts {
        RunnerClassFacts {
            os: "linux".into(),
            arch: "aarch64".into(),
            kernel_abi_class: "linux-6.1".into(),
            vmm: "firecracker".into(),
            vmm_version: "1.7.0".into(),
            snapshot_format: "fc-v2".into(),
            cpu_template: Some("T2A".into()),
            cpu_features: vec![],
            guest_kernel_id: "blake3:kern".into(),
            rootfs_base_id: "blake3:rootfs".into(),
            device_profile: "virtio-blk+virtio-net+vsock".into(),
            cgroup: "v2".into(),
            network_model: "tap-nat".into(),
        }
    }

    fn linux_x86_64_fc() -> RunnerClassFacts {
        RunnerClassFacts {
            arch: "x86_64".into(),
            ..linux_aarch64_fc()
        }
    }

    fn mac_facts() -> DesktopRunnerFacts {
        build_macos_facts(
            &MacosProbeInputs {
                host_arch: "aarch64".into(),
                product_version: Some("26.0".into()),
                is_apple_silicon: true,
                container_path: Some("/usr/local/bin/container".into()),
                container_version: Some("container 0.1.0".into()),
                container_service_running: false,
                podman_binary_present: false,
                podman_version: None,
                podman_machine: super::super::macos::PodmanMachineProbe::default(),
            },
            "0.7.0",
        )
    }

    #[test]
    fn exact_runner_class_match_succeeds() {
        let class = linux_aarch64_fc();
        assert!(class_compatible(&class, &class).is_ok());
    }

    #[test]
    fn arch_mismatch_fails_clearly() {
        let artifact = linux_x86_64_fc();
        let host = linux_aarch64_fc();
        let err = class_compatible(&artifact, &host).unwrap_err();
        assert_eq!(err.first_divergent_field, "arch");
    }

    #[test]
    fn linux_x86_64_firecracker_artifact_is_not_selected_for_macos_aarch64() {
        let host = mac_facts();
        // The mac host's *class* is its own host class; the artifact is foreign.
        let host_class = RunnerClassFacts::from_host();
        let placement = select_placement(&host, &host_class, Some(&linux_x86_64_fc()), true);
        match placement {
            DesktopPlacement::SuggestManagedRunner { reason, .. } => {
                assert!(reason.contains("managed runner"), "{reason}");
            }
            other => panic!("must not restore/cold-start a foreign-class artifact: {other:?}"),
        }
    }

    #[test]
    fn managed_runner_suggestion_is_explicit_when_ready_state_enabled_without_artifact() {
        let host = mac_facts();
        let host_class = RunnerClassFacts::from_host();
        let placement = select_placement(&host, &host_class, None, true);
        match placement {
            DesktopPlacement::SuggestManagedRunner { reason, .. } => {
                assert!(reason.contains("managed runner"), "{reason}");
            }
            other => panic!("expected explicit managed-runner suggestion, got {other:?}"),
        }
    }

    #[test]
    fn matching_artifact_with_ready_state_enabled_is_unsupported_local_not_cold() {
        // Exact-class artifact but no restore backend (M2) + Ready-State on:
        // never silently cold-start — surface the local-restore gap explicitly.
        let host = mac_facts();
        let host_class = RunnerClassFacts::from_host();
        let artifact = host_class.clone();
        let placement = select_placement(&host, &host_class, Some(&artifact), true);
        match placement {
            DesktopPlacement::ReadyStateRestoreUnsupportedLocal { reason } => {
                assert!(reason.contains("managed runner"), "{reason}");
            }
            other => panic!("expected ReadyStateRestoreUnsupportedLocal, got {other:?}"),
        }
    }

    #[test]
    fn cold_run_uses_local_backend_when_ready_state_disabled() {
        let host = mac_facts();
        let host_class = RunnerClassFacts::from_host();
        let placement = select_placement(&host, &host_class, None, false);
        match placement {
            DesktopPlacement::ColdOciLocal { backend_substrate } => {
                assert_eq!(
                    backend_substrate,
                    super::super::facts::SUBSTRATE_APPLE_CONTAINERIZATION
                );
            }
            other => panic!("expected local cold OCI, got {other:?}"),
        }
    }

    #[test]
    fn no_local_backend_suggests_managed_runner() {
        // A host with no backends (e.g. container not installed) cannot serve a
        // cold run locally.
        let host = build_macos_facts(
            &MacosProbeInputs {
                host_arch: "aarch64".into(),
                product_version: Some("26.0".into()),
                is_apple_silicon: true,
                container_path: None,
                container_version: None,
                container_service_running: false,
                podman_binary_present: false,
                podman_version: None,
                podman_machine: super::super::macos::PodmanMachineProbe::default(),
            },
            "0.7.0",
        );
        let host_class = RunnerClassFacts::from_host();
        let placement = select_placement(&host, &host_class, None, false);
        match placement {
            DesktopPlacement::SuggestManagedRunner { blockers, .. } => {
                assert!(
                    blockers
                        .iter()
                        .any(|b| b.as_str() == "apple_container_missing"),
                    "blockers should name the missing container tool: {blockers:?}"
                );
            }
            other => panic!("expected SuggestManagedRunner, got {other:?}"),
        }
    }
}
