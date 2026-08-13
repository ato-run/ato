//! Capsule Protocol Semantic Core.
//!
//! The native model is an immutable [`ComputationRef`] with typed [`PortDef`]s
//! and ordered [`InteractionRecord`]s. Capsule Protocol v1 State, Connector,
//! and I/O values remain available as an explicit compatibility surface.
//! Encoding, storage, process execution, and runtime bindings deliberately live
//! in higher layers.

#![forbid(unsafe_code)]

mod computation;
mod connector;
mod ids;
mod interaction;
mod record;
mod state;
mod validate;

pub use computation::{
    CURRENT_COMPUTATION_SCHEMA_VERSION, ComputationDescriptor, ComputationRef,
    LEGACY_STATE_IO_COMPUTATION_TYPE, LEGACY_STATE_IO_OBJECT_SCHEMA, LegacyStateIoComputationV1,
    PortDef, PortMode,
};
pub use connector::{CURRENT_SCHEMA_VERSION, CapsuleDescriptor, ConnectorDef};
pub use ids::{
    COMPONENT_ID_PATTERN, ComputationTypeId, ConnectorId, ContentRef, IdentifierError,
    InteractionKindId, MAX_IDENTIFIER_BYTES, PortId, ProtocolId, RecordKindId, StateTypeId,
    VERSIONED_ID_PATTERN,
};
pub use interaction::{InteractionPayload, InteractionRecord};
pub use record::{Direction, IoRecord, Payload};
pub use state::StateRef;
pub use validate::{
    ComputationDescriptorError, DescriptorError, InteractionStreamValidationError,
    InteractionStreamValidator, StreamValidationError, StreamValidator,
};
