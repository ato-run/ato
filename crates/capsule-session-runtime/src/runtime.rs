use std::collections::BTreeMap;

use capsule_protocol::{ConnectorId, StateRef};
use thiserror::Error;

use crate::{
    AttachmentEndpoint, AttachmentPlan, AttachmentRequirement, BarrierId, DriverQuiesceReport,
    FrontierBarrier, RecordFrontier, StateRuntimeCapabilities,
};

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
        through: RecordFrontier,
    ) -> Result<DriverQuiesceReport, RuntimeBoundaryError>;
}

pub struct SessionBootstrap;

impl SessionBootstrap {
    pub fn start(
        state: &StateRef,
        mut state_runtime: Box<dyn StateRuntime>,
        mut drivers: Vec<Box<dyn ConnectorDriverRuntime>>,
    ) -> Result<RunningSessionRuntime, RuntimeBoundaryError> {
        let requirements = drivers
            .iter()
            .map(|driver| driver.attachment_requirement())
            .collect::<Vec<_>>();
        let capabilities = state_runtime.capabilities();
        if !capabilities.restore_paused {
            return Err(RuntimeBoundaryError::PausedRestoreRequired);
        }
        let plan = AttachmentPlan::resolve(&requirements, &capabilities.attachment_mechanisms)
            .map_err(RuntimeBoundaryError::AttachmentUnavailable)?;

        // Drivers establish their isolated side of the boundary before State
        // restoration can create or start computation.
        for driver in &mut drivers {
            driver.prepare()?;
        }
        state_runtime.prepare_restore(&plan)?;
        let mut computation = state_runtime.restore_paused(state, &plan)?;
        let endpoints = match computation.materialize_endpoints(&plan) {
            Ok(endpoints) => endpoints,
            Err(error) => {
                let _ = computation.terminate();
                return Err(error);
            }
        };

        for driver in &mut drivers {
            let endpoint = endpoints.get(driver.connector_id()).ok_or_else(|| {
                RuntimeBoundaryError::EndpointMissing(driver.connector_id().clone())
            })?;
            if let Err(error) = driver.connect(endpoint) {
                let _ = computation.terminate();
                return Err(error);
            }
        }
        if let Err(error) = computation.resume() {
            let _ = computation.terminate();
            return Err(error);
        }

        Ok(RunningSessionRuntime {
            computation,
            drivers,
        })
    }
}

pub struct RunningSessionRuntime {
    computation: Box<dyn PausedComputationRuntime>,
    drivers: Vec<Box<dyn ConnectorDriverRuntime>>,
}

impl RunningSessionRuntime {
    pub fn establish_barrier(
        &mut self,
        barrier_id: BarrierId,
        through: RecordFrontier,
    ) -> Result<FrontierBarrier, RuntimeBoundaryError> {
        for driver in &mut self.drivers {
            driver.begin_quiesce(&barrier_id)?;
        }
        self.computation.pause()?;

        let mut reports = Vec::with_capacity(self.drivers.len());
        for driver in &mut self.drivers {
            reports.push(driver.finish_quiesce(&barrier_id, through)?);
        }
        let expected_connectors = self
            .drivers
            .iter()
            .map(|driver| driver.connector_id().clone())
            .collect();
        let barrier = FrontierBarrier::new(barrier_id, through, expected_connectors);
        barrier
            .validate_reports(&reports)
            .map_err(|error| RuntimeBoundaryError::Barrier(error.to_string()))?;
        Ok(barrier)
    }

    pub fn resume(&mut self) -> Result<(), RuntimeBoundaryError> {
        self.computation.resume()
    }

    pub fn terminate(&mut self) -> Result<(), RuntimeBoundaryError> {
        self.computation.terminate()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RuntimeBoundaryError {
    #[error("State Runtime must support paused restore before Connector attachment")]
    PausedRestoreRequired,
    #[error("no compatible attachment for Connector {0}")]
    AttachmentUnavailable(ConnectorId),
    #[error("State Runtime did not materialize an endpoint for Connector {0}")]
    EndpointMissing(ConnectorId),
    #[error("State Runtime failed: {0}")]
    State(String),
    #[error("Connector Driver failed: {0}")]
    Driver(String),
    #[error("consistent frontier barrier failed: {0}")]
    Barrier(String),
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use capsule_protocol::{ContentRef, StateTypeId};

    use super::*;
    use crate::AttachmentMechanism;

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
            Ok(())
        }

        fn begin_quiesce(&mut self, _barrier_id: &BarrierId) -> Result<(), RuntimeBoundaryError> {
            push(&self.events, "driver.begin_quiesce");
            Ok(())
        }

        fn finish_quiesce(
            &mut self,
            barrier_id: &BarrierId,
            through: RecordFrontier,
        ) -> Result<DriverQuiesceReport, RuntimeBoundaryError> {
            push(&self.events, "driver.finish_quiesce");
            Ok(DriverQuiesceReport {
                connector_id: self.connector_id.clone(),
                barrier_id: barrier_id.clone(),
                through,
                safe_cut: true,
            })
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
        };
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::PtyEndpoint],
        };

        let _session = SessionBootstrap::start(&state(), Box::new(runtime), vec![Box::new(driver)])
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
        };
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::HttpProxy],
        };

        let result = SessionBootstrap::start(&state(), Box::new(runtime), vec![Box::new(driver)]);

        assert!(matches!(
            result,
            Err(RuntimeBoundaryError::AttachmentUnavailable(_))
        ));
        assert!(events.lock().expect("event lock").is_empty());
    }

    #[test]
    fn barrier_stops_driver_delivery_before_computation_pause() {
        let events = Events::default();
        let driver = FakeDriver {
            connector_id: ConnectorId::parse("terminal.main").expect("connector"),
            events: events.clone(),
        };
        let runtime = FakeStateRuntime {
            events: events.clone(),
            mechanisms: vec![AttachmentMechanism::PtyEndpoint],
        };
        let mut session =
            SessionBootstrap::start(&state(), Box::new(runtime), vec![Box::new(driver)])
                .expect("start session");
        events.lock().expect("event lock").clear();

        session
            .establish_barrier(
                BarrierId::new("barrier-1").expect("barrier"),
                RecordFrontier::Through(9),
            )
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
}
