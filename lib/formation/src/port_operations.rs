//! Pure declaration and safe evidence for HTTP Adapter operations. Values and
//! physical endpoints are Runtime inputs; neither belongs in this declaration.
use crate::authoring::{BoundPort, BoundStep, HTTP_PROTOCOL, PROCESS_PROTOCOL};
use crate::requirements::{
    AuthorityRequirement, ExecutionPhase, ExecutionRequirements, ResourceOperation,
};
use crate::variables::VariableRequirement;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTemplate {
    pub method: Method,
    pub path: String,
    /// Public JSON field to declared Binding input name, never its value.
    pub json_bindings: BTreeMap<String, String>,
}

fn name_valid(name: &str, binding: bool) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with("ATO_")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || (!binding && b".-".contains(&b)))
}
impl RequestTemplate {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.path.starts_with('/')
            || self.path.starts_with("//")
            || self.path.len() > 2048
            || self
                .path
                .bytes()
                .any(|b| !b.is_ascii_graphic() || b"?#\\".contains(&b))
            || self.path.split('/').any(|s| s == "..")
            || self.json_bindings.len() > 32
            || (self.method == Method::Get && !self.json_bindings.is_empty())
            || self
                .json_bindings
                .iter()
                .any(|(field, binding)| !name_valid(field, false) || !name_valid(binding, true))
        {
            return Err("invalid bound HTTP request template");
        }
        Ok(())
    }
}

/// The HTTP Adapter owns this domain-specific payload; Kernel remains opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observation {
    RequestTemplate { template: RequestTemplate },
    ResponseStatus { status: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusGuard {
    pub path: String,
    /// Execute only when this explicit GET reports one of these statuses.
    pub statuses: Vec<u16>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePortOperation {
    pub port: String,
    pub request: RequestTemplate,
    pub accepted_statuses: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<StatusGuard>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortOperationObservation {
    pub operation_index: usize,
    pub port: String,
    pub guard: bool,
    pub observation: Observation,
}
pub const RUNTIME_CAPABILITY: &str = "formation.http_port.operations.v1";
pub fn is_false(value: &bool) -> bool {
    !*value
}

fn statuses_valid(statuses: &[u16]) -> bool {
    !statuses.is_empty()
        && statuses.len() <= 16
        && statuses.iter().all(|s| (200..=599).contains(s))
        && statuses.windows(2).all(|pair| pair[0] < pair[1])
}
impl RuntimePortOperation {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.request.validate()?;
        if self.port.is_empty()
            || self.port.len() > 64
            || !self
                .port
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || !statuses_valid(&self.accepted_statuses)
        {
            return Err("runtime_port_operation_invalid");
        }
        if let Some(guard) = &self.when {
            RequestTemplate {
                method: Method::Get,
                path: guard.path.clone(),
                json_bindings: BTreeMap::new(),
            }
            .validate()?;
            if !statuses_valid(&guard.statuses) {
                return Err("runtime_port_guard_invalid");
            }
        }
        Ok(())
    }
    pub fn authority(&self) -> AuthorityRequirement {
        AuthorityRequirement {
            phase: ExecutionPhase::Runtime,
            protocol: HTTP_PROTOCOL.into(),
            resource: self.port.clone(),
            operation: ResourceOperation::Execute,
        }
    }
}
pub fn validate_bound(
    operations: &[RuntimePortOperation],
    ports: &[BoundPort],
    steps: &[BoundStep],
    variables: &[VariableRequirement],
    requirements: &ExecutionRequirements,
) -> Result<(), &'static str> {
    if operations.len() > 4 {
        return Err("runtime_port_operation_bounds");
    }
    for operation in operations {
        operation.validate()?;
        let port = ports
            .iter()
            .find(|p| p.id == operation.port && p.protocol == HTTP_PROTOCOL)
            .ok_or("runtime_port_unbound")?;
        if !steps
            .iter()
            .any(|s| s.id == port.from && s.protocol == PROCESS_PROTOCOL && s.op == "serve")
        {
            return Err("runtime_port_operation_route_unsupported");
        }
        if !requirements.authority.contains(&operation.authority()) {
            return Err("runtime_port_authority_missing");
        }
        for name in operation.request.json_bindings.values() {
            if !variables.iter().any(|v| {
                v.name == *name
                    && v.phase == ExecutionPhase::Runtime
                    && !v.artifact_embedding
                    && v.resource == operation.port
                    && v.operation == ResourceOperation::Execute
            }) {
                return Err("runtime_port_input_unbound");
            }
        }
    }
    Ok(())
}
