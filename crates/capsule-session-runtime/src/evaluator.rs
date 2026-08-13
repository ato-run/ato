use std::collections::BTreeMap;

use capsule_protocol::{ComputationRef, ComputationTypeId, PortDef, PortId};
use thiserror::Error;

use crate::{
    AttachmentEndpoint, AttachmentPlan, PausedComputationRuntime, RuntimeBoundaryError,
    StateRuntimeCapabilities,
};

/// Physical runtime capabilities offered by one computation evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatorCapabilities {
    pub runtime: StateRuntimeCapabilities,
}

/// One run's binding of semantic Ports to physical attachment endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortBindingPlan {
    pub ports: BTreeMap<PortId, AttachmentEndpoint>,
    pub environment: BTreeMap<String, String>,
}

impl PortBindingPlan {
    pub fn from_attachment_plan(plan: &AttachmentPlan) -> Self {
        Self {
            ports: plan
                .connectors
                .iter()
                .map(|(connector, endpoint)| {
                    (
                        PortId::parse(connector.as_str())
                            .expect("ConnectorId and PortId share validation rules"),
                        endpoint.clone(),
                    )
                })
                .collect(),
            environment: plan.environment.clone(),
        }
    }
}

/// Restores the current head of one computation while keeping State and
/// snapshot machinery below the semantic computation boundary.
pub trait ComputationEvaluator {
    fn computation_type(&self) -> &ComputationTypeId;

    fn capabilities(&self) -> EvaluatorCapabilities;

    fn restore_paused(
        &mut self,
        computation: &ComputationRef,
        ports: &BTreeMap<PortId, PortDef>,
        plan: &PortBindingPlan,
    ) -> Result<Box<dyn PausedComputationRuntime>, RuntimeBoundaryError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EvaluatorRegistryError {
    #[error("duplicate computation evaluator for {0}")]
    Duplicate(ComputationTypeId),
    #[error("no computation evaluator is registered for {0}")]
    Unsupported(ComputationTypeId),
}

/// Exact computation-type dispatch. Registration order cannot affect which
/// evaluator owns a computation type.
#[derive(Default)]
pub struct ComputationEvaluatorRegistry {
    evaluators: BTreeMap<ComputationTypeId, Box<dyn ComputationEvaluator>>,
}

impl ComputationEvaluatorRegistry {
    pub fn register(
        &mut self,
        evaluator: Box<dyn ComputationEvaluator>,
    ) -> Result<(), EvaluatorRegistryError> {
        let computation_type = evaluator.computation_type().clone();
        if self.evaluators.contains_key(&computation_type) {
            return Err(EvaluatorRegistryError::Duplicate(computation_type));
        }
        self.evaluators.insert(computation_type, evaluator);
        Ok(())
    }

    pub fn get_mut(
        &mut self,
        computation_type: &ComputationTypeId,
    ) -> Result<&mut (dyn ComputationEvaluator + '_), EvaluatorRegistryError> {
        match self.evaluators.get_mut(computation_type) {
            Some(evaluator) => Ok(evaluator.as_mut()),
            None => Err(EvaluatorRegistryError::Unsupported(
                computation_type.clone(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AttachmentMechanism;

    struct FakeEvaluator {
        computation_type: ComputationTypeId,
    }

    impl ComputationEvaluator for FakeEvaluator {
        fn computation_type(&self) -> &ComputationTypeId {
            &self.computation_type
        }

        fn capabilities(&self) -> EvaluatorCapabilities {
            EvaluatorCapabilities {
                runtime: StateRuntimeCapabilities {
                    restore_paused: true,
                    live_checkpoint: false,
                    local_checkpoint: false,
                    portable_export: false,
                    atomic_snapshot: false,
                    attachment_mechanisms: vec![AttachmentMechanism::PtyEndpoint],
                },
            }
        }

        fn restore_paused(
            &mut self,
            _computation: &ComputationRef,
            _ports: &BTreeMap<PortId, PortDef>,
            _plan: &PortBindingPlan,
        ) -> Result<Box<dyn PausedComputationRuntime>, RuntimeBoundaryError> {
            Err(RuntimeBoundaryError::State(
                "not used by registry test".to_owned(),
            ))
        }
    }

    fn computation_type() -> ComputationTypeId {
        ComputationTypeId::parse("capsule.computation.legacy-state-io@1").unwrap()
    }

    #[test]
    fn registry_dispatches_exactly_and_rejects_duplicates() {
        let mut registry = ComputationEvaluatorRegistry::default();
        registry
            .register(Box::new(FakeEvaluator {
                computation_type: computation_type(),
            }))
            .unwrap();
        assert_eq!(
            registry
                .get_mut(&computation_type())
                .unwrap()
                .computation_type(),
            &computation_type()
        );
        assert!(matches!(
            registry.register(Box::new(FakeEvaluator {
                computation_type: computation_type(),
            })),
            Err(EvaluatorRegistryError::Duplicate(_))
        ));
        let unknown = ComputationTypeId::parse("example.computation.unknown@1").unwrap();
        assert!(matches!(
            registry.get_mut(&unknown),
            Err(EvaluatorRegistryError::Unsupported(_))
        ));
    }

    #[test]
    fn port_binding_projection_preserves_endpoint_identity() {
        let connector = capsule_protocol::ConnectorId::parse("terminal.main").unwrap();
        let plan = AttachmentPlan {
            connectors: BTreeMap::from([(
                connector,
                AttachmentEndpoint {
                    mechanism: AttachmentMechanism::PtyEndpoint,
                    address: "pty://42".to_owned(),
                },
            )]),
            environment: BTreeMap::from([("TERM".to_owned(), "xterm".to_owned())]),
        };
        let projected = PortBindingPlan::from_attachment_plan(&plan);
        assert_eq!(
            projected
                .ports
                .get(&PortId::parse("terminal.main").unwrap())
                .unwrap()
                .address,
            "pty://42"
        );
        assert_eq!(projected.environment["TERM"], "xterm");
    }
}
