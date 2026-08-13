//! Protocol-independent semantic representation of sealed Ato computations.
//!
//! Runtime process states, calculus names, codecs, evaluators, persistence,
//! traces, and materializations intentionally live outside this crate.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Maximum UTF-8 byte length of every Core identifier.
pub const MAX_IDENTIFIER_BYTES: usize = 255;

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
component_id!(RoleId, "role id");
versioned_id!(SemanticsId, "semantics id");
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

/// Exact identity of a sealed canonical [`ComputationObject`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ComputationRef(ContentRef);

impl ComputationRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let reference = ContentRef::parse(value)?;
        if reference.algorithm() != "blake3" {
            return Err(invalid(
                "computation ref",
                reference.as_str(),
                "canonical computation objects use blake3",
            ));
        }
        Ok(Self(reference))
    }

    pub fn content_ref(&self) -> &ContentRef {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ComputationRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ComputationRef {
    type Err = IdentifierError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// One typed opening in a sealed computation boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDef {
    pub protocol: ProtocolId,
    pub role: RoleId,
}

/// Interface signature exposed by a sealed computation.
pub type Boundary = BTreeMap<PortId, PortDef>;

/// Canonicalizable sealed representation of a residual computation.
///
/// The semantics module interprets `residual`, including any mapping from
/// stable boundary Port IDs to runtime calculus names or child Ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputationObject {
    pub semantics: SemanticsId,
    pub boundary: Boundary,
    pub residual: ContentRef,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computation_reference_is_only_a_sealed_object_address() {
        let reference = ComputationRef::parse(format!("blake3:{}", "ab".repeat(32))).unwrap();

        assert_eq!(reference.content_ref().algorithm(), "blake3");
    }

    #[test]
    fn computation_reference_rejects_noncanonical_hash_algorithm() {
        let result = ComputationRef::parse(format!("sha256:{}", "ab".repeat(32)));

        assert!(result.is_err());
    }

    #[test]
    fn boundary_declares_protocol_and_role_without_runtime_direction() {
        let port = PortId::parse("greeter.name").unwrap();
        let object = ComputationObject {
            semantics: SemanticsId::parse("example.greeter@1").unwrap(),
            boundary: BTreeMap::from([(
                port.clone(),
                PortDef {
                    protocol: ProtocolId::parse("example.text@1").unwrap(),
                    role: RoleId::parse("server").unwrap(),
                },
            )]),
            residual: ContentRef::parse(format!("blake3:{}", "cd".repeat(32))).unwrap(),
        };

        assert_eq!(object.boundary[&port].role.as_str(), "server");
    }
}
