use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::snapshot_manifest::{CapturePolicy, SnapshotCatalogRecord, SnapshotManifestV1};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSnapshot {
    pub manifest: SnapshotManifestV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableSessionHandle {
    pub opaque_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEligibility {
    pub external_state_required_by_live_workload: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceConfig {
    pub seal_at_argv: Vec<String>,
    pub verification_timeout: Duration,
    pub total_deadline: Duration,
    pub maximum_attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationOutcome {
    Exited(i32),
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct AcceptanceCancellation(Arc<AtomicBool>);

impl AcceptanceCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceAttemptReceipt {
    pub attempt: u32,
    pub candidate_snapshot_id: Option<String>,
    pub outcome: String,
    pub process_tree_terminated: bool,
    pub disposable_session_destroyed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceReceipt {
    pub capture_policy: CapturePolicy,
    pub maximum_attempts: u32,
    pub attempts: Vec<AcceptanceAttemptReceipt>,
    pub accepted_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceResult {
    pub snapshot: SnapshotCatalogRecord,
    pub receipt: AcceptanceReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AcceptanceError {
    #[error("running Snapshot is ineligible because the live workload requires External State")]
    ExternalStateRequiresWorkloadIdle,
    #[error("invalid Snapshot acceptance configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("Snapshot acceptance was cancelled")]
    Cancelled,
    #[error("Snapshot candidate lifecycle failed during {phase}")]
    Lifecycle { phase: &'static str },
    #[error("disposable Session cleanup failed")]
    Cleanup,
    #[error("Snapshot candidate was not accepted within the configured attempts and deadline")]
    Exhausted { receipt: AcceptanceReceipt },
}

pub trait DisposableAcceptanceLifecycle {
    fn capture_candidate(&mut self, attempt: u32) -> Result<CandidateSnapshot, String>;

    /// Creates an isolated Session with no production secrets, user state, or
    /// Ato user identity attached. Implementations must treat that boundary as
    /// part of their security contract.
    fn create_disposable_session(
        &mut self,
        candidate: &CandidateSnapshot,
    ) -> Result<DisposableSessionHandle, String>;

    fn restore_candidate(
        &mut self,
        session: &DisposableSessionHandle,
        candidate: &CandidateSnapshot,
    ) -> Result<(), String>;

    fn execute_exact_argv(
        &mut self,
        session: &DisposableSessionHandle,
        argv: &[String],
        timeout: Duration,
        cancellation: &AcceptanceCancellation,
    ) -> Result<VerificationOutcome, String>;

    fn terminate_process_tree(&mut self, session: &DisposableSessionHandle) -> Result<(), String>;

    fn destroy_disposable_session(
        &mut self,
        session: DisposableSessionHandle,
    ) -> Result<(), String>;
}

pub struct RunningSnapshotAcceptance;

impl RunningSnapshotAcceptance {
    pub fn accept(
        lifecycle: &mut impl DisposableAcceptanceLifecycle,
        eligibility: SnapshotEligibility,
        config: &AcceptanceConfig,
        cancellation: &AcceptanceCancellation,
    ) -> Result<AcceptanceResult, AcceptanceError> {
        validate_config(config)?;
        if eligibility.external_state_required_by_live_workload {
            return Err(AcceptanceError::ExternalStateRequiresWorkloadIdle);
        }

        let started = Instant::now();
        let mut receipt = AcceptanceReceipt {
            capture_policy: CapturePolicy::Running,
            maximum_attempts: config.maximum_attempts,
            attempts: Vec::new(),
            accepted_snapshot_id: None,
        };

        for attempt in 1..=config.maximum_attempts {
            if cancellation.is_cancelled() {
                return Err(AcceptanceError::Cancelled);
            }
            if started.elapsed() >= config.total_deadline {
                break;
            }

            let candidate = match lifecycle.capture_candidate(attempt) {
                Ok(candidate) => candidate,
                Err(_) => {
                    receipt.attempts.push(AcceptanceAttemptReceipt {
                        attempt,
                        candidate_snapshot_id: None,
                        outcome: "capture-failed".to_string(),
                        process_tree_terminated: false,
                        disposable_session_destroyed: false,
                    });
                    continue;
                }
            };
            candidate
                .manifest
                .validate()
                .map_err(|_| AcceptanceError::Lifecycle {
                    phase: "candidate-validation",
                })?;
            if candidate.manifest.capture_policy != CapturePolicy::Running {
                return Err(AcceptanceError::Lifecycle {
                    phase: "capture-policy",
                });
            }

            let mut attempt_receipt = AcceptanceAttemptReceipt {
                attempt,
                candidate_snapshot_id: Some(candidate.manifest.snapshot_id.clone()),
                outcome: "restore-failed".to_string(),
                process_tree_terminated: false,
                disposable_session_destroyed: false,
            };
            let session = match lifecycle.create_disposable_session(&candidate) {
                Ok(session) => session,
                Err(_) => {
                    attempt_receipt.outcome = "create-session-failed".to_string();
                    receipt.attempts.push(attempt_receipt);
                    continue;
                }
            };

            let (accepted, terminate_required) =
                if lifecycle.restore_candidate(&session, &candidate).is_err() {
                    attempt_receipt.outcome = "restore-failed".to_string();
                    (false, false)
                } else {
                    match lifecycle.execute_exact_argv(
                        &session,
                        &config.seal_at_argv,
                        config.verification_timeout,
                        cancellation,
                    ) {
                        Ok(VerificationOutcome::Exited(_)) if cancellation.is_cancelled() => {
                            attempt_receipt.outcome = "cancelled".to_string();
                            (false, true)
                        }
                        Ok(VerificationOutcome::Exited(0))
                            if started.elapsed() < config.total_deadline =>
                        {
                            attempt_receipt.outcome = "accepted".to_string();
                            (true, false)
                        }
                        Ok(VerificationOutcome::Exited(0)) => {
                            attempt_receipt.outcome = "deadline-exceeded".to_string();
                            (false, false)
                        }
                        Ok(VerificationOutcome::Exited(_)) => {
                            attempt_receipt.outcome = "nonzero-exit".to_string();
                            (false, false)
                        }
                        Ok(VerificationOutcome::TimedOut) => {
                            attempt_receipt.outcome = "timeout".to_string();
                            (false, true)
                        }
                        Ok(VerificationOutcome::Cancelled) => {
                            attempt_receipt.outcome = "cancelled".to_string();
                            (false, true)
                        }
                        Err(_) => {
                            attempt_receipt.outcome = "verification-error".to_string();
                            (false, true)
                        }
                    }
                };

            // Destruction is unconditional after Session creation. In particular,
            // a failed process-tree termination must not leak the disposable VM.
            let mut cleanup_failed = false;
            if terminate_required {
                match lifecycle.terminate_process_tree(&session) {
                    Ok(()) => attempt_receipt.process_tree_terminated = true,
                    Err(_) => cleanup_failed = true,
                }
            }
            match lifecycle.destroy_disposable_session(session) {
                Ok(()) => attempt_receipt.disposable_session_destroyed = true,
                Err(_) => cleanup_failed = true,
            }
            receipt.attempts.push(attempt_receipt);

            if cleanup_failed {
                return Err(AcceptanceError::Cleanup);
            }

            if accepted {
                receipt.accepted_snapshot_id = Some(candidate.manifest.snapshot_id.clone());
                return Ok(AcceptanceResult {
                    snapshot: SnapshotCatalogRecord::accepted(candidate.manifest),
                    receipt,
                });
            }
            if cancellation.is_cancelled() {
                return Err(AcceptanceError::Cancelled);
            }
        }

        Err(AcceptanceError::Exhausted { receipt })
    }
}

fn validate_config(config: &AcceptanceConfig) -> Result<(), AcceptanceError> {
    if config.maximum_attempts == 0 {
        return Err(AcceptanceError::InvalidConfig(
            "maximum_attempts must be positive",
        ));
    }
    if config.verification_timeout.is_zero() {
        return Err(AcceptanceError::InvalidConfig(
            "verification_timeout must be positive",
        ));
    }
    if config.total_deadline < config.verification_timeout {
        return Err(AcceptanceError::InvalidConfig(
            "total_deadline must cover one verification timeout",
        ));
    }
    if config.seal_at_argv.is_empty()
        || config
            .seal_at_argv
            .iter()
            .any(|arg| arg.is_empty() || arg.contains('\0'))
    {
        return Err(AcceptanceError::InvalidConfig(
            "seal_at argv must be non-empty exact arguments without NUL",
        ));
    }
    Ok(())
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

    struct MockLifecycle {
        outcomes: Vec<VerificationOutcome>,
        captures: u32,
        destroys: u32,
        terminates: u32,
        executed_argv: Vec<Vec<String>>,
        restore_fails: bool,
        terminate_fails: bool,
        execute_delay: Duration,
    }

    impl MockLifecycle {
        fn new(outcomes: Vec<VerificationOutcome>) -> Self {
            Self {
                outcomes,
                captures: 0,
                destroys: 0,
                terminates: 0,
                executed_argv: Vec::new(),
                restore_fails: false,
                terminate_fails: false,
                execute_delay: Duration::ZERO,
            }
        }

        fn manifest(&self) -> SnapshotManifestV1 {
            SnapshotManifestV1::new(
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
                CapturePolicy::Running,
                CaptureProvenance {
                    builder: "test".to_string(),
                    build_receipt_id: None,
                    capsule_manifest_hash: None,
                },
                SanitizationAttestation {
                    policy: "test".to_string(),
                    steps: Vec::new(),
                },
                SecretScanAttestation {
                    scanner: "test".to_string(),
                    findings: 0,
                    redacted_summary: None,
                },
            )
            .unwrap()
        }
    }

    impl DisposableAcceptanceLifecycle for MockLifecycle {
        fn capture_candidate(&mut self, _attempt: u32) -> Result<CandidateSnapshot, String> {
            self.captures += 1;
            Ok(CandidateSnapshot {
                manifest: self.manifest(),
            })
        }

        fn create_disposable_session(
            &mut self,
            _candidate: &CandidateSnapshot,
        ) -> Result<DisposableSessionHandle, String> {
            Ok(DisposableSessionHandle {
                opaque_id: format!("session-{}", self.captures),
            })
        }

        fn restore_candidate(
            &mut self,
            _session: &DisposableSessionHandle,
            _candidate: &CandidateSnapshot,
        ) -> Result<(), String> {
            if self.restore_fails {
                Err("restore failed".to_string())
            } else {
                Ok(())
            }
        }

        fn execute_exact_argv(
            &mut self,
            _session: &DisposableSessionHandle,
            argv: &[String],
            _timeout: Duration,
            _cancellation: &AcceptanceCancellation,
        ) -> Result<VerificationOutcome, String> {
            self.executed_argv.push(argv.to_vec());
            std::thread::sleep(self.execute_delay);
            Ok(self.outcomes.remove(0))
        }

        fn terminate_process_tree(
            &mut self,
            _session: &DisposableSessionHandle,
        ) -> Result<(), String> {
            self.terminates += 1;
            if self.terminate_fails {
                Err("termination failed".to_string())
            } else {
                Ok(())
            }
        }

        fn destroy_disposable_session(
            &mut self,
            _session: DisposableSessionHandle,
        ) -> Result<(), String> {
            self.destroys += 1;
            Ok(())
        }
    }

    fn config(maximum_attempts: u32) -> AcceptanceConfig {
        AcceptanceConfig {
            seal_at_argv: vec![
                "npm".to_string(),
                "run".to_string(),
                "verify-ready".to_string(),
            ],
            verification_timeout: Duration::from_secs(1),
            total_deadline: Duration::from_secs(10),
            maximum_attempts,
        }
    }

    #[test]
    fn accepts_only_exit_zero_and_preserves_exact_argv() {
        let mut lifecycle = MockLifecycle::new(vec![
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(0),
        ]);
        let result = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            SnapshotEligibility {
                external_state_required_by_live_workload: false,
            },
            &config(2),
            &AcceptanceCancellation::default(),
        )
        .unwrap();

        assert_eq!(lifecycle.captures, 2);
        assert_eq!(lifecycle.destroys, 2);
        assert_eq!(lifecycle.executed_argv, vec![config(2).seal_at_argv; 2]);
        assert_eq!(result.receipt.attempts.len(), 2);
    }

    #[test]
    fn timeout_terminates_process_tree_and_destroys_session() {
        let mut lifecycle = MockLifecycle::new(vec![VerificationOutcome::TimedOut]);
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            SnapshotEligibility {
                external_state_required_by_live_workload: false,
            },
            &config(1),
            &AcceptanceCancellation::default(),
        )
        .unwrap_err();

        assert!(matches!(error, AcceptanceError::Exhausted { .. }));
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1);
    }

    #[test]
    fn restore_failure_is_explicit_and_still_destroys_session() {
        let mut lifecycle = MockLifecycle::new(Vec::new());
        lifecycle.restore_fails = true;
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            SnapshotEligibility {
                external_state_required_by_live_workload: false,
            },
            &config(1),
            &AcceptanceCancellation::default(),
        )
        .unwrap_err();

        let AcceptanceError::Exhausted { receipt } = error else {
            panic!("expected exhausted receipt");
        };
        assert_eq!(receipt.attempts[0].outcome, "restore-failed");
        assert!(lifecycle.executed_argv.is_empty());
        assert_eq!(lifecycle.destroys, 1);
    }

    #[test]
    fn termination_failure_does_not_skip_disposable_session_destroy() {
        let mut lifecycle = MockLifecycle::new(vec![VerificationOutcome::TimedOut]);
        lifecycle.terminate_fails = true;
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            SnapshotEligibility {
                external_state_required_by_live_workload: false,
            },
            &config(1),
            &AcceptanceCancellation::default(),
        )
        .unwrap_err();

        assert_eq!(error, AcceptanceError::Cleanup);
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1);
    }

    #[test]
    fn successful_verification_after_deadline_is_not_reported_as_nonzero_exit() {
        let mut lifecycle = MockLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        lifecycle.execute_delay = Duration::from_millis(5);
        let deadline_config = AcceptanceConfig {
            verification_timeout: Duration::from_millis(1),
            total_deadline: Duration::from_millis(1),
            ..config(1)
        };
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            SnapshotEligibility {
                external_state_required_by_live_workload: false,
            },
            &deadline_config,
            &AcceptanceCancellation::default(),
        )
        .unwrap_err();

        let AcceptanceError::Exhausted { receipt } = error else {
            panic!("expected exhausted receipt");
        };
        assert_eq!(receipt.attempts[0].outcome, "deadline-exceeded");
    }

    #[test]
    fn external_state_required_running_snapshot_fails_before_capture() {
        let mut lifecycle = MockLifecycle::new(Vec::new());
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            SnapshotEligibility {
                external_state_required_by_live_workload: true,
            },
            &config(1),
            &AcceptanceCancellation::default(),
        )
        .unwrap_err();

        assert_eq!(error, AcceptanceError::ExternalStateRequiresWorkloadIdle);
        assert_eq!(lifecycle.captures, 0);
    }

    #[test]
    fn pre_cancelled_acceptance_creates_no_resources() {
        let cancellation = AcceptanceCancellation::default();
        cancellation.cancel();
        let mut lifecycle = MockLifecycle::new(Vec::new());
        assert_eq!(
            RunningSnapshotAcceptance::accept(
                &mut lifecycle,
                SnapshotEligibility {
                    external_state_required_by_live_workload: false,
                },
                &config(1),
                &cancellation,
            )
            .unwrap_err(),
            AcceptanceError::Cancelled
        );
        assert_eq!(lifecycle.captures, 0);
    }
}
