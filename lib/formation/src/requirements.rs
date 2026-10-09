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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOs {
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostArch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceleratorVendor {
    Nvidia,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceleratorRequirement {
    pub vendor: AcceleratorVendor,
    pub count: u32,
    pub min_vram_mib: u64,
}

/// The host one Runner must measurably provide for this route to run there.
///
/// A placement condition, not authority: it grants nothing, and it is matched
/// against what a Runner measured about itself, never against what a provider
/// promised. Absent means the route states no host condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRequirement {
    pub os: HostOs,
    pub arch: HostArch,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accelerators: Vec<AcceleratorRequirement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_memory_mib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_scratch_mib: Option<u64>,
}

const MAX_ACCELERATOR_COUNT: u32 = 16;
const MAX_VRAM_MIB: u64 = 1 << 20; // 1 TiB per device
const MAX_MEMORY_MIB: u64 = 16 << 20; // 16 TiB
const MAX_SCRATCH_MIB: u64 = 1 << 30; // 1 PiB

impl HostRequirement {
    pub fn validate(&self) -> Result<(), RequirementError> {
        let bounded = |value: Option<u64>, max: u64| value.is_none_or(|v| (1..=max).contains(&v));
        if self.accelerators.len() > 4
            || self.accelerators.iter().any(|a| {
                !(1..=MAX_ACCELERATOR_COUNT).contains(&a.count)
                    || !(1..=MAX_VRAM_MIB).contains(&a.min_vram_mib)
            })
            || !bounded(self.min_memory_mib, MAX_MEMORY_MIB)
            || !bounded(self.min_scratch_mib, MAX_SCRATCH_MIB)
        {
            return Err(RequirementError("host_requirement_invalid"));
        }
        // One entry per vendor: two entries for the same vendor would need a
        // rule for combining them, and none is defined.
        if self
            .accelerators
            .iter()
            .map(|a| a.vendor)
            .collect::<BTreeSet<_>>()
            .len()
            != self.accelerators.len()
        {
            return Err(RequirementError("execution_requirements_duplicate"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRequirements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<NetworkRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authority: Vec<AuthorityRequirement>,
    /// Absent from every route formed before it existed, so those routes keep
    /// their exact bytes and `DerivationRef`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostRequirement>,
    /// Physical startup gate on the selected D, independent of K's verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup: Option<StartupRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupRequirement {
    pub port: String,
    pub path: String,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abi: Option<ProcessAbiRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessAbiRequirement {
    pub glibc_min: String,
    pub glibcxx_min: String,
}

pub fn valid_abi_version(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    version.len() <= 32
        && (2..=3).contains(&parts.len())
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && part.parse::<u16>().is_ok()
        })
}

impl StartupRequirement {
    pub fn validate(&self) -> Result<(), RequirementError> {
        if self.port.is_empty()
            || self.port.len() > 160
            || !self.path.starts_with('/')
            || self.path.starts_with("//")
            || self.path.len() > 1024
            || self
                .path
                .bytes()
                .any(|b| b.is_ascii_control() || b == b'\\' || b == b'#')
            || !(1..=300_000).contains(&self.timeout_ms)
            || self.abi.as_ref().is_some_and(|abi| {
                !valid_abi_version(&abi.glibc_min) || !valid_abi_version(&abi.glibcxx_min)
            })
        {
            return Err(RequirementError("startup_requirement_invalid"));
        }
        Ok(())
    }
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
        self.network.is_empty()
            && self.authority.is_empty()
            && self.host.is_none()
            && self.startup.is_none()
    }

    pub fn validate(&self) -> Result<(), RequirementError> {
        if let Some(startup) = &self.startup {
            startup.validate()?;
        }
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
        if let Some(host) = &self.host {
            host.validate()?;
        }
        Ok(())
    }

    pub fn canonicalized(&self) -> Result<Self, RequirementError> {
        self.validate()?;
        let mut requirements = self.clone();
        requirements.network.sort();
        requirements.authority.sort();
        if let Some(host) = &mut requirements.host {
            host.accelerators.sort();
        }
        Ok(requirements)
    }

    /// Exact subset only. Neither wildcard matching nor inferred grants.
    ///
    /// `host` and `startup` are not compared: it is a placement condition, and a grant can
    /// neither widen nor narrow it. Runtimes that cannot honour a host
    /// condition refuse it at admission instead.
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
