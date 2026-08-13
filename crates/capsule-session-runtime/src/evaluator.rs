use std::collections::BTreeMap;
use std::path::Path;

use capsule_core::{PortId, SemanticsId};
use capsule_core_codec::{ObjectResolver, ResolvedComputation};
use thiserror::Error;

use crate::{AttachmentEndpoint, PausedComputationRuntime, RuntimeBoundaryError};

/// One run's explicit binding of semantic Ports to physical endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortBindingPlan {
    pub ports: BTreeMap<PortId, AttachmentEndpoint>,
    pub environment: BTreeMap<String, String>,
}

/// Runtime-specific materializers exposed without making them part of Core.
pub trait MaterializationServices: Send + Sync {
    fn materialization_root(&self) -> &Path;
}

/// Dependencies and bindings for one materialization attempt.
///
/// `objects` is deliberately generic: evaluators can open the residual object
/// and every transitively referenced object without adding those formats to
/// the Computation Core.
pub struct EvaluationContext<'a> {
    pub objects: &'a dyn ObjectResolver,
    pub bindings: &'a PortBindingPlan,
    pub session_root: &'a Path,
    pub materialization: &'a dyn MaterializationServices,
}

/// Restores one verified computation while keeping runtime machinery below
/// the semantic computation boundary.
pub trait ComputationEvaluator: Send + Sync {
    fn semantics(&self) -> &SemanticsId;

    fn materialize(
        &self,
        computation: &ResolvedComputation,
        context: &EvaluationContext<'_>,
    ) -> Result<Box<dyn PausedComputationRuntime>, RuntimeBoundaryError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EvaluatorRegistryError {
    #[error("duplicate computation evaluator for {0}")]
    Duplicate(SemanticsId),
    #[error("no computation evaluator is registered for {0}")]
    Unsupported(SemanticsId),
}

/// Exact semantics dispatch. Registration order cannot affect ownership.
#[derive(Default)]
pub struct ComputationEvaluatorRegistry {
    evaluators: BTreeMap<SemanticsId, Box<dyn ComputationEvaluator>>,
}

impl ComputationEvaluatorRegistry {
    pub fn register(
        &mut self,
        evaluator: Box<dyn ComputationEvaluator>,
    ) -> Result<(), EvaluatorRegistryError> {
        let semantics = evaluator.semantics().clone();
        if self.evaluators.contains_key(&semantics) {
            return Err(EvaluatorRegistryError::Duplicate(semantics));
        }
        self.evaluators.insert(semantics, evaluator);
        Ok(())
    }

    pub fn get(
        &self,
        semantics: &SemanticsId,
    ) -> Result<&(dyn ComputationEvaluator + '_), EvaluatorRegistryError> {
        self.evaluators
            .get(semantics)
            .map(Box::as_ref)
            .ok_or_else(|| EvaluatorRegistryError::Unsupported(semantics.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEvaluator {
        semantics: SemanticsId,
    }

    impl ComputationEvaluator for FakeEvaluator {
        fn semantics(&self) -> &SemanticsId {
            &self.semantics
        }

        fn materialize(
            &self,
            _computation: &ResolvedComputation,
            _context: &EvaluationContext<'_>,
        ) -> Result<Box<dyn PausedComputationRuntime>, RuntimeBoundaryError> {
            Err(RuntimeBoundaryError::State(
                "not used by registry test".to_owned(),
            ))
        }
    }

    fn semantics() -> SemanticsId {
        SemanticsId::parse("capsule.legacy-v1@1").unwrap()
    }

    #[test]
    fn registry_dispatches_exactly_and_rejects_duplicates() {
        let mut registry = ComputationEvaluatorRegistry::default();
        registry
            .register(Box::new(FakeEvaluator {
                semantics: semantics(),
            }))
            .unwrap();
        assert_eq!(
            registry.get(&semantics()).unwrap().semantics(),
            &semantics()
        );
        assert!(matches!(
            registry.register(Box::new(FakeEvaluator {
                semantics: semantics(),
            })),
            Err(EvaluatorRegistryError::Duplicate(_))
        ));
        let unknown = SemanticsId::parse("example.unknown@1").unwrap();
        assert!(matches!(
            registry.get(&unknown),
            Err(EvaluatorRegistryError::Unsupported(_))
        ));
    }
}
