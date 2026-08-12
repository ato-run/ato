use std::collections::{BTreeMap, BTreeSet};

use capsule_protocol::{ConnectorId, IoRecord};
use thiserror::Error;

use crate::{
    BoundaryDeliveryLedger, BoundaryOperationId, DurableFrontier, EffectIntent, EffectState,
    EffectTransaction, RecordFrontier, SessionWal, SharedSessionWal, WalEntry, WalError, WalRecord,
};

pub trait JournalCommit {
    type Error: std::error::Error + Send + Sync + 'static;

    fn commit(&mut self, entries: &[WalEntry]) -> Result<DurableFrontier, Self::Error>;
}

impl JournalCommit for SessionWal {
    type Error = WalError;

    fn commit(&mut self, entries: &[WalEntry]) -> Result<DurableFrontier, Self::Error> {
        self.append_batch(entries)
    }
}

impl JournalCommit for SharedSessionWal {
    type Error = WalError;

    fn commit(&mut self, entries: &[WalEntry]) -> Result<DurableFrontier, Self::Error> {
        self.with_mut(|wal| wal.append_batch(entries))
    }
}

pub trait BoundaryDriver {
    type Error: std::error::Error + Send + Sync + 'static;

    fn deliver_ingress(
        &mut self,
        operation_id: &BoundaryOperationId,
        record: &IoRecord,
    ) -> Result<(), Self::Error>;

    fn dispatch_effect(
        &mut self,
        operation_id: &BoundaryOperationId,
        intent: &EffectIntent,
    ) -> Result<(), Self::Error>;
}

/// Reference ordering coordinator for one Driver boundary.
///
/// It deliberately commits release states before calling the Driver. If the
/// Driver disappears during the call, recovery sees an uncertain delivery or
/// Dispatching effect and invalidates the computation incarnation.
pub struct BoundaryCoordinator<J, D> {
    journal: J,
    driver: D,
    deliveries: BoundaryDeliveryLedger,
    effects: BTreeMap<BoundaryOperationId, EffectTransaction>,
    operation_ids: BTreeSet<BoundaryOperationId>,
}

impl<J, D> BoundaryCoordinator<J, D>
where
    J: JournalCommit,
    D: BoundaryDriver,
{
    pub fn new(journal: J, driver: D) -> Self {
        Self {
            journal,
            driver,
            deliveries: BoundaryDeliveryLedger::default(),
            effects: BTreeMap::new(),
            operation_ids: BTreeSet::new(),
        }
    }

    pub fn deliver_ingress(
        &mut self,
        operation_id: BoundaryOperationId,
        record: &IoRecord,
    ) -> Result<(), DriverBoundaryError> {
        self.reserve_operation(&operation_id)?;
        if let Err(error) = self.journal.commit(&[
            WalEntry::RecordCandidate {
                operation_id: operation_id.clone(),
                record: WalRecord::from(record),
                effect: None,
            },
            WalEntry::HighWaterMark { seq: record.seq },
        ]) {
            self.operation_ids.remove(&operation_id);
            return Err(journal_error(error));
        }
        self.deliveries
            .candidate_durable(operation_id.clone())
            .map_err(|error| DriverBoundaryError::Protocol(error.to_string()))?;

        self.journal
            .commit(&[WalEntry::DeliveryReleased {
                operation_id: operation_id.clone(),
            }])
            .map_err(journal_error)?;
        self.deliveries
            .release_delivery(&operation_id)
            .map_err(|error| DriverBoundaryError::Protocol(error.to_string()))?;

        self.driver
            .deliver_ingress(&operation_id, record)
            .map_err(|error| DriverBoundaryError::Driver(error.to_string()))?;

        self.journal
            .commit(&[WalEntry::DeliveryAcknowledged {
                operation_id: operation_id.clone(),
            }])
            .map_err(journal_error)?;
        self.deliveries
            .acknowledge_delivery(&operation_id)
            .map_err(|error| DriverBoundaryError::Protocol(error.to_string()))?;
        Ok(())
    }

    /// Durably commits an observed Egress record that has no external effect.
    ///
    /// The caller may release the bytes to observers only after this method
    /// returns. Ordinary terminal output must use this path instead of
    /// manufacturing an `EffectIntent` with a no-op effect class.
    pub fn commit_egress(
        &mut self,
        operation_id: BoundaryOperationId,
        record: &IoRecord,
    ) -> Result<DurableFrontier, DriverBoundaryError> {
        if record.direction != capsule_protocol::Direction::Egress {
            return Err(DriverBoundaryError::Protocol(
                "commit_egress requires an Egress record".to_owned(),
            ));
        }
        self.reserve_operation(&operation_id)?;
        match self.journal.commit(&[
            WalEntry::RecordCandidate {
                operation_id: operation_id.clone(),
                record: WalRecord::from(record),
                effect: None,
            },
            WalEntry::HighWaterMark { seq: record.seq },
        ]) {
            Ok(frontier) => Ok(frontier),
            Err(error) => {
                self.operation_ids.remove(&operation_id);
                Err(journal_error(error))
            }
        }
    }

    pub fn prepare_effect(
        &mut self,
        operation_id: BoundaryOperationId,
        record: &IoRecord,
        intent: EffectIntent,
    ) -> Result<(), DriverBoundaryError> {
        self.reserve_operation(&operation_id)?;
        if let Err(error) = self.journal.commit(&[
            WalEntry::RecordCandidate {
                operation_id: operation_id.clone(),
                record: WalRecord::from(record),
                effect: Some(intent.clone()),
            },
            WalEntry::EffectTransition {
                operation_id: operation_id.clone(),
                state: EffectState::Prepared,
            },
            WalEntry::HighWaterMark { seq: record.seq },
        ]) {
            self.operation_ids.remove(&operation_id);
            return Err(journal_error(error));
        }
        self.effects.insert(
            operation_id.clone(),
            EffectTransaction::prepare(operation_id, intent),
        );
        Ok(())
    }

    pub fn authorize_and_dispatch(
        &mut self,
        operation_id: &BoundaryOperationId,
    ) -> Result<(), DriverBoundaryError> {
        let transaction = self.effects.get(operation_id).ok_or_else(|| {
            DriverBoundaryError::Protocol(format!("unknown effect {operation_id}"))
        })?;
        let authorized = transaction
            .authorized()
            .map_err(|error| DriverBoundaryError::Protocol(error.to_string()))?;
        self.journal
            .commit(&[WalEntry::EffectTransition {
                operation_id: operation_id.clone(),
                state: EffectState::Authorized,
            }])
            .map_err(journal_error)?;
        self.effects
            .insert(operation_id.clone(), authorized.clone());

        let dispatching = authorized
            .dispatching()
            .map_err(|error| DriverBoundaryError::Protocol(error.to_string()))?;
        self.journal
            .commit(&[WalEntry::EffectTransition {
                operation_id: operation_id.clone(),
                state: EffectState::Dispatching,
            }])
            .map_err(journal_error)?;
        self.effects
            .insert(operation_id.clone(), dispatching.clone());

        self.driver
            .dispatch_effect(operation_id, &dispatching.intent)
            .map_err(|error| DriverBoundaryError::Driver(error.to_string()))?;
        Ok(())
    }

    pub fn has_uncertain_delivery(&self) -> bool {
        self.deliveries.has_uncertain_delivery()
    }

    pub fn recover_effects_after_crash(&mut self) {
        for transaction in self.effects.values_mut() {
            transaction.recover_after_crash();
        }
    }

    pub fn effect(&self, operation_id: &BoundaryOperationId) -> Option<&EffectTransaction> {
        self.effects.get(operation_id)
    }

    pub fn into_parts(self) -> (J, D) {
        (self.journal, self.driver)
    }

    fn reserve_operation(
        &mut self,
        operation_id: &BoundaryOperationId,
    ) -> Result<(), DriverBoundaryError> {
        if !self.operation_ids.insert(operation_id.clone()) {
            return Err(DriverBoundaryError::Protocol(format!(
                "duplicate boundary operation {operation_id}"
            )));
        }
        Ok(())
    }
}

fn journal_error(error: impl std::error::Error) -> DriverBoundaryError {
    DriverBoundaryError::Journal(error.to_string())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DriverBoundaryError {
    #[error("journal commit failed: {0}")]
    Journal(String),
    #[error("Driver boundary failed: {0}")]
    Driver(String),
    #[error("boundary protocol failed: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BarrierId(String);

impl BarrierId {
    pub fn new(value: impl Into<String>) -> Result<Self, BarrierError> {
        let value = value.into();
        if value.is_empty() || value.len() > 255 || !value.is_ascii() {
            return Err(BarrierError::InvalidId);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverQuiesceReport {
    pub connector_id: ConnectorId,
    pub barrier_id: BarrierId,
    pub through: RecordFrontier,
    pub safe_cut: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierBarrier {
    pub barrier_id: BarrierId,
    pub durable_frontier: DurableFrontier,
    expected_connectors: Vec<ConnectorId>,
}

impl FrontierBarrier {
    pub fn new(
        barrier_id: BarrierId,
        durable_frontier: DurableFrontier,
        expected_connectors: Vec<ConnectorId>,
    ) -> Self {
        Self {
            barrier_id,
            durable_frontier,
            expected_connectors,
        }
    }

    pub fn validate_reports(&self, reports: &[DriverQuiesceReport]) -> Result<(), BarrierError> {
        if reports.len() != self.expected_connectors.len() {
            return Err(BarrierError::MissingConnectorReport);
        }
        for connector_id in &self.expected_connectors {
            let report = reports
                .iter()
                .find(|report| &report.connector_id == connector_id)
                .ok_or(BarrierError::MissingConnectorReport)?;
            if report.barrier_id != self.barrier_id {
                return Err(BarrierError::BarrierMismatch);
            }
            if !report.safe_cut {
                return Err(BarrierError::UnsafeCut(report.connector_id.clone()));
            }
            if report.through != self.durable_frontier.records_through {
                return Err(BarrierError::FrontierMismatch {
                    connector_id: report.connector_id.clone(),
                    expected: self.durable_frontier.records_through,
                    actual: report.through,
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BarrierError {
    #[error("barrier id must be 1..=255 ASCII bytes")]
    InvalidId,
    #[error("a Connector did not report the barrier")]
    MissingConnectorReport,
    #[error("Driver reported a different barrier")]
    BarrierMismatch,
    #[error("Connector {0} has not reached a protocol-safe cut")]
    UnsafeCut(ConnectorId),
    #[error("Connector {connector_id} reported {actual:?}, expected {expected:?}")]
    FrontierMismatch {
        connector_id: ConnectorId,
        expected: RecordFrontier,
        actual: RecordFrontier,
    },
}

#[cfg(test)]
mod tests {
    use capsule_protocol::{Direction, Payload, RecordKindId};

    use super::*;
    use crate::{EffectClass, EffectOperationDigest};

    #[derive(Debug, Default)]
    struct FakeJournal {
        durable: Vec<WalEntry>,
        fail_next: bool,
        successful_commits_before_failure: Option<usize>,
        durable_frontier: DurableFrontier,
    }

    #[derive(Debug, Error)]
    #[error("fake journal failure")]
    struct FakeJournalError;

    impl JournalCommit for FakeJournal {
        type Error = FakeJournalError;

        fn commit(&mut self, entries: &[WalEntry]) -> Result<DurableFrontier, Self::Error> {
            if std::mem::take(&mut self.fail_next) {
                return Err(FakeJournalError);
            }
            if let Some(remaining) = &mut self.successful_commits_before_failure {
                if *remaining == 0 {
                    self.successful_commits_before_failure = None;
                    return Err(FakeJournalError);
                }
                *remaining -= 1;
            }
            self.durable.extend_from_slice(entries);
            if let Some(seq) = entries.iter().filter_map(WalEntry::record_seq).max() {
                self.durable_frontier.records_through = RecordFrontier::Through(seq);
            }
            self.durable_frontier.journal_through = crate::JournalLsn::new(
                self.durable_frontier.journal_through.get() + entries.len() as u64,
            );
            Ok(self.durable_frontier)
        }
    }

    #[derive(Debug, Default)]
    struct FakeDriver {
        delivered: Vec<BoundaryOperationId>,
        dispatched: Vec<BoundaryOperationId>,
        fail_delivery_after_accept: bool,
        fail_dispatch_after_accept: bool,
    }

    #[derive(Debug, Error)]
    #[error("fake Driver failure")]
    struct FakeDriverError;

    impl BoundaryDriver for FakeDriver {
        type Error = FakeDriverError;

        fn deliver_ingress(
            &mut self,
            operation_id: &BoundaryOperationId,
            _record: &IoRecord,
        ) -> Result<(), Self::Error> {
            self.delivered.push(operation_id.clone());
            if self.fail_delivery_after_accept {
                return Err(FakeDriverError);
            }
            Ok(())
        }

        fn dispatch_effect(
            &mut self,
            operation_id: &BoundaryOperationId,
            _intent: &EffectIntent,
        ) -> Result<(), Self::Error> {
            self.dispatched.push(operation_id.clone());
            if self.fail_dispatch_after_accept {
                return Err(FakeDriverError);
            }
            Ok(())
        }
    }

    fn record(seq: u64, direction: Direction) -> IoRecord {
        IoRecord {
            seq,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("network.main").expect("connector"),
            direction,
            kind: RecordKindId::parse("message").expect("kind"),
            payload: Payload::Inline(b"payload".to_vec()),
        }
    }

    #[test]
    fn ingress_is_not_delivered_when_candidate_commit_fails() {
        let journal = FakeJournal {
            fail_next: true,
            ..FakeJournal::default()
        };
        let driver = FakeDriver::default();
        let mut coordinator = BoundaryCoordinator::new(journal, driver);
        let result = coordinator.deliver_ingress(
            BoundaryOperationId::parse("input-1").expect("operation"),
            &record(1, Direction::Ingress),
        );
        let (_, driver) = coordinator.into_parts();

        assert!(matches!(result, Err(DriverBoundaryError::Journal(_))));
        assert!(driver.delivered.is_empty());
    }

    #[test]
    fn duplicate_operation_is_rejected_before_second_wal_write() {
        let mut coordinator =
            BoundaryCoordinator::new(FakeJournal::default(), FakeDriver::default());
        let operation_id = BoundaryOperationId::parse("input-duplicate").expect("operation");
        coordinator
            .deliver_ingress(operation_id.clone(), &record(1, Direction::Ingress))
            .expect("first delivery");
        let durable_before_duplicate = coordinator.journal.durable.clone();

        assert!(matches!(
            coordinator.deliver_ingress(operation_id, &record(2, Direction::Ingress)),
            Err(DriverBoundaryError::Protocol(_))
        ));
        assert_eq!(coordinator.journal.durable, durable_before_duplicate);
    }

    #[test]
    fn ordinary_egress_is_durable_before_observer_release() {
        let mut coordinator =
            BoundaryCoordinator::new(FakeJournal::default(), FakeDriver::default());
        let frontier = coordinator
            .commit_egress(
                BoundaryOperationId::parse("output-1").expect("operation"),
                &record(7, Direction::Egress),
            )
            .expect("commit output");

        assert_eq!(frontier.records_through, RecordFrontier::Through(7));
        assert!(matches!(
            coordinator.journal.durable.as_slice(),
            [
                WalEntry::RecordCandidate { effect: None, .. },
                WalEntry::HighWaterMark { seq: 7 }
            ]
        ));
    }

    #[test]
    fn ordinary_egress_rejects_ingress_without_touching_wal() {
        let mut coordinator =
            BoundaryCoordinator::new(FakeJournal::default(), FakeDriver::default());
        assert!(matches!(
            coordinator.commit_egress(
                BoundaryOperationId::parse("wrong-direction").expect("operation"),
                &record(1, Direction::Ingress),
            ),
            Err(DriverBoundaryError::Protocol(_))
        ));
        assert!(coordinator.journal.durable.is_empty());
    }

    #[test]
    fn failed_effect_commit_does_not_advance_memory_state() {
        let mut coordinator =
            BoundaryCoordinator::new(FakeJournal::default(), FakeDriver::default());
        let operation_id = BoundaryOperationId::parse("charge-fail").expect("operation");
        coordinator
            .prepare_effect(
                operation_id.clone(),
                &record(3, Direction::Egress),
                EffectIntent {
                    class: EffectClass::External,
                    operation_digest: EffectOperationDigest::for_bytes(b"charge"),
                },
            )
            .expect("prepare effect");
        coordinator.journal.fail_next = true;

        assert!(matches!(
            coordinator.authorize_and_dispatch(&operation_id),
            Err(DriverBoundaryError::Journal(_))
        ));
        assert_eq!(
            coordinator
                .effect(&operation_id)
                .map(|effect| &effect.state),
            Some(&EffectState::Prepared)
        );
    }

    #[test]
    fn failed_dispatching_commit_leaves_memory_at_durable_authorized_state() {
        let mut coordinator =
            BoundaryCoordinator::new(FakeJournal::default(), FakeDriver::default());
        let operation_id = BoundaryOperationId::parse("charge-dispatch-fail").expect("operation");
        coordinator
            .prepare_effect(
                operation_id.clone(),
                &record(4, Direction::Egress),
                EffectIntent {
                    class: EffectClass::External,
                    operation_digest: EffectOperationDigest::for_bytes(b"charge"),
                },
            )
            .expect("prepare effect");
        coordinator.journal.successful_commits_before_failure = Some(1);

        assert!(matches!(
            coordinator.authorize_and_dispatch(&operation_id),
            Err(DriverBoundaryError::Journal(_))
        ));
        assert_eq!(
            coordinator
                .effect(&operation_id)
                .map(|effect| &effect.state),
            Some(&EffectState::Authorized)
        );
    }

    #[test]
    fn uncertain_delivery_requires_new_runtime_incarnation() {
        let driver = FakeDriver {
            fail_delivery_after_accept: true,
            ..FakeDriver::default()
        };
        let mut coordinator = BoundaryCoordinator::new(FakeJournal::default(), driver);
        let result = coordinator.deliver_ingress(
            BoundaryOperationId::parse("input-2").expect("operation"),
            &record(2, Direction::Ingress),
        );

        assert!(matches!(result, Err(DriverBoundaryError::Driver(_))));
        assert!(coordinator.has_uncertain_delivery());
    }

    #[test]
    fn dispatching_is_durable_before_driver_can_emit_external_effect() {
        let driver = FakeDriver {
            fail_dispatch_after_accept: true,
            ..FakeDriver::default()
        };
        let mut coordinator = BoundaryCoordinator::new(FakeJournal::default(), driver);
        let operation_id = BoundaryOperationId::parse("charge-1").expect("operation");
        coordinator
            .prepare_effect(
                operation_id.clone(),
                &record(3, Direction::Egress),
                EffectIntent {
                    class: EffectClass::External,
                    operation_digest: EffectOperationDigest::for_bytes(b"charge"),
                },
            )
            .expect("prepare effect");
        assert!(matches!(
            coordinator.authorize_and_dispatch(&operation_id),
            Err(DriverBoundaryError::Driver(_))
        ));
        coordinator.recover_effects_after_crash();
        assert_eq!(
            coordinator
                .effect(&operation_id)
                .map(|effect| &effect.state),
            Some(&EffectState::InDoubt)
        );
    }

    #[test]
    fn barrier_requires_every_driver_at_same_safe_frontier() {
        let connector = ConnectorId::parse("stream.main").expect("connector");
        let barrier_id = BarrierId::new("barrier-1").expect("barrier");
        let barrier = FrontierBarrier::new(
            barrier_id.clone(),
            DurableFrontier {
                records_through: RecordFrontier::Through(8),
                journal_through: crate::JournalLsn::new(800),
            },
            vec![connector.clone()],
        );
        let report = DriverQuiesceReport {
            connector_id: connector.clone(),
            barrier_id,
            through: RecordFrontier::Through(7),
            safe_cut: true,
        };
        assert!(matches!(
            barrier.validate_reports(&[report]),
            Err(BarrierError::FrontierMismatch { .. })
        ));
    }
}
