use std::collections::BTreeMap;

use crate::{ComputationTypeId, ContentRef, PortId, ProtocolId};

/// Current semantic schema for computation descriptors.
pub const CURRENT_COMPUTATION_SCHEMA_VERSION: u16 = 1;

/// An immutable, content-addressed residual computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputationRef {
    pub computation_type: ComputationTypeId,
    pub computation_ref: ContentRef,
}

/// The directions in which interactions may cross a Port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMode {
    IngressOnly,
    EgressOnly,
    Duplex,
}

/// A typed semantic interface exposed by a computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDef {
    pub protocol: ProtocolId,
    pub mode: PortMode,
    pub config_ref: Option<ContentRef>,
}

/// The current head of a computation and its open composition boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputationDescriptor {
    pub schema_version: u16,
    pub root: ComputationRef,
    pub ports: BTreeMap<PortId, PortDef>,
    /// Optional evidence origin for a trace ending at `root`.
    pub trace_from: Option<ComputationRef>,
}

/// Canonical compatibility object that seals one Capsule Protocol v1
/// State + Connector + Record recipe as a computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyStateIoComputationV1 {
    pub schema: String,
    pub descriptor_ref: ContentRef,
    pub record_stream_ref: ContentRef,
}

pub const LEGACY_STATE_IO_COMPUTATION_TYPE: &str = "capsule.computation.legacy-state-io@1";
pub const LEGACY_STATE_IO_OBJECT_SCHEMA: &str = "capsule.computation.legacy-state-io.object@1";
