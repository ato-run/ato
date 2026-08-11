use std::fmt;
use std::str::FromStr;

use thiserror::Error;

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

fn validate_component_id(
    kind: &'static str,
    value: &str,
    allow_dot: bool,
) -> Result<(), IdentifierError> {
    if value.is_empty() || value.len() > 255 {
        return Err(invalid(
            kind,
            value,
            "length must be between 1 and 255 bytes",
        ));
    }
    if !value.as_bytes()[0].is_ascii_lowercase() && !value.as_bytes()[0].is_ascii_digit() {
        return Err(invalid(
            kind,
            value,
            "must start with a lowercase ASCII letter or digit",
        ));
    }
    let valid = value.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'-'
            || byte == b'_'
            || (allow_dot && byte == b'.')
    });
    if !valid || value.ends_with(['.', '-', '_']) || value.contains("..") {
        return Err(invalid(
            kind,
            value,
            "contains an invalid character or separator",
        ));
    }
    Ok(())
}

fn validate_versioned_id(kind: &'static str, value: &str) -> Result<(), IdentifierError> {
    let (name, version) = value
        .rsplit_once('@')
        .ok_or_else(|| invalid(kind, value, "must end in @<positive-version>"))?;
    if !name.contains('.') {
        return Err(invalid(kind, value, "must use a namespaced name"));
    }
    validate_component_id(kind, name, true)?;
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
    ($name:ident, $kind:literal, $allow_dot:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
                let value = value.into();
                validate_component_id($kind, &value, $allow_dot)?;
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

component_id!(ConnectorId, "connector id", true);
component_id!(RecordKindId, "record kind id", true);
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
    }

    #[test]
    fn content_refs_are_algorithm_tagged_lowercase_hex() {
        let digest = "ab".repeat(32);
        assert!(ContentRef::parse(format!("blake3:{digest}")).is_ok());
        assert!(ContentRef::parse(format!("md5:{digest}")).is_err());
        assert!(ContentRef::parse(format!("sha256:{}", digest.to_uppercase())).is_err());
    }
}
