use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Maximum UTF-8 byte length of every Capsule Protocol identifier.
pub const MAX_IDENTIFIER_BYTES: usize = 255;

/// Normative regular-expression pattern for connector and record-kind identifiers.
pub const COMPONENT_ID_PATTERN: &str =
    r"[a-z0-9]([a-z0-9_-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9_-]*[a-z0-9])?)*";

/// Normative regular-expression pattern for state-type and protocol identifiers.
pub const VERSIONED_ID_PATTERN: &str =
    r"[a-z0-9]([a-z0-9_-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9_-]*[a-z0-9])?)+@[1-9][0-9]*";

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

component_id!(ConnectorId, "connector id");
component_id!(RecordKindId, "record kind id");
versioned_id!(ProtocolId, "protocol id");
versioned_id!(StateTypeId, "state type id");

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_adapter_defined_identifiers() {
        assert_eq!(
            ConnectorId::parse("terminal.main").unwrap().as_str(),
            "terminal.main"
        );
        assert_eq!(
            ProtocolId::parse("ato.io.pty@1").unwrap().as_str(),
            "ato.io.pty@1"
        );
        assert_eq!(
            StateTypeId::parse("ato.state.ready-state@1")
                .unwrap()
                .as_str(),
            "ato.state.ready-state@1"
        );
        assert_eq!(
            RecordKindId::parse("vendor.pointer_move").unwrap().as_str(),
            "vendor.pointer_move"
        );
    }

    #[test]
    fn rejects_malformed_identifiers() {
        assert!(ConnectorId::parse("Terminal Main").is_err());
        assert!(ProtocolId::parse("pty").is_err());
        assert!(ProtocolId::parse("ato.io.pty@0").is_err());
        assert!(RecordKindId::parse("stdin/").is_err());
        assert!(ConnectorId::parse("terminal.-main").is_err());
        assert!(ConnectorId::parse("terminal.main_").is_err());
    }

    #[test]
    fn identifier_byte_limit_applies_to_the_complete_versioned_id() {
        let name = format!("ato.{}", "a".repeat(249));
        let maximum = format!("{name}@1");
        assert_eq!(maximum.len(), MAX_IDENTIFIER_BYTES);
        assert!(ProtocolId::parse(maximum).is_ok());

        let too_long = format!("{name}@12");
        assert_eq!(too_long.len(), MAX_IDENTIFIER_BYTES + 1);
        assert!(ProtocolId::parse(too_long).is_err());
    }

    #[test]
    fn content_refs_are_algorithm_tagged_lowercase_hex() {
        let digest = "ab".repeat(32);
        assert!(ContentRef::parse(format!("blake3:{digest}")).is_ok());
        assert!(ContentRef::parse(format!("md5:{digest}")).is_err());
        assert!(ContentRef::parse(format!("sha256:{}", digest.to_uppercase())).is_err());
    }
}
