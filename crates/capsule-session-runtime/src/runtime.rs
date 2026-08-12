use std::collections::{BTreeMap, BTreeSet};

use capsule_protocol::{ConnectorId, StateRef};
use thiserror::Error;

use crate::{
    AttachmentEndpoint, AttachmentPlan, AttachmentPlanError, AttachmentRequirement, BarrierId,
    DriverQuiesceReport, DurableFrontier, FrontierBarrier, StateRuntimeCapabilities,
};

pub trait DurableFrontierSource {
    fn durable_frontier(&self) -> Result<DurableFrontier, RuntimeBoundaryError>;
}

impl DurableFrontierSource for crate::SessionWal {
    fn durable_frontier(&self) -> Result<DurableFrontier, RuntimeBoundaryError> {
        Ok(crate::SessionWal::durable_frontier(self))
    }
}

impl DurableFrontierSource for crate::SharedSessionWal {
    fn durable_frontier(&self) -> Result<DurableFrontier, RuntimeBoundaryError> {
        crate::SharedSessionWal::durable_frontier(self)
            .map_err(|error| RuntimeBoundaryError::Journal(error.to_string()))
    }
}

pub trait StateRuntime {
    fn capabilities(&self) -> StateRuntimeCapabilities;

    fn prepare_restore(&mut self, plan: &AttachmentPlan) -> Result<(), RuntimeBoundaryError>;

    fn restore_paused(
        &mut self,
        state: &StateRef,
        plan: &AttachmentPlan,
    ) -> Result<Box<dyn PausedComputationRuntime>, RuntimeBoundaryError>;
}

pub trait PausedComputationRuntime {
    fn materialize_endpoints(
        &mut self,
        plan: &AttachmentPlan,
    ) -> Result<BTreeMap<ConnectorId, AttachmentEndpoint>, RuntimeBoundaryError>;

    fn resume(&mut self) -> Result<(), RuntimeBoundaryError>;

    fn pause(&mut self) -> Result<(), RuntimeBoundaryError>;

    fn terminate(&mut self) -> Result<(), RuntimeBoundaryError>;
}

pub trait ConnectorDriverRuntime {
    fn connector_id(&self) -> &ConnectorId;

    fn attachment_requirement(&self) -> AttachmentRequirement;

    fn prepare(&mut self) -> Result<(), RuntimeBoundaryError>;

    fn connect(&mut self, endpoint: &AttachmentEndpoint) -> Result<(), RuntimeBoundaryError>;

    fn begin_quiesce(&mut self, barrier_id: &BarrierId) -> Result<(), RuntimeBoundaryError>;

    fn finish_quiesce(
        &mut self,
        barrier_id: &BarrierId,
    ) -> Result<DriverQuiesceReport, RuntimeBoundaryError>;

    /// Leaves a completed quiesce barrier. Called before computation resumes.
    fn resume_after_quiesce(&mut self) -> Result<(), RuntimeBoundaryError>;

    /// Releases prepared, connected, or quiescing resources. Implementations
    /// must make this idempotent so failure cleanup can call it unconditionally.
    fn close(&mut self) -> Result<(), RuntimeBoundaryError>;
}

pub struct SessionBootstrap;

impl SessionBootstrap {
    pub fn start(
        state: &StateRef,
        mut state_runtime: Box<dyn StateRuntime>,
        mut drivers: Vec<Box<dyn ConnectorDriverRuntime>>,
        durable_frontier: Box<dyn DurableFrontierSource>,
    ) -> Result<RunningSessionRuntime, RuntimeBoundaryError> {
        let mut driver_ids = BTreeSet::new();
        let mut requirements = Vec::with_capacity(drivers.len());
        for driver in &drivers {
            let driver_id = driver.connector_id();
            if !driver_ids.insert(driver_id.clone()) {
                return Err(RuntimeBoundaryError::AttachmentPlan(
                    AttachmentPlanError::DuplicateConnector(driver_id.clone()),
                ));
            }
            let requirement = driver.attachment_requirement();
            if &requirement.connector_id != driver_id {
                return Err(RuntimeBoundaryError::DriverRequirementMismatch {
                    driver: driver_id.clone(),
                    requirement: requirement.connector_id,
                });
            }
            requirements.push(requirement);
        }
        let capabilities = state_runtime.capabilities();
        if !capabilities.restore_paused {
            return Err(RuntimeBoundaryError::PausedRestoreRequired);
        }
        let plan = AttachmentPlan::resolve(&requirements, &capabilities.attachment_mechanisms)
            .map_err(RuntimeBoundaryError::AttachmentPlan)?;

        // Drivers establish their isolated side of the boundary before State
        // restoration can create or start computation.
        for driver in &mut drivers {
            if let Err(error) = driver.prepare() {
                close_drivers(&mut drivers);
                return Err(error);
            }
        }
        if let Err(error) = state_runtime.prepare_restore(&plan) {
            close_drivers(&mut drivers);
            return Err(error);
        }
        let mut computation = match state_runtime.restore_paused(state, &plan) {
            Ok(computation) => computation,
            Err(error) => {
                close_drivers(&mut drivers);
                return Err(error);
            }
        };
        let endpoints = match computation.materialize_endpoints(&plan) {
            Ok(endpoints) => endpoints,
            Err(error) => {
                let _ = computation.terminate();
                close_drivers(&mut drivers);
                return Err(error);
            }
        };

        for driver in &mut drivers {
            let Some(endpoint) = endpoints.get(driver.connector_id()) else {
                let connector_id = driver.connector_id().clone();
                let _ = computation.terminate();
                close_drivers(&mut drivers);
                return Err(RuntimeBoundaryError::EndpointMissing(connector_id));
            };
            if let Err(error) = driver.connect(endpoint) {
                let _ = computation.terminate();
                close_drivers(&mut drivers);
                return Err(error);
            }
        }
        if let Err(error) = computation.resume() {
            let _ = computation.terminate();
            close_drivers(&mut drivers);
            return Err(error);
        }

        Ok(RunningSessionRuntime {
            computation,
            drivers,
            durable_frontier,
            usable: true,
        })
    }
}

fn close_drivers(drivers: &mut [Box<dyn ConnectorDriverRuntime>]) {
    for driver in drivers {
        let _ = driver.close();
    }
}

pub struct RunningSessionRuntime {
    computation: Box<dyn PausedComputationRuntime>,
    drivers: Vec<Box<dyn ConnectorDriverRuntime>>,
    durable_frontier: Box<dyn DurableFrontierSource>,
    usable: bool,
}

impl RunningSessionRuntime {
    pub fn establish_barrier(
        &mut self,
        barrier_id: BarrierId,
    ) -> Result<FrontierBarrier, RuntimeBoundaryError> {
        self.ensure_usable()?;
        for driver in &mut self.drivers {
            if let Err(error) = driver.begin_quiesce(&barrier_id) {
                self.invalidate_incarnation();
                return Err(error);
            }
        }
        if let Err(error) = self.computation.pause() {
            self.invalidate_incarnation();
            return Err(error);
        }

        let mut reports = Vec::with_capacity(self.drivers.len());
        for driver in &mut self.drivers {
            match driver.finish_quiesce(&barrier_id) {
                Ok(report) => reports.push(report),
                Err(error) => {
                    self.invalidate_incarnation();
                    return Err(error);
                }
            }
        }
        let durable_frontier = match self.durable_frontier.durable_frontier() {
            Ok(frontier) => frontier,
            Err(error) => {
                self.invalidate_incarnation();
                return Err(error);
            }
        };
        let expected_connectors = self
            .drivers
            .iter()
            .map(|driver| driver.connector_id().clone())
            .collect();
        let barrier = FrontierBarrier::new(barrier_id, durable_frontier, expected_connectors);
        if let Err(error) = barrier.validate_reports(&reports) {
            self.invalidate_incarnation();
            return Err(RuntimeBoundaryError::Barrier(error.to_string()));
        }
        Ok(barrier)
    }

    pub fn resume(&mut self) -> Result<(), RuntimeBoundaryError> {
        self.ensure_usable()?;
        for driver in &mut self.drivers {
            if let Err(error) = driver.resume_after_quiesce() {
                self.invalidate_incarnation();
                return Err(error);
            }
        }
        if let Err(error) = self.computation.resume() {
            self.invalidate_incarnation();
            return Err(error);
        }
        Ok(())
    }

    pub fn terminate(&mut self) -> Result<(), RuntimeBoundaryError> {
        close_drivers(&mut self.drivers);
        self.usable = false;
        self.computation.terminate()
    }

    fn ensure_usable(&self) -> Result<(), RuntimeBoundaryError> {
        if self.usable {
            Ok(())
        } else {
            Err(RuntimeBoundaryError::IncarnationTerminated)
        }
    }

    fn invalidate_incarnation(&mut self) {
        let _ = self.computation.pause();
        close_drivers(&mut self.drivers);
        let _ = self.computation.terminate();
        self.usable = false;
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeBoundaryError {
    #[error("State Runtime must support paused restore before Connector attachment")]
    PausedRestoreRequired,
    #[error("attachment plan failed: {0}")]
    AttachmentPlan(AttachmentPlanError),
    #[error("State Runtime did not materialize an endpoint for Connector {0}")]
    EndpointMissing(ConnectorId),
    #[error("Driver Connector {driver} declared attachment requirements for {requirement}")]
    DriverRequirementMismatch {
        driver: ConnectorId,
        requirement: ConnectorId,
    },
    #[error("State Runtime failed: {0}")]
    State(String),
    #[error("Connector Driver failed: {0}")]
    Driver(String),
    #[error("Session journal failed: {0}")]
    Journal(String),
    #[error("consistent frontier barrier failed: {0}")]
    Barrier(String),
    #[error("Session runtime incarnation has been terminated")]
    IncarnationTerminated,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use capsule_protocol::{ContentRef, StateTypeId};

    use super::*;
    use crate::{AttachmentMechanism, JournalLsn, RecordFrontier};

    type Events = Arc<Mutex<Vec<&'static str>>>;

    fn push(events: &Events, event: &'static str) {
        events.lock().expect("event lock").push(event);
    }

    struct FakeStateRuntime {
        events: Events,
        mechanisms: Vec<AttachmentMechanism>,
    }

    struct FakeComputation {
        events: Events,
    }

    impl StateRuntime for FakeStateRuntime {
        fn capabilities(&self) -> StateRuntimeCapabilities {
            StateRuntimeCapabilities {
                restore_paused: true,
                live_checkpoint: false,
                local_checkpoint: true,
                portable_export: true,
                atomic_snapshot: false,
                attachment_mechanisms: self.mechanisms.clone(),
            }
        }

        fn prepare_restore(&mut self, _plan: &AttachmentPlan) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "state.prepare_restore");
            Ok(())
        }

        fn restore_paused(
            &mut self,
            _state: &StateRef,
            _plan: &AttachmentPlan,
        ) -> Result<Box<dyn PausedComputationRuntime>, RuntimeBoundaryError> {
            push(&self.events, "state.restore_paused");
            Ok(Box::new(FakeComputation {
                events: self.events.clone(),
            }))
        }
    }

    impl PausedComputationRuntime for FakeComputation {
        fn materialize_endpoints(
            &mut self,
            plan: &AttachmentPlan,
        ) -> Result<BTreeMap<ConnectorId, AttachmentEndpoint>, RuntimeBoundaryError> {
            push(&self.events, "state.materialize_endpoints");
            Ok(plan
                .connectors
                .iter()
                .map(|(id, endpoint)| {
                    let mut endpoint = endpoint.clone();
                    endpoint.address = "fake://endpoint".to_owned();
                    (id.clone(), endpoint)
                })
                .collect())
        }

        fn resume(&mut self) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "state.resume");
            Ok(())
        }

        fn pause(&mut self) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "state.pause");
            Ok(())
        }

        fn terminate(&mut self) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "state.terminate");
            Ok(())
        }
    }

    struct FakeDriver {
        connector_id: ConnectorId,
        events: Events,
        reported_through: RecordFrontier,
        fail_connect: bool,
    }

    struct FakeFrontierSource(DurableFrontier);

    impl DurableFrontierSource for FakeFrontierSource {
        fn durable_frontier(&self) -> Result<DurableFrontier, RuntimeBoundaryError> {
            Ok(self.0)
        }
    }

    fn frontier(through: RecordFrontier) -> Box<dyn DurableFrontierSource> {
        Box::new(FakeFrontierSource(DurableFrontier {
            records_through: through,
            journal_through: JournalLsn::new(4096),
        }))
    }

    impl ConnectorDriverRuntime for FakeDriver {
        fn connector_id(&self) -> &ConnectorId {
            &self.connector_id
        }

        fn attachment_requirement(&self) -> AttachmentRequirement {
            AttachmentRequirement {
                connector_id: self.connector_id.clone(),
                accepted_mechanisms: vec![AttachmentMechanism::PtyEndpoint],
            }
        }

        fn prepare(&mut self) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "driver.prepare");
            Ok(())
        }

        fn connect(&mut self, _endpoint: &AttachmentEndpoint) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "driver.connect");
            if self.fail_connect {
                return Err(RuntimeBoundaryError::Driver("connect failed".to_owned()));
            }
            Ok(())
        }

        fn begin_quiesce(&mut self, _barrier_id: &BarrierId) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "driver.begin_quiesce");
            Ok(())
        }

        fn finish_quiesce(
            &mut self,
            barrier_id: &BarrierId,
        ) -> Result<DriverQuiesceReport, RuntimeBoundaryError> {
            push(&self.events, "driver.finish_quiesce");
            Ok(DriverQuiesceReport {
                connector_id: self.connector_id.clone(),
                barrier_id: barrier_id.clone(),
                through: self.reported_through,
                safe_cut: true,
            })
        }

        fn resume_after_quiesce(&mut self) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "driver.resume_after_quiesce");
            Ok(())
        }

        fn close(&mut self) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "driver.close");
            Ok(())
        }
    }

    fn state() -> StateRef {
        StateRef {
            state_type: StateTypeId::parse("ato.state.test@1").expect("state type"),
            state_ref: ContentRef::parse(format!("blake3:{}", "a".repeat(64))).expect("state ref"),
        }
    }

    #[test]
    fn computation_resumes_only_after_attachment_is_ready() {
        let events = Events::default();
        let driver = FakeDriver {
            connector_id: ConnectorId::parse("terminal.main").expect("connector"),
            events: events.clone(),
            reported_through: RecordFrontier::Origin,
            fail_connect: false,
        };
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::PtyEndpoint],
        };

        let _session = SessionBootstrap::start(
            &state(),
            Box::new(runtime),
            vec![Box::new(driver)],
            frontier(RecordFrontier::Origin),
        )
        .expect("start session");

        assert_eq!(
            *events.lock().expect("event lock"),
            [
                "driver.prepare",
                "state.prepare_restore",
                "state.restore_paused",
                "state.materialize_endpoints",
                "driver.connect",
                "state.resume",
            ]
        );
    }

    #[test]
    fn incompatible_attachment_fails_before_state_restore() {
        let events = Events::default();
        let driver = FakeDriver {
            connector_id: ConnectorId::parse("terminal.main").expect("connector"),
            events: events.clone(),
            reported_through: RecordFrontier::Origin,
            fail_connect: false,
        };
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::HttpProxy],
        };

        let result = SessionBootstrap::start(
            &state(),
            Box::new(runtime),
            vec![Box::new(driver)],
            frontier(RecordFrontier::Origin),
        );

        assert!(matches!(
            result,
            Err(RuntimeBoundaryError::AttachmentPlan(
                AttachmentPlanError::Unavailable(_)
            ))
        ));
        assert!(events.lock().expect("event lock").is_empty());
    }

    #[test]
    fn barrier_stops_driver_delivery_before_computation_pause() {
        let events = Events::default();
        let driver = FakeDriver {
            connector_id: ConnectorId::parse("terminal.main").expect("connector"),
            events: events.clone(),
            reported_through: RecordFrontier::Through(9),
            fail_connect: false,
        };
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::PtyEndpoint],
        };
        let mut session = SessionBootstrap::start(
            &state(),
            Box::new(runtime),
            vec![Box::new(driver)],
            frontier(RecordFrontier::Through(9)),
        )
        .expect("start session");
        events.lock().expect("event lock").clear();

        session
            .establish_barrier(BarrierId::new("barrier-1").expect("barrier"))
            .expect("establish barrier");

        assert_eq!(
            *events.lock().expect("event lock"),
            [
                "driver.begin_quiesce",
                "state.pause",
                "driver.finish_quiesce",
            ]
        );
    }

    #[test]
    fn barrier_rejects_driver_frontier_not_proven_by_wal_and_terminates_incarnation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let wal = crate::SharedSessionWal::open(directory.path().join("journal/wal-000001"))
            .expect("open shared WAL");
        wal.with_mut(|wal| {
            wal.append_batch(&[crate::WalEntry::RecordCandidate {
                operation_id: crate::BoundaryOperationId::parse("durable-5").expect("operation"),
                record: crate::WalRecord::from(&capsule_protocol::IoRecord {
                    seq: 5,
                    offset_ns: None,
                    observed_at_unix_ns: None,
                    connector: ConnectorId::parse("terminal.main").expect("connector"),
                    direction: capsule_protocol::Direction::Ingress,
                    kind: capsule_protocol::RecordKindId::parse("stdin").expect("kind"),
                    payload: capsule_protocol::Payload::Inline(b"input".to_vec()),
                }),
                effect: None,
            }])
        })
        .expect("commit durable frontier");
        let events = Events::default();
        let driver = FakeDriver {
            connector_id: ConnectorId::parse("terminal.main").expect("connector"),
            events: events.clone(),
            reported_through: RecordFrontier::Through(9),
            fail_connect: false,
        };
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::PtyEndpoint],
        };
        let mut session = SessionBootstrap::start(
            &state(),
            Box::new(runtime),
            vec![Box::new(driver)],
            Box::new(wal),
        )
        .expect("start session");
        events.lock().expect("event lock").clear();

        assert!(matches!(
            session.establish_barrier(BarrierId::new("barrier-forged").expect("barrier")),
            Err(RuntimeBoundaryError::Barrier(_))
        ));
        assert!(matches!(
            session.resume(),
            Err(RuntimeBoundaryError::IncarnationTerminated)
        ));
        let events = events.lock().expect("event lock");
        assert!(events.contains(&"driver.close"));
        assert!(events.contains(&"state.terminate"));
    }

    #[test]
    fn duplicate_connectors_fail_before_prepare_or_restore() {
        let events = Events::default();
        let connector_id = ConnectorId::parse("terminal.main").expect("connector");
        let drivers = vec![
            Box::new(FakeDriver {
                connector_id: connector_id.clone(),
                events: events.clone(),
                reported_through: RecordFrontier::Origin,
                fail_connect: false,
            }) as Box<dyn ConnectorDriverRuntime>,
            Box::new(FakeDriver {
                connector_id: connector_id.clone(),
                events: events.clone(),
                reported_through: RecordFrontier::Origin,
                fail_connect: false,
            }) as Box<dyn ConnectorDriverRuntime>,
        ];
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::PtyEndpoint],
        };

        assert!(matches!(
            SessionBootstrap::start(
                &state(),
                Box::new(runtime),
                drivers,
                frontier(RecordFrontier::Origin)
            ),
            Err(RuntimeBoundaryError::AttachmentPlan(
                AttachmentPlanError::DuplicateConnector(id)
            )) if id == connector_id
        ));
        assert!(events.lock().expect("event lock").is_empty());
    }

    #[test]
    fn connection_failure_closes_all_drivers_and_terminates_computation() {
        let events = Events::default();
        let drivers = vec![
            Box::new(FakeDriver {
                connector_id: ConnectorId::parse("terminal.main").expect("connector"),
                events: events.clone(),
                reported_through: RecordFrontier::Origin,
                fail_connect: false,
            }) as Box<dyn ConnectorDriverRuntime>,
            Box::new(FakeDriver {
                connector_id: ConnectorId::parse("network.main").expect("connector"),
                events: events.clone(),
                reported_through: RecordFrontier::Origin,
                fail_connect: true,
            }) as Box<dyn ConnectorDriverRuntime>,
        ];
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::PtyEndpoint],
        };

        assert!(matches!(
            SessionBootstrap::start(
                &state(),
                Box::new(runtime),
                drivers,
                frontier(RecordFrontier::Origin)
            ),
            Err(RuntimeBoundaryError::Driver(_))
        ));
        let events = events.lock().expect("event lock");
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == "driver.close")
                .count(),
            2
        );
        assert!(events.contains(&"state.terminate"));
    }
}
