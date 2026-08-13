use std::collections::BTreeMap;
use std::path::Path;

use capsule_core::{ComputationRef, ComputationTypeId, PortId, ResolvedComputation};
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

/// Resolves and hash-validates a complete computation object.
pub trait ComputationObjectResolver: Send + Sync {
    fn resolve(
        &self,
        reference: &ComputationRef,
    ) -> Result<ResolvedComputation, RuntimeBoundaryError>;
}

/// Runtime-specific materializers exposed to an evaluator without making them
/// part of the Computation Core.
pub trait MaterializationServices: Send + Sync {
    fn materialization_root(&self) -> &Path;
}

/// Dependencies and bindings for one materialization attempt.
pub struct EvaluationContext<'a> {
    pub object_resolver: &'a dyn ComputationObjectResolver,
    pub port_bindings: &'a PortBindingPlan,
    pub session_root: &'a Path,
    pub materialization_services: &'a dyn MaterializationServices,
}

impl PortBindingPlan {
    /// Projects Protocol v1 Connector bindings using the compatibility rule
    /// `ConnectorId == PortId`. Native computations must bind Ports explicitly.
    pub fn from_legacy_v1_attachment_plan(plan: &AttachmentPlan) -> Self {
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
pub trait ComputationEvaluator: Send + Sync {
    fn type_id(&self) -> &ComputationTypeId;

    fn capabilities(&self) -> EvaluatorCapabilities;

    fn materialize(
        &self,
        computation: ResolvedComputation,
        context: &EvaluationContext<'_>,
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
        let computation_type = evaluator.type_id().clone();
        if self.evaluators.contains_key(&computation_type) {
            return Err(EvaluatorRegistryError::Duplicate(computation_type));
        }
        self.evaluators.insert(computation_type, evaluator);
        Ok(())
    }

    pub fn get(
        &self,
        computation_type: &ComputationTypeId,
    ) -> Result<&(dyn ComputationEvaluator + '_), EvaluatorRegistryError> {
        match self.evaluators.get(computation_type) {
            Some(evaluator) => Ok(evaluator.as_ref()),
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
    use capsule_core::{
        Boundary, COMPUTATION_OBJECT_SCHEMA, ComputationObject, ComputationSchemaId, ContentRef,
    };
    use std::path::PathBuf;

    struct FakeEvaluator {
        computation_type: ComputationTypeId,
    }

    impl ComputationEvaluator for FakeEvaluator {
        fn type_id(&self) -> &ComputationTypeId {
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

        fn materialize(
            &self,
            _computation: ResolvedComputation,
            _context: &EvaluationContext<'_>,
        ) -> Result<Box<dyn PausedComputationRuntime>, RuntimeBoundaryError> {
            Err(RuntimeBoundaryError::State(
                "not used by registry test".to_owned(),
            ))
        }
    }

    fn computation_type() -> ComputationTypeId {
        ComputationTypeId::parse("capsule.computation.legacy-v1@1").unwrap()
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
            registry.get(&computation_type()).unwrap().type_id(),
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
            registry.get(&unknown),
            Err(EvaluatorRegistryError::Unsupported(_))
        ));
    }

    struct FakeResolver {
        resolved: ResolvedComputation,
    }

    impl ComputationObjectResolver for FakeResolver {
        fn resolve(
            &self,
            reference: &ComputationRef,
        ) -> Result<ResolvedComputation, RuntimeBoundaryError> {
            if reference != &self.resolved.reference {
                return Err(RuntimeBoundaryError::State(
                    "unexpected computation reference".to_owned(),
                ));
            }
            Ok(self.resolved.clone())
        }
    }

    struct FakeMaterializationServices(PathBuf);

    impl MaterializationServices for FakeMaterializationServices {
        fn materialization_root(&self) -> &Path {
            &self.0
        }
    }

    #[test]
    fn resolver_returns_boundary_as_part_of_computation_object() {
        let reference = ComputationRef {
            computation_type: computation_type(),
            object_ref: ContentRef::parse(format!("blake3:{}", "a".repeat(64))).unwrap(),
        };
        let resolved = ResolvedComputation {
            reference: reference.clone(),
            object: ComputationObject {
                schema: ComputationSchemaId::parse(COMPUTATION_OBJECT_SCHEMA).unwrap(),
                boundary: Boundary::default(),
                body: ContentRef::parse(format!("blake3:{}", "b".repeat(64))).unwrap(),
            },
        };
        let resolver = FakeResolver {
            resolved: resolved.clone(),
        };
        let bindings = PortBindingPlan::default();
        let services = FakeMaterializationServices(PathBuf::from("/materialization"));
        let context = EvaluationContext {
            object_resolver: &resolver,
            port_bindings: &bindings,
            session_root: Path::new("/session"),
            materialization_services: &services,
        };

        let actual = context.object_resolver.resolve(&reference).unwrap();

        assert_eq!(actual, resolved);
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
        let projected = PortBindingPlan::from_legacy_v1_attachment_plan(&plan);
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
