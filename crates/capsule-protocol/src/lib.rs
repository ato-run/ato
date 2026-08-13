//! Exact semantic domain for Capsule Protocol v1.
//!
//! State, Connector definitions, and ordered I/O Records remain isolated here
//! as the accepted v1 compatibility contract. The protocol-independent
//! Computation model lives in `capsule-core`.

#![forbid(unsafe_code)]

mod connector;
mod ids;
mod record;
mod state;
mod validate;

pub use connector::{CURRENT_SCHEMA_VERSION, CapsuleDescriptor, ConnectorDef};
pub use ids::{
    COMPONENT_ID_PATTERN, ConnectorId, ContentRef, IdentifierError, MAX_IDENTIFIER_BYTES,
    ProtocolId, RecordKindId, StateTypeId, VERSIONED_ID_PATTERN,
};
pub use record::{Direction, IoRecord, Payload};
pub use state::StateRef;
pub use validate::{DescriptorError, StreamValidationError, StreamValidator};
