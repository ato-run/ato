//! Canonical residual and semantic validation for `capsule.compose@1`.
//!
//! Composition remains an ordinary semantics over the unchanged Capsule Core.
//! This crate deliberately contains no evaluator, runtime binding, history, or
//! materialization model. Its reducer accepts child transition evidence only.

#![forbid(unsafe_code)]

mod codec;
mod semantics;
mod step;
mod validate;

use ato_computation::{ProtocolId, ResolvedComputation, RoleId, SemanticsId};
use ato_objects::{
    BundleError, ComputationReferences, ObjectLink, ObjectResolver, read_exact_object,
};

pub use ato_computation::{
    CompositeResidual, Connection, ConnectionError, Endpoint, NodeId, NodeIdError,
};

pub use codec::{
    CompositeResidualCodecError, MAX_COMPOSITE_RESIDUAL_BYTES, composite_residual_ref,
    decode_composite_residual, encode_composite_residual,
};
pub use semantics::ComposeSemantics;
pub use step::{
    CompositeReduction, CompositeStepError, NodeStep, StepLabel, lift_exported_step,
    lift_internal_step, synchronize_connection,
};
pub use validate::{
    BoundaryVisibility, CompositeValidationError, DEFAULT_MAX_RESOLVED_BYTES,
    DEFAULT_MAX_UNIQUE_COMPUTATIONS, DEFAULT_MAX_VALIDATION_DEPTH, ValidatedComposite,
    ValidationBudget, ValidationResource, ValidationResourceLimitExceeded, validate_composite,
    validate_composite_with_budget,
};

/// The only SemanticsId interpreted by this crate.
pub const COMPOSE_SEMANTICS_ID: &str = "capsule.compose@1";

pub struct ComposeReferences {
    id: SemanticsId,
}

impl Default for ComposeReferences {
    fn default() -> Self {
        Self {
            id: SemanticsId::parse(COMPOSE_SEMANTICS_ID)
                .expect("static compose semantics id is valid"),
        }
    }
}

impl ComputationReferences for ComposeReferences {
    fn semantics(&self) -> &SemanticsId {
        &self.id
    }

    fn outgoing(
        &self,
        computation: &ResolvedComputation,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let residual = &computation.object().residual;
        let metadata = objects.metadata(residual)?;
        let bytes = read_exact_object(
            objects,
            residual,
            metadata.size,
            MAX_COMPOSITE_RESIDUAL_BYTES,
        )?;
        let composite = decode_composite_residual(&bytes).map_err(|error| {
            BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
        })?;
        Ok(composite
            .nodes
            .into_values()
            .map(ObjectLink::Computation)
            .collect())
    }
}

/// Protocol-owned decision surface used by the compose validator.
///
/// Returning `false` also represents an unknown protocol or role pair. Compose
/// never infers compatibility from identifier spelling.
pub trait ProtocolRolePolicy: Send + Sync {
    fn connection_roles_compatible(
        &self,
        protocol: &ProtocolId,
        first: &RoleId,
        second: &RoleId,
    ) -> bool;

    fn export_role_compatible(
        &self,
        protocol: &ProtocolId,
        parent: &RoleId,
        child: &RoleId,
    ) -> bool;
}

fn compose_semantics_id() -> SemanticsId {
    SemanticsId::parse(COMPOSE_SEMANTICS_ID).expect("static compose semantics id is valid")
}
