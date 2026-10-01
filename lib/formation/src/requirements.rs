//! Executable requirements are part of D; ceilings and approval are not.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Dependencies,
    Build,
    Runtime,
}

/// Exact HTTPS/HTTP destination. A broker must also reject private resolved
/// addresses and independently check every redirect and connection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRequirement {
    pub phase: ExecutionPhase,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceOperation {
    Read,
    Write,
    Create,
    Execute,
    Bind,
}

/// Adapter-owned logical resource, resolved only within a search's isolated
/// namespace. It is never a host path or a production credential value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityRequirement {
    pub phase: ExecutionPhase,
    pub protocol: String,
    pub resource: String,
    pub operation: ResourceOperation,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRequirements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<NetworkRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authority: Vec<AuthorityRequirement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RequirementError(pub &'static str);

fn hostname(host: &str) -> bool {
    host.len() <= 253
        && host.parse::<std::net::IpAddr>().is_err()
        && host.split('.').count() >= 2
        && !host.ends_with(".localhost")
        && !host.ends_with(".local")
        && host.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl ExecutionRequirements {
    pub fn is_empty(&self) -> bool {
        self.network.is_empty() && self.authority.is_empty()
    }

    pub fn validate(&self) -> Result<(), RequirementError> {
        if self.network.len() > 64 || self.authority.len() > 64 {
            return Err(RequirementError("execution_requirements_bounds"));
        }
        if self
            .network
            .iter()
            .any(|r| !hostname(&r.host) || !matches!(r.port, 80 | 443))
        {
            return Err(RequirementError("network_requirement_invalid"));
        }
        if self.authority.iter().any(|r| {
            r.protocol.is_empty()
                || r.protocol.len() > 96
                || !r
                    .protocol
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-@/".contains(&b))
                || r.resource.is_empty()
                || r.resource.len() > 256
                || !r
                    .resource
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
        }) {
            return Err(RequirementError("authority_requirement_invalid"));
        }
        if self.network.iter().collect::<BTreeSet<_>>().len() != self.network.len()
            || self.authority.iter().collect::<BTreeSet<_>>().len() != self.authority.len()
        {
            return Err(RequirementError("execution_requirements_duplicate"));
        }
        Ok(())
    }

    pub fn canonicalized(&self) -> Result<Self, RequirementError> {
        self.validate()?;
        let mut requirements = self.clone();
        requirements.network.sort();
        requirements.authority.sort();
        Ok(requirements)
    }

    /// Exact subset only. Neither wildcard matching nor inferred grants.
    pub fn within(&self, ceiling: &Self) -> Result<(), RequirementError> {
        self.validate()?;
        ceiling.validate()?;
        if self.network.iter().any(|r| !ceiling.network.contains(r))
            || self
                .authority
                .iter()
                .any(|r| !ceiling.authority.contains(r))
        {
            return Err(RequirementError("exploration_authority_exceeded"));
        }
        Ok(())
    }
}
