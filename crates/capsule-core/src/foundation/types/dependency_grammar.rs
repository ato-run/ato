use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapsuleUrl(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractRef {
    pub name: String,
    pub major: u32,
}

impl ContractRef {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let (name, major) = trimmed.rsplit_once('@').ok_or_else(|| {
            format!("contract reference must use <name>@<major>, got '{trimmed}'")
        })?;
        if name.trim().is_empty() {
            return Err(format!("contract reference is missing a name: '{trimmed}'"));
        }
        let major = major
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("contract reference major must be an integer: '{trimmed}'"))?;
        Ok(Self {
            name: name.trim().to_string(),
            major,
        })
    }
}

impl fmt::Display for ContractRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.major)
    }
}

impl Serialize for ContractRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContractRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatedString {
    pub segments: Vec<TemplateSegment>,
}

impl TemplatedString {
    pub fn literal(value: impl Into<String>) -> Self {
        Self {
            segments: vec![TemplateSegment::Literal(value.into())],
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        let mut segments = Vec::new();
        let mut cursor = raw;

        while let Some(start) = cursor.find("{{") {
            let literal = &cursor[..start];
            if !literal.is_empty() {
                segments.push(TemplateSegment::Literal(literal.to_string()));
            }

            let rest = &cursor[start + 2..];
            let end = rest
                .find("}}")
                .ok_or_else(|| format!("unterminated template expression in '{raw}'"))?;
            let expr = rest[..end].trim();
            if expr.is_empty() {
                return Err(format!("empty template expression in '{raw}'"));
            }
            segments.push(TemplateSegment::Expr(parse_template_expr(expr)?));
            cursor = &rest[end + 2..];
        }

        if cursor.contains("}}") {
            return Err(format!("unexpected closing template delimiter in '{raw}'"));
        }

        if !cursor.is_empty() {
            segments.push(TemplateSegment::Literal(cursor.to_string()));
        }

        Ok(Self { segments })
    }
}

impl fmt::Display for TemplatedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in &self.segments {
            match segment {
                TemplateSegment::Literal(value) => write!(f, "{value}")?,
                TemplateSegment::Expr(expr) => write!(f, "{{{{{expr}}}}}")?,
            }
        }
        Ok(())
    }
}

impl Serialize for TemplatedString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for TemplatedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSegment {
    Literal(String),
    Expr(TemplateExpr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateExpr {
    Params(String),
    Credentials(String),
    Env(String),
    Host,
    Port,
    Socket,
    StateDir,
    DepRuntimeExport { dep: String, key: String },
    DepIdentityExport { dep: String, key: String },
}

impl fmt::Display for TemplateExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateExpr::Params(key) => write!(f, "params.{key}"),
            TemplateExpr::Credentials(key) => write!(f, "credentials.{key}"),
            TemplateExpr::Env(key) => write!(f, "env.{key}"),
            TemplateExpr::Host => write!(f, "host"),
            TemplateExpr::Port => write!(f, "port"),
            TemplateExpr::Socket => write!(f, "socket"),
            TemplateExpr::StateDir => write!(f, "state.dir"),
            TemplateExpr::DepRuntimeExport { dep, key } => {
                write!(f, "deps.{dep}.runtime_exports.{key}")
            }
            TemplateExpr::DepIdentityExport { dep, key } => {
                write!(f, "deps.{dep}.identity_exports.{key}")
            }
        }
    }
}

fn parse_template_expr(raw: &str) -> Result<TemplateExpr, String> {
    if raw == "host" {
        return Ok(TemplateExpr::Host);
    }
    if raw == "port" {
        return Ok(TemplateExpr::Port);
    }
    if raw == "socket" {
        return Ok(TemplateExpr::Socket);
    }
    if raw == "state.dir" {
        return Ok(TemplateExpr::StateDir);
    }
    if let Some(key) = raw.strip_prefix("params.") {
        return non_empty_template_key(key, raw).map(TemplateExpr::Params);
    }
    if let Some(key) = raw.strip_prefix("credentials.") {
        return non_empty_template_key(key, raw).map(TemplateExpr::Credentials);
    }
    if let Some(key) = raw.strip_prefix("env.") {
        return non_empty_template_key(key, raw).map(TemplateExpr::Env);
    }
    if let Some(rest) = raw.strip_prefix("deps.") {
        if let Some((dep, key)) = rest.split_once(".runtime_exports.") {
            return Ok(TemplateExpr::DepRuntimeExport {
                dep: non_empty_template_key(dep, raw)?,
                key: non_empty_template_key(key, raw)?,
            });
        }
        if let Some((dep, key)) = rest.split_once(".identity_exports.") {
            return Ok(TemplateExpr::DepIdentityExport {
                dep: non_empty_template_key(dep, raw)?,
                key: non_empty_template_key(key, raw)?,
            });
        }
    }
    Err(format!("unsupported template expression '{raw}'"))
}

fn non_empty_template_key(value: &str, raw: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("template expression is missing a key: '{raw}'"))
    } else {
        Ok(trimmed.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    String(String),
    Int(i64),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Int,
    Bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamSchema {
    #[serde(rename = "type")]
    pub value_type: ValueType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ParamValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialSchema {
    #[serde(rename = "type")]
    pub value_type: ValueType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ParamValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyStateOwnership {
    Parent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyStateSpec {
    pub name: String,
    #[serde(default = "default_dependency_state_ownership")]
    pub ownership: DependencyStateOwnership,
}

fn default_dependency_state_ownership() -> DependencyStateOwnership {
    DependencyStateOwnership::Parent
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencySpec {
    pub capsule: CapsuleUrl,
    pub contract: ContractRef,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, ParamValue>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, TemplatedString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<DependencyStateSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractStateSpec {
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuntimeExportSpec {
    Shorthand(TemplatedString),
    Detailed(RuntimeExportValue),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeExportValue {
    pub value: TemplatedString,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadyProbe {
    Tcp {
        target: TemplatedString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
    },
    Probe {
        run: TemplatedString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
    },
    Http {
        url: TemplatedString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_status: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
    },
    UnixSocket {
        path: TemplatedString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointSpec {
    Fixed(u16),
    Auto,
    AutoSocket,
    None,
}

impl Serialize for EndpointSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            EndpointSpec::Fixed(port) => serializer.serialize_u16(*port),
            EndpointSpec::Auto => serializer.serialize_str("auto"),
            EndpointSpec::AutoSocket => serializer.serialize_str("auto"),
            EndpointSpec::None => serializer.serialize_unit(),
        }
    }
}

impl<'de> Deserialize<'de> for EndpointSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = toml::Value::deserialize(deserializer)?;
        match raw {
            toml::Value::Integer(value) => {
                let port = u16::try_from(value)
                    .map_err(|_| de::Error::custom(format!("port must fit in u16, got {value}")))?;
                Ok(EndpointSpec::Fixed(port))
            }
            toml::Value::String(value) => match value.trim() {
                "auto" => Ok(EndpointSpec::Auto),
                other => Err(de::Error::custom(format!(
                    "unsupported endpoint spec '{other}'"
                ))),
            },
            other => Err(de::Error::custom(format!(
                "unsupported endpoint value '{}'",
                other
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractSpec {
    pub target: String,
    pub ready: ReadyProbe,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, ParamSchema>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub credentials: BTreeMap<String, CredentialSchema>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub identity_exports: BTreeMap<String, TemplatedString>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtime_exports: BTreeMap<String, RuntimeExportSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ContractStateSpec>,
}

#[cfg(test)]
mod tests {
    use super::{ContractRef, TemplateExpr, TemplateSegment, TemplatedString};

    #[test]
    fn parses_contract_ref() {
        let parsed = ContractRef::parse("service@1").expect("parse");
        assert_eq!(parsed.name, "service");
        assert_eq!(parsed.major, 1);
    }

    #[test]
    fn templated_string_parses_supported_tokens() {
        let parsed = TemplatedString::parse(
            "postgresql://postgres:{{credentials.password}}@{{host}}:{{port}}/{{params.database}}",
        )
        .expect("parse");
        assert!(parsed.segments.iter().any(|segment| matches!(
            segment,
            TemplateSegment::Expr(TemplateExpr::Credentials(key)) if key == "password"
        )));
        assert!(parsed
            .segments
            .iter()
            .any(|segment| matches!(segment, TemplateSegment::Expr(TemplateExpr::Host))));
        assert!(parsed
            .segments
            .iter()
            .any(|segment| matches!(segment, TemplateSegment::Expr(TemplateExpr::Port))));
    }

    #[test]
    fn templated_string_rejects_unknown_tokens() {
        let error = TemplatedString::parse("{{unknown.token}}").expect_err("reject unknown token");
        assert!(error.contains("unsupported template expression"));
    }
}
