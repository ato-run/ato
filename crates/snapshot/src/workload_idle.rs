use std::time::Duration;

use protocol::binding_lease::BindingLeaseReceipt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::acceptance::{
    AcceptanceCancellation, CandidateSnapshot, DisposableSessionHandle, VerificationOutcome,
};
use crate::external_state::{ExternalStateAttachmentPlan, SessionExternalStateReceipt};
use crate::snapshot_manifest::{CapturePolicy, SnapshotCatalogRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticBindingSpec {
    pub name: String,
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadIdleConfig {
    pub seal_at_argv: Vec<String>,
    pub timeout: Duration,
    pub synthetic_bindings: Vec<SyntheticBindingSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadIdleAcceptanceReceipt {
    pub snapshot_id: String,
    pub placeholder_revocation_attested: bool,
    pub events: Vec<String>,
    pub disposable_session_destroyed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadIdleRunReceipt {
    pub snapshot_id: String,
    pub events: Vec<String>,
    pub external_state: Vec<SessionExternalStateReceipt>,
    pub binding_leases: Vec<BindingLeaseReceipt>,
    pub exposed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposedWorkloadIdleSession {
    pub session: DisposableSessionHandle,
    pub receipt: WorkloadIdleRunReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkloadIdleError {
    #[error("invalid workload_idle configuration")]
    InvalidConfig,
    #[error("workload_idle lifecycle failed during {phase}")]
    Lifecycle { phase: &'static str },
    #[error("placeholder revocation was not attested")]
    PlaceholderRevocationUnattested,
    #[error("workload_idle operation was cancelled")]
    Cancelled,
    #[error("workload_idle validation failed")]
    ValidationFailed,
    #[error("workload_idle cleanup failed")]
    CleanupFailed,
}

pub trait WorkloadIdleLifecycle {
    fn prepare_synthetic_build_bindings(
        &mut self,
        bindings: &[SyntheticBindingSpec],
    ) -> Result<(), String>;
    fn stop_workload(&mut self, session: Option<&DisposableSessionHandle>) -> Result<(), String>;
    fn revoke_all_bindings(
        &mut self,
        session: Option<&DisposableSessionHandle>,
    ) -> Result<bool, String>;
    fn capture_workload_idle_candidate(&mut self) -> Result<CandidateSnapshot, String>;
    fn create_disposable_session(
        &mut self,
        candidate: &CandidateSnapshot,
    ) -> Result<DisposableSessionHandle, String>;
    fn restore_candidate(
        &mut self,
        session: &DisposableSessionHandle,
        candidate: &CandidateSnapshot,
    ) -> Result<(), String>;
    fn attach_synthetic_validation_bindings(
        &mut self,
        session: &DisposableSessionHandle,
        bindings: &[SyntheticBindingSpec],
    ) -> Result<(), String>;
    fn attach_external_state(
        &mut self,
        session: &DisposableSessionHandle,
        state: &[ExternalStateAttachmentPlan],
    ) -> Result<(), String>;
    fn deliver_real_binding_leases(
        &mut self,
        session: &DisposableSessionHandle,
    ) -> Result<Vec<BindingLeaseReceipt>, String>;
    fn start_workload(&mut self, session: &DisposableSessionHandle) -> Result<(), String>;
    fn verify_exact_argv(
        &mut self,
        session: &DisposableSessionHandle,
        argv: &[String],
        timeout: Duration,
        cancellation: &AcceptanceCancellation,
    ) -> Result<VerificationOutcome, String>;
    fn verify_session_readiness(
        &mut self,
        session: &DisposableSessionHandle,
        timeout: Duration,
        cancellation: &AcceptanceCancellation,
    ) -> Result<(), String>;
    fn expose_session(&mut self, session: &DisposableSessionHandle) -> Result<(), String>;
    fn detach_external_state(&mut self, session: &DisposableSessionHandle) -> Result<(), String>;
    fn destroy_session(&mut self, session: DisposableSessionHandle) -> Result<(), String>;
}

pub fn accept_workload_idle_snapshot(
    lifecycle: &mut impl WorkloadIdleLifecycle,
    config: &WorkloadIdleConfig,
    cancellation: &AcceptanceCancellation,
) -> Result<(SnapshotCatalogRecord, WorkloadIdleAcceptanceReceipt), WorkloadIdleError> {
    validate_config(config)?;
    check_cancelled(cancellation)?;
    lifecycle
        .prepare_synthetic_build_bindings(&config.synthetic_bindings)
        .map_err(|_| WorkloadIdleError::Lifecycle {
            phase: "prepare-synthetic-bindings",
        })?;
    lifecycle
        .stop_workload(None)
        .map_err(|_| WorkloadIdleError::Lifecycle {
            phase: "stop-build-workload",
        })?;
    let revoked =
        lifecycle
            .revoke_all_bindings(None)
            .map_err(|_| WorkloadIdleError::Lifecycle {
                phase: "revoke-placeholders",
            })?;
    if !revoked {
        return Err(WorkloadIdleError::PlaceholderRevocationUnattested);
    }
    check_cancelled(cancellation)?;

    let candidate = lifecycle
        .capture_workload_idle_candidate()
        .map_err(|_| WorkloadIdleError::Lifecycle { phase: "capture" })?;
    candidate
        .manifest
        .validate()
        .map_err(|_| WorkloadIdleError::ValidationFailed)?;
    if candidate.manifest.capture_policy != CapturePolicy::WorkloadIdle {
        return Err(WorkloadIdleError::ValidationFailed);
    }
    if !candidate
        .manifest
        .sanitization_attestation
        .steps
        .iter()
        .any(|step| step == "revoke-bindings")
    {
        return Err(WorkloadIdleError::PlaceholderRevocationUnattested);
    }

    let session = lifecycle
        .create_disposable_session(&candidate)
        .map_err(|_| WorkloadIdleError::Lifecycle {
            phase: "create-disposable-session",
        })?;
    let mut events = vec![
        "prepare_synthetic_bindings".to_string(),
        "stop_workload".to_string(),
        "revoke_placeholders".to_string(),
        "capture_candidate".to_string(),
        "create_disposable_session".to_string(),
    ];

    let result = (|| {
        lifecycle
            .restore_candidate(&session, &candidate)
            .map_err(|_| WorkloadIdleError::Lifecycle { phase: "restore" })?;
        events.push("restore_candidate".to_string());
        check_cancelled(cancellation)?;
        lifecycle
            .attach_synthetic_validation_bindings(&session, &config.synthetic_bindings)
            .map_err(|_| WorkloadIdleError::Lifecycle {
                phase: "attach-synthetic-validation-bindings",
            })?;
        events.push("attach_synthetic_validation_bindings".to_string());
        lifecycle
            .start_workload(&session)
            .map_err(|_| WorkloadIdleError::Lifecycle {
                phase: "start-validation-workload",
            })?;
        events.push("start_workload".to_string());
        let outcome = lifecycle
            .verify_exact_argv(&session, &config.seal_at_argv, config.timeout, cancellation)
            .map_err(|_| WorkloadIdleError::ValidationFailed)?;
        events.push("verify_seal_at".to_string());
        if outcome != VerificationOutcome::Exited(0) || cancellation.is_cancelled() {
            return Err(if cancellation.is_cancelled() {
                WorkloadIdleError::Cancelled
            } else {
                WorkloadIdleError::ValidationFailed
            });
        }
        Ok(())
    })();

    let cleanup = cleanup_disposable(lifecycle, session);
    events.extend([
        "stop_validation_workload".to_string(),
        "revoke_validation_bindings".to_string(),
        "detach_validation_state".to_string(),
        "destroy_disposable_session".to_string(),
    ]);
    cleanup?;
    result?;

    let receipt = WorkloadIdleAcceptanceReceipt {
        snapshot_id: candidate.manifest.snapshot_id.clone(),
        placeholder_revocation_attested: true,
        events,
        disposable_session_destroyed: true,
    };
    Ok((SnapshotCatalogRecord::accepted(candidate.manifest), receipt))
}

pub fn restore_workload_idle_session(
    lifecycle: &mut impl WorkloadIdleLifecycle,
    snapshot: &SnapshotCatalogRecord,
    external_state: &[ExternalStateAttachmentPlan],
    timeout: Duration,
    cancellation: &AcceptanceCancellation,
) -> Result<ExposedWorkloadIdleSession, WorkloadIdleError> {
    if snapshot.manifest.capture_policy != CapturePolicy::WorkloadIdle
        || !matches!(
            snapshot.status,
            crate::snapshot_manifest::SnapshotCatalogStatus::Accepted
        )
        || timeout.is_zero()
    {
        return Err(WorkloadIdleError::InvalidConfig);
    }
    snapshot
        .manifest
        .validate()
        .map_err(|_| WorkloadIdleError::ValidationFailed)?;
    check_cancelled(cancellation)?;

    let candidate = CandidateSnapshot {
        manifest: snapshot.manifest.clone(),
    };
    let session = lifecycle
        .create_disposable_session(&candidate)
        .map_err(|_| WorkloadIdleError::Lifecycle {
            phase: "create-restored-session",
        })?;
    let mut events = vec!["restore_session".to_string()];
    let result = (|| {
        lifecycle
            .restore_candidate(&session, &candidate)
            .map_err(|_| WorkloadIdleError::Lifecycle { phase: "restore" })?;
        check_cancelled(cancellation)?;
        lifecycle
            .attach_external_state(&session, external_state)
            .map_err(|_| WorkloadIdleError::Lifecycle {
                phase: "attach-external-state",
            })?;
        events.push("attach_external_state".to_string());
        let binding_leases = lifecycle
            .deliver_real_binding_leases(&session)
            .map_err(|_| WorkloadIdleError::Lifecycle {
                phase: "deliver-binding-leases",
            })?;
        events.push("deliver_binding_leases".to_string());
        lifecycle
            .start_workload(&session)
            .map_err(|_| WorkloadIdleError::Lifecycle {
                phase: "start-workload",
            })?;
        events.push("start_workload".to_string());
        lifecycle
            .verify_session_readiness(&session, timeout, cancellation)
            .map_err(|_| WorkloadIdleError::ValidationFailed)?;
        events.push("verify_readiness".to_string());
        check_cancelled(cancellation)?;
        lifecycle
            .expose_session(&session)
            .map_err(|_| WorkloadIdleError::Lifecycle { phase: "expose" })?;
        events.push("expose_session".to_string());
        Ok(binding_leases)
    })();

    match result {
        Ok(binding_leases) => Ok(ExposedWorkloadIdleSession {
            receipt: WorkloadIdleRunReceipt {
                snapshot_id: snapshot.manifest.snapshot_id.clone(),
                events,
                external_state: external_state.iter().map(|state| state.receipt()).collect(),
                binding_leases,
                exposed: true,
            },
            session,
        }),
        Err(error) => {
            cleanup_disposable(lifecycle, session)?;
            Err(error)
        }
    }
}

fn cleanup_disposable(
    lifecycle: &mut impl WorkloadIdleLifecycle,
    session: DisposableSessionHandle,
) -> Result<(), WorkloadIdleError> {
    let mut failed = false;
    failed |= lifecycle.stop_workload(Some(&session)).is_err();
    failed |= lifecycle.revoke_all_bindings(Some(&session)).is_err();
    failed |= lifecycle.detach_external_state(&session).is_err();
    failed |= lifecycle.destroy_session(session).is_err();
    if failed {
        Err(WorkloadIdleError::CleanupFailed)
    } else {
        Ok(())
    }
}

fn validate_config(config: &WorkloadIdleConfig) -> Result<(), WorkloadIdleError> {
    if config.timeout.is_zero()
        || config.seal_at_argv.is_empty()
        || config
            .seal_at_argv
            .iter()
            .any(|argument| argument.is_empty() || argument.contains('\0'))
        || config
            .synthetic_bindings
            .iter()
            .any(|binding| binding.name.is_empty() || binding.schema.is_empty())
    {
        return Err(WorkloadIdleError::InvalidConfig);
    }
    Ok(())
}

fn check_cancelled(cancellation: &AcceptanceCancellation) -> Result<(), WorkloadIdleError> {
    if cancellation.is_cancelled() {
        Err(WorkloadIdleError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::ExecutionId;
    use capsulefs::{BlobManifest, ChunkingKind, LayerKind};

    use super::*;
    use crate::manifest::RestoreContract;
    use crate::snapshot_manifest::{
        CaptureProvenance, SanitizationAttestation, SecretScanAttestation,
        SnapshotCompatibilityContract, SnapshotLayerRefs, SnapshotManifestV1,
    };

    #[derive(Default)]
    struct MockLifecycle {
        calls: Vec<&'static str>,
        revoke_attested: bool,
        fail_readiness: bool,
    }

    impl MockLifecycle {
        fn candidate(&self) -> CandidateSnapshot {
            CandidateSnapshot {
                manifest: SnapshotManifestV1::new(
                    ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap(),
                    SnapshotCompatibilityContract {
                        backend: "fake".to_string(),
                        format: "fake-v1".to_string(),
                        vmm_version: "1".to_string(),
                        kernel_digest: "sha256:kernel".to_string(),
                        cpu_template: None,
                        codec: "raw".to_string(),
                        runner_contract: "runner/v1".to_string(),
                    },
                    SnapshotLayerRefs {
                        memory: Some(BlobManifest::new(
                            LayerKind::Memory,
                            0,
                            ChunkingKind::ContentDefined,
                            Vec::new(),
                        )),
                        vmstate: None,
                        disk_layers: Vec::new(),
                    },
                    RestoreContract::default(),
                    CapturePolicy::WorkloadIdle,
                    CaptureProvenance {
                        builder: "test".to_string(),
                        build_receipt_id: None,
                        capsule_manifest_hash: None,
                    },
                    SanitizationAttestation {
                        policy: "placeholder-revocation/v1".to_string(),
                        steps: vec!["stop-workload".to_string(), "revoke-bindings".to_string()],
                    },
                    SecretScanAttestation {
                        scanner: "test".to_string(),
                        findings: 0,
                        redacted_summary: None,
                    },
                )
                .unwrap(),
            }
        }
    }

    impl WorkloadIdleLifecycle for MockLifecycle {
        fn prepare_synthetic_build_bindings(
            &mut self,
            _: &[SyntheticBindingSpec],
        ) -> Result<(), String> {
            self.calls.push("prepare");
            Ok(())
        }
        fn stop_workload(&mut self, _: Option<&DisposableSessionHandle>) -> Result<(), String> {
            self.calls.push("stop");
            Ok(())
        }
        fn revoke_all_bindings(
            &mut self,
            _: Option<&DisposableSessionHandle>,
        ) -> Result<bool, String> {
            self.calls.push("revoke");
            Ok(self.revoke_attested)
        }
        fn capture_workload_idle_candidate(&mut self) -> Result<CandidateSnapshot, String> {
            self.calls.push("capture");
            Ok(self.candidate())
        }
        fn create_disposable_session(
            &mut self,
            _: &CandidateSnapshot,
        ) -> Result<DisposableSessionHandle, String> {
            self.calls.push("create");
            Ok(DisposableSessionHandle {
                opaque_id: "session".to_string(),
            })
        }
        fn restore_candidate(
            &mut self,
            _: &DisposableSessionHandle,
            _: &CandidateSnapshot,
        ) -> Result<(), String> {
            self.calls.push("restore");
            Ok(())
        }
        fn attach_synthetic_validation_bindings(
            &mut self,
            _: &DisposableSessionHandle,
            _: &[SyntheticBindingSpec],
        ) -> Result<(), String> {
            self.calls.push("attach_synthetic");
            Ok(())
        }
        fn attach_external_state(
            &mut self,
            _: &DisposableSessionHandle,
            _: &[ExternalStateAttachmentPlan],
        ) -> Result<(), String> {
            self.calls.push("attach_state");
            Ok(())
        }
        fn deliver_real_binding_leases(
            &mut self,
            _: &DisposableSessionHandle,
        ) -> Result<Vec<BindingLeaseReceipt>, String> {
            self.calls.push("deliver");
            Ok(Vec::new())
        }
        fn start_workload(&mut self, _: &DisposableSessionHandle) -> Result<(), String> {
            self.calls.push("start");
            Ok(())
        }
        fn verify_exact_argv(
            &mut self,
            _: &DisposableSessionHandle,
            _: &[String],
            _: Duration,
            _: &AcceptanceCancellation,
        ) -> Result<VerificationOutcome, String> {
            self.calls.push("seal");
            Ok(VerificationOutcome::Exited(0))
        }
        fn verify_session_readiness(
            &mut self,
            _: &DisposableSessionHandle,
            _: Duration,
            _: &AcceptanceCancellation,
        ) -> Result<(), String> {
            self.calls.push("ready");
            if self.fail_readiness {
                Err("not ready".to_string())
            } else {
                Ok(())
            }
        }
        fn expose_session(&mut self, _: &DisposableSessionHandle) -> Result<(), String> {
            self.calls.push("expose");
            Ok(())
        }
        fn detach_external_state(&mut self, _: &DisposableSessionHandle) -> Result<(), String> {
            self.calls.push("detach");
            Ok(())
        }
        fn destroy_session(&mut self, _: DisposableSessionHandle) -> Result<(), String> {
            self.calls.push("destroy");
            Ok(())
        }
    }

    fn config() -> WorkloadIdleConfig {
        WorkloadIdleConfig {
            seal_at_argv: vec!["verify".to_string()],
            timeout: Duration::from_secs(1),
            synthetic_bindings: vec![SyntheticBindingSpec {
                name: "database".to_string(),
                schema: "1".to_string(),
            }],
        }
    }

    #[test]
    fn acceptance_stops_then_revokes_before_capture_and_cleans_disposable() {
        let mut lifecycle = MockLifecycle {
            revoke_attested: true,
            ..Default::default()
        };
        let (_, receipt) = accept_workload_idle_snapshot(
            &mut lifecycle,
            &config(),
            &AcceptanceCancellation::default(),
        )
        .unwrap();
        assert_eq!(&lifecycle.calls[..3], &["prepare", "stop", "revoke"]);
        assert_eq!(lifecycle.calls[3], "capture");
        assert!(receipt.placeholder_revocation_attested);
        assert_eq!(
            &lifecycle.calls[lifecycle.calls.len() - 4..],
            &["stop", "revoke", "detach", "destroy"]
        );
    }

    #[test]
    fn unattested_revocation_blocks_capture() {
        let mut lifecycle = MockLifecycle::default();
        assert_eq!(
            accept_workload_idle_snapshot(
                &mut lifecycle,
                &config(),
                &AcceptanceCancellation::default()
            )
            .unwrap_err(),
            WorkloadIdleError::PlaceholderRevocationUnattested
        );
        assert!(!lifecycle.calls.contains(&"capture"));
    }

    #[test]
    fn real_restore_attaches_before_start_and_expose() {
        let mut lifecycle = MockLifecycle {
            revoke_attested: true,
            ..Default::default()
        };
        let (snapshot, _) = accept_workload_idle_snapshot(
            &mut lifecycle,
            &config(),
            &AcceptanceCancellation::default(),
        )
        .unwrap();
        lifecycle.calls.clear();
        let exposed = restore_workload_idle_session(
            &mut lifecycle,
            &snapshot,
            &[],
            Duration::from_secs(1),
            &AcceptanceCancellation::default(),
        )
        .unwrap();
        assert_eq!(
            lifecycle.calls,
            vec![
                "create",
                "restore",
                "attach_state",
                "deliver",
                "start",
                "ready",
                "expose"
            ]
        );
        assert!(exposed.receipt.exposed);
    }

    #[test]
    fn readiness_failure_cleans_every_runtime_resource() {
        let mut lifecycle = MockLifecycle {
            revoke_attested: true,
            ..Default::default()
        };
        let (snapshot, _) = accept_workload_idle_snapshot(
            &mut lifecycle,
            &config(),
            &AcceptanceCancellation::default(),
        )
        .unwrap();
        lifecycle.calls.clear();
        lifecycle.fail_readiness = true;
        assert_eq!(
            restore_workload_idle_session(
                &mut lifecycle,
                &snapshot,
                &[],
                Duration::from_secs(1),
                &AcceptanceCancellation::default(),
            )
            .unwrap_err(),
            WorkloadIdleError::ValidationFailed
        );
        assert_eq!(
            &lifecycle.calls[lifecycle.calls.len() - 4..],
            &["stop", "revoke", "detach", "destroy"]
        );
    }
}
