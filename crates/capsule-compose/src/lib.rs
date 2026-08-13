//! Canonical residual and semantic validation for `capsule.compose@1`.
//!
//! Composition remains an ordinary semantics over the unchanged Capsule Core.
//! This crate deliberately contains no evaluator, runtime binding, history, or
//! materialization model. Its reducer accepts child transition evidence only.

#![forbid(unsafe_code)]

mod codec;
mod step;
mod validate;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use capsule_core::{ComputationRef, PortId, ProtocolId, RoleId, SemanticsId};
use thiserror::Error;

pub use codec::{
    CompositeResidualCodecError, MAX_COMPOSITE_RESIDUAL_BYTES, composite_residual_ref,
    decode_composite_residual, encode_composite_residual,
};
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

/// Identifier for a child occurrence within one composite residual.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(String);

impl NodeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, NodeIdError> {
        let value = value.into();
        validate_node_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for NodeId {
    type Err = NodeIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid node id `{value}`: {reason}")]
pub struct NodeIdError {
    value: String,
    reason: &'static str,
}

fn validate_node_id(value: &str) -> Result<(), NodeIdError> {
    if value.is_empty() || value.len() > capsule_core::MAX_IDENTIFIER_BYTES {
        return Err(NodeIdError {
            value: value.to_owned(),
            reason: "length must be between 1 and 255 bytes",
        });
    }
    if !value.split('.').all(valid_node_segment) {
        return Err(NodeIdError {
            value: value.to_owned(),
            reason: "contains an invalid character or separator",
        });
    }
    Ok(())
}

fn valid_node_segment(segment: &str) -> bool {
    let Some(first) = segment.bytes().next() else {
        return false;
    };
    let Some(last) = segment.bytes().next_back() else {
        return false;
    };
    is_node_alphanumeric(first)
        && is_node_alphanumeric(last)
        && segment
            .bytes()
            .all(|byte| is_node_alphanumeric(byte) || matches!(byte, b'-' | b'_'))
}

fn is_node_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// A child Port selected by a node occurrence and its local PortId.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Endpoint {
    pub node: NodeId,
    pub port: PortId,
}

/// An undirected connection stored in canonical endpoint order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Connection {
    first: Endpoint,
    second: Endpoint,
}

impl Connection {
    pub fn new(first: Endpoint, second: Endpoint) -> Result<Self, ConnectionError> {
        if first == second {
            return Err(ConnectionError::SelfConnection(first));
        }
        let (first, second) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        Ok(Self { first, second })
    }

    pub fn first(&self) -> &Endpoint {
        &self.first
    }

    pub fn second(&self) -> &Endpoint {
        &self.second
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConnectionError {
    #[error("endpoint {0:?} cannot be connected to itself")]
    SelfConnection(Endpoint),
}

/// Canonicalizable residual selected by `capsule.compose@1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeResidual {
    pub nodes: BTreeMap<NodeId, ComputationRef>,
    pub connections: Vec<Connection>,
    pub exports: BTreeMap<PortId, Endpoint>,
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
