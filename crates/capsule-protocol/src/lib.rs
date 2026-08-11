//! Capsule Protocol Semantic Core.
//!
//! This crate defines only continuable [`StateRef`]s, versioned
//! [`ConnectorDef`]s, and ordered [`IoRecord`]s. Encoding, storage, process
//! execution, and connector adapters deliberately live in higher layers.

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
