//! Protocol-independent semantic model for immutable Ato computations.
//!
//! A computation's typed boundary is stored inside the content-addressed
//! [`ComputationObject`]. Protocol records, runtime materializations, traces,
//! evaluators, and Connectors intentionally live outside this crate.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Maximum UTF-8 byte length of every Core identifier.
pub const MAX_IDENTIFIER_BYTES: usize = 255;

/// Generic schema for a computation object whose body schema is type-defined.
pub const COMPUTATION_OBJECT_SCHEMA: &str = "capsule.computation.object@1";

/// Composition is represented as an ordinary computation type, not a second
/// semantic primitive.
pub const COMPOSITE_COMPUTATION_TYPE: &str = "capsule.computation.compose@1";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid {kind} `{value}`: {reason}")]
pub struct IdentifierError {
    kind: &'static str,
    value: String,
    reason: &'static str,
}

fn invalid(kind: &'static str, value: &str, reason: &'static str) -> IdentifierError {
    IdentifierError {
        kind,
        value: value.to_owned(),
        reason,
    }
}

fn validate_component_id(kind: &'static str, value: &str) -> Result<(), IdentifierError> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
        return Err(invalid(
            kind,
            value,
            "length must be between 1 and 255 bytes",
        ));
    }
    if !value.split('.').all(valid_component_segment) {
        return Err(invalid(
            kind,
            value,
            "contains an invalid character or separator",
        ));
    }
    Ok(())
}

fn valid_component_segment(segment: &str) -> bool {
    let Some(first) = segment.bytes().next() else {
        return false;
    };
    let Some(last) = segment.bytes().next_back() else {
        return false;
    };
    is_ascii_alphanumeric(first)
        && is_ascii_alphanumeric(last)
        && segment
            .bytes()
            .all(|byte| is_ascii_alphanumeric(byte) || matches!(byte, b'-' | b'_'))
}

fn is_ascii_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

fn validate_versioned_id(kind: &'static str, value: &str) -> Result<(), IdentifierError> {
    if value.is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
        return Err(invalid(
            kind,
            value,
            "length must be between 1 and 255 bytes",
        ));
    }
    let (name, version) = value
        .rsplit_once('@')
        .ok_or_else(|| invalid(kind, value, "must end in @<positive-version>"))?;
    if !name.contains('.') {
        return Err(invalid(kind, value, "must use a namespaced name"));
    }
    validate_component_id(kind, name)?;
    if version.is_empty()
        || !version.bytes().all(|byte| byte.is_ascii_digit())
        || version.starts_with('0')
    {
        return Err(invalid(
            kind,
            value,
            "version must be a positive decimal integer",
        ));
    }
    Ok(())
}

macro_rules! component_id {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                validate_component_id($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    };
}

macro_rules! versioned_id {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                validate_versioned_id($kind, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }
    };
}

component_id!(PortId, "port id");
versioned_id!(ComputationSchemaId, "computation schema id");
versioned_id!(ComputationTypeId, "computation type id");
versioned_id!(ProtocolId, "protocol id");

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentRef(String);

impl ContentRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        let (algorithm, digest) = value
            .split_once(':')
            .ok_or_else(|| invalid("content ref", &value, "must be <algorithm>:<digest>"))?;
        if !matches!(algorithm, "blake3" | "sha256") {
            return Err(invalid(
                "content ref",
                &value,
                "unsupported digest algorithm",
            ));
        }
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid(
                "content ref",
                &value,
                "digest must be 64 lowercase hex characters",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn algorithm(&self) -> &str {
        self.0.split_once(':').expect("validated content ref").0
    }

    pub fn digest(&self) -> &str {
        self.0.split_once(':').expect("validated content ref").1
    }
}

impl fmt::Display for ContentRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ContentRef {
    type Err = IdentifierError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// An immutable residual computation. `object_ref` addresses an object that
/// includes the computation's boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputationRef {
    pub computation_type: ComputationTypeId,
    pub object_ref: ContentRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortMode {
    IngressOnly,
    EgressOnly,
    Duplex,
}

/// One typed opening in a computation boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDef {
    pub protocol: ProtocolId,
    pub mode: PortMode,
    pub config_ref: Option<ContentRef>,
}

/// The complete open boundary of a computation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Boundary {
    pub ports: BTreeMap<PortId, PortDef>,
}

/// Content-addressed computation value. The computation type defines the body
/// schema; the boundary remains part of this object's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputationObject {
    pub schema: ComputationSchemaId,
    pub boundary: Boundary,
    pub body: ContentRef,
}

/// A computation reference resolved to its hash-validated object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedComputation {
    pub reference: ComputationRef,
    pub object: ComputationObject,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computation_reference_names_only_type_and_complete_object() {
        let reference = ComputationRef {
            computation_type: ComputationTypeId::parse("capsule.computation.test@1").unwrap(),
            object_ref: ContentRef::parse(format!("blake3:{}", "ab".repeat(32))).unwrap(),
        };

        assert_eq!(
            reference.computation_type.as_str(),
            "capsule.computation.test@1"
        );
        assert_eq!(reference.object_ref.algorithm(), "blake3");
    }

    #[test]
    fn boundary_is_inside_the_resolved_computation_object() {
        let port = PortId::parse("greeter.name").unwrap();
        let object = ComputationObject {
            schema: ComputationSchemaId::parse(COMPUTATION_OBJECT_SCHEMA).unwrap(),
            boundary: Boundary {
                ports: BTreeMap::from([(
                    port.clone(),
                    PortDef {
                        protocol: ProtocolId::parse("example.greeter.text@1").unwrap(),
                        mode: PortMode::IngressOnly,
                        config_ref: None,
                    },
                )]),
            },
            body: ContentRef::parse(format!("blake3:{}", "cd".repeat(32))).unwrap(),
        };

        assert_eq!(object.boundary.ports[&port].mode, PortMode::IngressOnly);
    }
}
