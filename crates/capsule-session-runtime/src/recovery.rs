use std::collections::BTreeMap;

use capsule_protocol::{ConnectorId, ContentRef, ProtocolId, StateRef};
use thiserror::Error;

use crate::RecordFrontier;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRecoveryPoint {
    pub state: StateRef,
    pub through: RecordFrontier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorCheckpoint {
    pub connector_id: ConnectorId,
    pub protocol_id: ProtocolId,
    pub implementation_id: String,
    pub implementation_version: String,
    pub checkpoint_format: String,
    pub applied_through: RecordFrontier,
    pub opaque_ref: ContentRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectorRecoveryStrategy {
    Fresh,
    Checkpoint { checkpoint: ConnectorCheckpoint },
    ReconstructFromRecords { from: RecordFrontier },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorRecoveryPoint {
    pub connector_id: ConnectorId,
    pub through: RecordFrontier,
    pub strategy: ConnectorRecoveryStrategy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPlan {
    pub through: RecordFrontier,
    pub state: StateRecoveryPoint,
    pub connectors: BTreeMap<ConnectorId, ConnectorRecoveryPoint>,
}

impl RecoveryPlan {
    pub fn validate_for_target(&self, target: RecordFrontier) -> Result<(), RecoveryPlanError> {
        if self.through > target {
            return Err(RecoveryPlanError::RecoveryAfterTarget {
                recovery: self.through,
                target,
            });
        }
        if self.state.through != self.through {
            return Err(RecoveryPlanError::StateFrontierMismatch {
                plan: self.through,
                state: self.state.through,
            });
        }
        for (map_id, recovery) in &self.connectors {
            if map_id != &recovery.connector_id {
                return Err(RecoveryPlanError::ConnectorKeyMismatch);
            }
            if recovery.through != self.through {
                return Err(RecoveryPlanError::ConnectorFrontierMismatch {
                    connector_id: recovery.connector_id.clone(),
                    plan: self.through,
                    connector: recovery.through,
                });
            }
            if let ConnectorRecoveryStrategy::Checkpoint { checkpoint } = &recovery.strategy {
                if checkpoint.connector_id != recovery.connector_id {
                    return Err(RecoveryPlanError::CheckpointConnectorMismatch);
                }
                if checkpoint.applied_through != self.through {
                    return Err(RecoveryPlanError::CheckpointFrontierMismatch {
                        connector_id: recovery.connector_id.clone(),
                        plan: self.through,
                        checkpoint: checkpoint.applied_through,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn records_to_replay<'a, T>(
        &self,
        target: RecordFrontier,
        records: &'a [T],
        seq: impl Fn(&T) -> u64,
    ) -> Result<Vec<&'a T>, RecoveryPlanError> {
        self.validate_for_target(target)?;
        Ok(records
            .iter()
            .filter(|record| self.through.replay_contains(target, seq(record)))
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeFidelity {
    FilesystemRestart,
    ExactRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCheckpoint {
    pub state_ref: StateRef,
    pub captured_through: RecordFrontier,
    pub connector_checkpoints: BTreeMap<ConnectorId, ConnectorCheckpoint>,
    pub resume_fidelity: ResumeFidelity,
}

impl SessionCheckpoint {
    pub fn validate_consistent_cut(&self) -> Result<(), RecoveryPlanError> {
        for (connector_id, checkpoint) in &self.connector_checkpoints {
            if connector_id != &checkpoint.connector_id {
                return Err(RecoveryPlanError::CheckpointConnectorMismatch);
            }
            if checkpoint.applied_through != self.captured_through {
                return Err(RecoveryPlanError::CheckpointFrontierMismatch {
                    connector_id: connector_id.clone(),
                    plan: self.captured_through,
                    checkpoint: checkpoint.applied_through,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecoveryPlanError {
    #[error("recovery frontier {recovery:?} is after target {target:?}")]
    RecoveryAfterTarget {
        recovery: RecordFrontier,
        target: RecordFrontier,
    },
    #[error("state frontier {state:?} does not match plan {plan:?}")]
    StateFrontierMismatch {
        plan: RecordFrontier,
        state: RecordFrontier,
    },
    #[error("connector map key does not match its recovery point")]
    ConnectorKeyMismatch,
    #[error("connector {connector_id} frontier {connector:?} does not match plan {plan:?}")]
    ConnectorFrontierMismatch {
        connector_id: ConnectorId,
        plan: RecordFrontier,
        connector: RecordFrontier,
    },
    #[error("checkpoint belongs to a different connector")]
    CheckpointConnectorMismatch,
    #[error("connector {connector_id} checkpoint {checkpoint:?} does not match plan {plan:?}")]
    CheckpointFrontierMismatch {
        connector_id: ConnectorId,
        plan: RecordFrontier,
        checkpoint: RecordFrontier,
    },
}

#[cfg(test)]
mod tests {
    use capsule_protocol::StateTypeId;

    use super::*;

    fn state(through: RecordFrontier) -> StateRecoveryPoint {
        StateRecoveryPoint {
            state: StateRef {
                state_type: StateTypeId::parse("ato.state.test@1").expect("state type"),
                state_ref: ContentRef::parse(format!("blake3:{}", "a".repeat(64)))
                    .expect("content ref"),
            },
            through,
        }
    }

    #[test]
    fn rejects_components_restored_to_different_frontiers() {
        let connector_id = ConnectorId::parse("database.main").expect("connector id");
        let plan = RecoveryPlan {
            through: RecordFrontier::Through(5),
            state: state(RecordFrontier::Through(5)),
            connectors: BTreeMap::from([(
                connector_id.clone(),
                ConnectorRecoveryPoint {
                    connector_id,
                    through: RecordFrontier::Through(6),
                    strategy: ConnectorRecoveryStrategy::Fresh,
                },
            )]),
        };

        assert!(matches!(
            plan.validate_for_target(RecordFrontier::Through(9)),
            Err(RecoveryPlanError::ConnectorFrontierMismatch { .. })
        ));
    }

    #[test]
    fn replays_exactly_after_common_recovery_through_target() {
        let plan = RecoveryPlan {
            through: RecordFrontier::Through(2),
            state: state(RecordFrontier::Through(2)),
            connectors: BTreeMap::new(),
        };
        let records = [1_u64, 2, 3, 5, 8];
        let replay = plan
            .records_to_replay(RecordFrontier::Through(5), &records, |seq| *seq)
            .expect("replay range");

        assert_eq!(replay.into_iter().copied().collect::<Vec<_>>(), [3, 5]);
    }
}
