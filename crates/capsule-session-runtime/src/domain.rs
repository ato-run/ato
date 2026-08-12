use capsule_protocol::ConnectorId;
use serde::{Deserialize, Serialize};

use crate::BoundaryOperationId;

/// A committed cut in the global Capsule Record sequence.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case", tag = "kind", content = "seq")]
pub enum RecordFrontier {
    #[default]
    Origin,
    Through(u64),
}

/// Monotonic byte position of the last durably committed WAL frame.
///
/// This is runtime-local evidence and is never serialized into a portable
/// Capsule.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct JournalLsn(u64);

impl JournalLsn {
    pub const ORIGIN: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A Record frontier backed by a specific durable WAL position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DurableFrontier {
    pub records_through: RecordFrontier,
    pub journal_through: JournalLsn,
}

impl RecordFrontier {
    pub fn contains(self, seq: u64) -> bool {
        match self {
            Self::Origin => false,
            Self::Through(through) => seq <= through,
        }
    }

    pub fn replay_contains(self, target: Self, seq: u64) -> bool {
        !self.contains(seq) && target.contains(seq)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLifecycle {
    Created,
    Starting,
    Replaying,
    Running,
    Suspending,
    Suspended,
    Blocked { reason: SessionBlockReason },
    Terminating,
    Stopped,
    Failed { reason: SessionFailure },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBlockReason {
    EffectOutcomeUnknown { operation_id: BoundaryOperationId },
    BoundaryRecoveryRequired,
    RebindRequired { connector_id: ConnectorId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionFailure {
    BoundaryLost,
    RecoveryUnavailable,
    ProtocolViolation(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoricalReplayVerdict {
    Unverified,
    Verified {
        from: RecordFrontier,
        through: RecordFrontier,
    },
    Diverged {
        seq: u64,
    },
    Unsupported {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorMode {
    HistoricalReplay,
    Isolated,
    Live,
    Blocked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_range_excludes_recovery_frontier_and_includes_target() {
        let recovery = RecordFrontier::Through(10);
        let target = RecordFrontier::Through(13);

        assert!(!recovery.replay_contains(target, 10));
        assert!(recovery.replay_contains(target, 11));
        assert!(recovery.replay_contains(target, 13));
        assert!(!recovery.replay_contains(target, 14));
    }

    #[test]
    fn origin_is_distinct_from_unknown_and_replays_first_record() {
        assert!(RecordFrontier::Origin.replay_contains(RecordFrontier::Through(1), 1));
    }

    #[test]
    fn durable_frontier_keeps_record_and_journal_positions_distinct() {
        let frontier = DurableFrontier {
            records_through: RecordFrontier::Through(9),
            journal_through: JournalLsn::new(4096),
        };

        assert_eq!(frontier.records_through, RecordFrontier::Through(9));
        assert_eq!(frontier.journal_through.get(), 4096);
    }
}
