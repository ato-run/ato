use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ato_lock::{self, AtoLock};
use crate::execution_contract::{ExecutionContractV1, ExecutionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdReconstructionPolicy {
    Allow,
    Forbid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructionEvidence {
    pub reconstructed_contract: ExecutionContractV1,
    pub verifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedColdLaunch {
    execution_id: ExecutionId,
    contract: ExecutionContractV1,
    verifier: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    Cold,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdReconstructionReceipt {
    pub session_id: String,
    pub execution_id: ExecutionId,
    pub launch_mode: LaunchMode,
    pub verifier: String,
    pub reconstructed_contract_digest: ExecutionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ColdReconstructionError {
    #[error("cold reconstruction is forbidden by deployment policy")]
    ForbiddenByPolicy,
    #[error("ato.lock.json failed persisted identity validation")]
    InvalidLock,
    #[error("ato.lock.json has no Capsule v1 execution contract")]
    MissingExecutionContract,
    #[error("cold reconstruction failed before External State attachment")]
    ReconstructionFailed,
    #[error("cold reconstruction does not match the stored Execution Identity")]
    ContractMismatch,
    #[error("verified cold Session failed to start")]
    SessionStartFailed,
}

pub trait ColdReconstructionExecutor {
    /// Reconstructs runtime, dependencies, build outputs, filesystem,
    /// environment, policies, launch data, and guest surface without attaching
    /// External State or starting a Session.
    fn reconstruct_launch_contract(
        &mut self,
        expected: &ExecutionContractV1,
    ) -> Result<ReconstructionEvidence, String>;

    /// Starts only an already verified launch. Callers attach compatible
    /// External State between verification and this method.
    fn start_verified_session(&mut self, launch: &VerifiedColdLaunch) -> Result<String, String>;
}

impl VerifiedColdLaunch {
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    pub fn contract(&self) -> &ExecutionContractV1 {
        &self.contract
    }
}

pub struct ColdReconstruction;

impl ColdReconstruction {
    pub fn ensure_policy(policy: ColdReconstructionPolicy) -> Result<(), ColdReconstructionError> {
        if policy == ColdReconstructionPolicy::Forbid {
            return Err(ColdReconstructionError::ForbiddenByPolicy);
        }
        Ok(())
    }

    pub fn reconstruct_and_verify(
        lock: &AtoLock,
        policy: ColdReconstructionPolicy,
        executor: &mut impl ColdReconstructionExecutor,
    ) -> Result<VerifiedColdLaunch, ColdReconstructionError> {
        Self::ensure_policy(policy)?;
        ato_lock::validate_persisted_strict(lock)
            .map_err(|_| ColdReconstructionError::InvalidLock)?;
        let expected = lock
            .execution_contract
            .as_ref()
            .ok_or(ColdReconstructionError::MissingExecutionContract)?;
        let expected_id = lock
            .execution_id
            .as_ref()
            .ok_or(ColdReconstructionError::MissingExecutionContract)?;

        let evidence = executor
            .reconstruct_launch_contract(expected)
            .map_err(|_| ColdReconstructionError::ReconstructionFailed)?;
        let reconstructed_id = evidence
            .reconstructed_contract
            .compute_execution_id()
            .map_err(|_| ColdReconstructionError::ContractMismatch)?;
        if &reconstructed_id != expected_id || evidence.reconstructed_contract != *expected {
            return Err(ColdReconstructionError::ContractMismatch);
        }

        Ok(VerifiedColdLaunch {
            execution_id: expected_id.clone(),
            contract: evidence.reconstructed_contract,
            verifier: evidence.verifier,
        })
    }

    pub fn start_session(
        launch: VerifiedColdLaunch,
        executor: &mut impl ColdReconstructionExecutor,
    ) -> Result<ColdReconstructionReceipt, ColdReconstructionError> {
        let session_id = executor
            .start_verified_session(&launch)
            .map_err(|_| ColdReconstructionError::SessionStartFailed)?;
        if session_id.trim().is_empty() {
            return Err(ColdReconstructionError::SessionStartFailed);
        }
        let reconstructed_contract_digest = launch
            .contract
            .compute_execution_id()
            .map_err(|_| ColdReconstructionError::ContractMismatch)?;
        Ok(ColdReconstructionReceipt {
            session_id,
            execution_id: launch.execution_id,
            launch_mode: LaunchMode::Cold,
            verifier: launch.verifier,
            reconstructed_contract_digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ato_lock;
    use crate::execution_contract::{
        ContentDigest, DigestAlgorithm, EXECUTION_CONTRACT_V1_SCHEMA, EnvironmentVariableContract,
        ExternalStateAccess, ExternalStateContract, GuestSurfaceContract, ResolvedArtifactContract,
        ResolvedBuildOutputContract, ResolvedDependencyContract, ResolvedFilesystemContract,
        ResolvedLaunchContract, ResolvedPolicyContract, ResolvedSourceContract,
        ResolvedTargetContract, SnapshotExclusion,
    };

    fn digest(algorithm: DigestAlgorithm, byte: u8) -> ContentDigest {
        ContentDigest::new(algorithm, [byte; 32])
    }

    fn contract() -> ExecutionContractV1 {
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                kind: "git".to_string(),
                immutable_ref: "repo@commit".to_string(),
                digest: digest(DigestAlgorithm::Sha256, 1),
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: Some("glibc".to_string()),
                observable_features: BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "node".to_string(),
                resolved_ref: "node@22".to_string(),
                digest: digest(DigestAlgorithm::Sha256, 2),
            },
            dependencies: vec![ResolvedDependencyContract {
                name: "npm".to_string(),
                derivation_digest: digest(DigestAlgorithm::Blake3, 3),
                output_digest: digest(DigestAlgorithm::Blake3, 4),
            }],
            build_outputs: vec![ResolvedBuildOutputContract {
                name: "app".to_string(),
                digest: digest(DigestAlgorithm::Blake3, 5),
            }],
            launch: ResolvedLaunchContract {
                argv: vec!["node".to_string(), "app.js".to_string()],
                cwd: "/workspace".to_string(),
                environment: vec![EnvironmentVariableContract {
                    name: "NODE_ENV".to_string(),
                    value_digest: digest(DigestAlgorithm::Blake3, 6),
                }],
                secret_bindings: vec!["TOKEN".to_string()],
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: digest(DigestAlgorithm::Blake3, 7),
                readonly_layers: Vec::new(),
                writable_paths: vec!["/tmp".to_string()],
            },
            policy: ResolvedPolicyContract {
                network_digest: digest(DigestAlgorithm::Blake3, 8),
                capability_digest: digest(DigestAlgorithm::Blake3, 9),
                filesystem_digest: digest(DigestAlgorithm::Blake3, 10),
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "guest/v1".to_string(),
                port: Some(8080),
                features: vec!["exec".to_string()],
            },
            external_state: vec![ExternalStateContract {
                name: "data".to_string(),
                target: "/data".to_string(),
                access: ExternalStateAccess::ReadWrite,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            }],
        }
    }

    fn lock() -> AtoLock {
        let contract = contract();
        let execution_id = contract.compute_execution_id().unwrap();
        let mut lock = AtoLock {
            execution_contract: Some(contract),
            execution_id: Some(execution_id),
            ..AtoLock::default()
        };
        ato_lock::recompute_lock_id(&mut lock).unwrap();
        lock
    }

    struct Executor {
        reconstructed: ExecutionContractV1,
        reconstructs: u32,
        starts: u32,
    }

    impl ColdReconstructionExecutor for Executor {
        fn reconstruct_launch_contract(
            &mut self,
            _expected: &ExecutionContractV1,
        ) -> Result<ReconstructionEvidence, String> {
            self.reconstructs += 1;
            Ok(ReconstructionEvidence {
                reconstructed_contract: self.reconstructed.clone(),
                verifier: "test-reconstructor/v1".to_string(),
            })
        }

        fn start_verified_session(
            &mut self,
            _launch: &VerifiedColdLaunch,
        ) -> Result<String, String> {
            self.starts += 1;
            Ok("session-1".to_string())
        }
    }

    #[test]
    fn matching_contract_starts_under_existing_execution_id() {
        let lock = lock();
        let expected_id = lock.execution_id.clone().unwrap();
        let mut executor = Executor {
            reconstructed: contract(),
            reconstructs: 0,
            starts: 0,
        };
        let launch = ColdReconstruction::reconstruct_and_verify(
            &lock,
            ColdReconstructionPolicy::Allow,
            &mut executor,
        )
        .unwrap();
        assert_eq!(executor.starts, 0);
        let receipt = ColdReconstruction::start_session(launch, &mut executor).unwrap();
        assert_eq!(receipt.execution_id, expected_id);
        assert_eq!(receipt.launch_mode, LaunchMode::Cold);
        assert_eq!(executor.starts, 1);
    }

    #[test]
    fn digest_mismatch_fails_without_starting_or_rewriting_lock() {
        let lock = lock();
        let original = lock.clone();
        let mut reconstructed = contract();
        reconstructed.runtime.digest = digest(DigestAlgorithm::Sha256, 0xff);
        let mut executor = Executor {
            reconstructed,
            reconstructs: 0,
            starts: 0,
        };
        assert_eq!(
            ColdReconstruction::reconstruct_and_verify(
                &lock,
                ColdReconstructionPolicy::Allow,
                &mut executor,
            )
            .unwrap_err(),
            ColdReconstructionError::ContractMismatch
        );
        assert_eq!(executor.starts, 0);
        assert_eq!(lock, original);
    }

    #[test]
    fn deployment_policy_can_forbid_cold_reconstruction() {
        let lock = lock();
        let mut executor = Executor {
            reconstructed: contract(),
            reconstructs: 0,
            starts: 0,
        };
        assert_eq!(
            ColdReconstruction::reconstruct_and_verify(
                &lock,
                ColdReconstructionPolicy::Forbid,
                &mut executor,
            )
            .unwrap_err(),
            ColdReconstructionError::ForbiddenByPolicy
        );
        assert_eq!(executor.reconstructs, 0);
    }

    #[test]
    fn policy_gate_is_available_before_executor_construction() {
        assert_eq!(
            ColdReconstruction::ensure_policy(ColdReconstructionPolicy::Forbid).unwrap_err(),
            ColdReconstructionError::ForbiddenByPolicy
        );
        ColdReconstruction::ensure_policy(ColdReconstructionPolicy::Allow).unwrap();
    }
}
