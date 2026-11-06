//! Executable requirements are part of D; ceilings and approval are not.
pub mod inspection;

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
    /// The exact status that counts as started. Absent means any 2xx, which is
    /// what every route formed before this field existed observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_status: Option<u16>,
    /// A condition on the bounded response body. An application that answers
    /// 200 while it is still loading can say so here; the route is started only
    /// once the condition holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<StartupJsonCondition>,
    /// Upper bound on the bytes read to evaluate `json`. Absent means
    /// [`DEFAULT_STARTUP_BODY_MAX_BYTES`]; a body that exceeds it fails the
    /// probe rather than being truncated into a false verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_bytes: Option<u32>,
}

/// A bounded condition over a JSON startup response.
///
/// This is a physical preparation signal, never a Contract observation: it
/// informs when a Run may be treated as started, not whether K is satisfied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupJsonCondition {
    /// RFC 6901 JSON Pointer into the response body (`/data/progress`).
    /// The empty pointer names the whole body.
    pub pointer: String,
    /// The pointed scalar must be at least this integer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gte: Option<StartupJsonScalar>,
    /// The pointed scalar must equal this value exactly, including its JSON
    /// type: `100` is not `"100"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<StartupJsonScalar>,
}

/// A JSON scalar a startup condition compares against.
///
/// Floats are deliberately absent: a readiness threshold is a discrete
/// progress value, and accepting `99.5` would make the comparison depend on
/// binary floating point on both sides of the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StartupJsonScalar {
    Bool(bool),
    Integer(i64),
    Text(String),
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

/// The default bound on a startup response body read for a JSON condition.
pub const DEFAULT_STARTUP_BODY_MAX_BYTES: u32 = 64 * 1024;
/// The largest body a route may ask to read. A startup probe is a signal, not
/// a document capture.
pub const MAX_STARTUP_BODY_BYTES: u32 = 1024 * 1024;
const MAX_STARTUP_POINTER_BYTES: usize = 512;
const MAX_STARTUP_EQUALS_TEXT_BYTES: usize = 256;

impl StartupRequirement {
    /// The number of body bytes a probe may read to evaluate `json`.
    pub fn body_max_bytes(&self) -> u32 {
        self.max_body_bytes
            .unwrap_or(DEFAULT_STARTUP_BODY_MAX_BYTES)
    }

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
            || self
                .expect_status
                .is_some_and(|status| !(100..=599).contains(&status))
            || self
                .max_body_bytes
                .is_some_and(|bytes| !(1..=MAX_STARTUP_BODY_BYTES).contains(&bytes))
            // A size bound with nothing to read is a requirement nothing
            // observes; refuse it instead of pretending it is enforced.
            || (self.max_body_bytes.is_some() && self.json.is_none())
        {
            return Err(RequirementError("startup_requirement_invalid"));
        }
        if let Some(json) = &self.json {
            json.validate()?;
        }
        Ok(())
    }
}

impl StartupJsonCondition {
    pub fn validate(&self) -> Result<(), RequirementError> {
        let pointer_bytes = self.pointer.as_bytes();
        let pointer_valid = self.pointer.len() <= MAX_STARTUP_POINTER_BYTES
            && (self.pointer.is_empty()
                || (self.pointer.starts_with('/')
                    && !pointer_bytes.iter().any(|byte| byte.is_ascii_control())));
        // Exactly one comparison: "at least 100 AND equals \"ok\"" has no
        // defined meaning for a scalar, and silently picking one is worse.
        let single_comparison = self.gte.is_some() ^ self.equals.is_some();
        let gte_valid = self
            .gte
            .as_ref()
            .is_none_or(|value| matches!(value, StartupJsonScalar::Integer(0..)));
        let equals_valid = self.equals.as_ref().is_none_or(|value| match value {
            StartupJsonScalar::Text(text) => text.len() <= MAX_STARTUP_EQUALS_TEXT_BYTES,
            _ => true,
        });
        if !pointer_valid || !single_comparison || !gte_valid || !equals_valid {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn startup() -> StartupRequirement {
        StartupRequirement {
            port: "app.http".to_owned(),
            path: "/health".to_owned(),
            timeout_ms: 60_000,
            abi: None,
            expect_status: None,
            json: None,
            max_body_bytes: None,
        }
    }

    #[test]
    fn q15_a_startup_without_the_new_fields_keeps_its_exact_bytes() {
        let legacy = startup();
        let json = serde_json::to_string(&legacy).unwrap();
        assert_eq!(
            json,
            r#"{"port":"app.http","path":"/health","timeout_ms":60000}"#
        );
        let parsed: StartupRequirement = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, legacy);
        assert_eq!(legacy.body_max_bytes(), DEFAULT_STARTUP_BODY_MAX_BYTES);
    }

    #[test]
    fn q15_a_startup_json_condition_is_bounded_and_unambiguous() {
        let mut requirement = startup();
        requirement.json = Some(StartupJsonCondition {
            pointer: "/data/progress".to_owned(),
            gte: Some(StartupJsonScalar::Integer(100)),
            equals: None,
        });
        requirement.expect_status = Some(200);
        requirement.max_body_bytes = Some(4096);
        requirement.validate().unwrap();
        assert_eq!(requirement.body_max_bytes(), 4096);

        let round_trip: StartupRequirement =
            serde_json::from_str(&serde_json::to_string(&requirement).unwrap()).unwrap();
        assert_eq!(round_trip, requirement);

        for (what, mutate) in [
            (
                "both comparisons",
                Box::new(|r: &mut StartupRequirement| {
                    r.json.as_mut().unwrap().equals = Some(StartupJsonScalar::Bool(true));
                }) as Box<dyn Fn(&mut StartupRequirement)>,
            ),
            (
                "no comparison",
                Box::new(|r: &mut StartupRequirement| {
                    r.json.as_mut().unwrap().gte = None;
                }),
            ),
            (
                "boolean threshold",
                Box::new(|r: &mut StartupRequirement| {
                    r.json.as_mut().unwrap().gte = Some(StartupJsonScalar::Bool(true));
                }),
            ),
            (
                "negative threshold",
                Box::new(|r: &mut StartupRequirement| {
                    r.json.as_mut().unwrap().gte = Some(StartupJsonScalar::Integer(-1));
                }),
            ),
            (
                "relative pointer",
                Box::new(|r: &mut StartupRequirement| {
                    r.json.as_mut().unwrap().pointer = "data/progress".to_owned();
                }),
            ),
            (
                "status out of range",
                Box::new(|r: &mut StartupRequirement| {
                    r.expect_status = Some(99);
                }),
            ),
            (
                "size bound without a condition",
                Box::new(|r: &mut StartupRequirement| {
                    r.json = None;
                }),
            ),
            (
                "unbounded body",
                Box::new(|r: &mut StartupRequirement| {
                    r.max_body_bytes = Some(MAX_STARTUP_BODY_BYTES + 1);
                }),
            ),
        ] {
            let mut candidate = requirement.clone();
            mutate(&mut candidate);
            assert!(candidate.validate().is_err(), "{what}");
        }
    }
}
