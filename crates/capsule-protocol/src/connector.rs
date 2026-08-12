use std::collections::BTreeMap;

use crate::{ConnectorId, ContentRef, ProtocolId, StateRef};

pub const CURRENT_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleDescriptor {
    pub schema_version: u16,
    pub base_state: StateRef,
    pub connectors: BTreeMap<ConnectorId, ConnectorDef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorDef {
    pub protocol: ProtocolId,
    pub config_ref: Option<ContentRef>,
}
