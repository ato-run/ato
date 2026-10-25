//! Pure declaration and safe evidence for HTTP Adapter operations. Values and
//! physical endpoints are Runtime inputs; neither belongs in this declaration.
use crate::authoring::{BoundPort, BoundStep, HTTP_PROTOCOL, PROCESS_PROTOCOL};
use crate::requirements::{
    AuthorityRequirement, ExecutionPhase, ExecutionRequirements, ResourceOperation,
};
use crate::variables::VariableRequirement;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarKind {
    String,
    Boolean,
    Integer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderEncoding {
    Direct,
    Bearer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderBinding {
    pub binding: String,
    pub encoding: HeaderEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectKeySelector {
    pub pointer: String,
    pub where_pointer: String,
    pub binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseBinding {
    pub binding: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub json_pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_text_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_object_key: Option<ObjectKeySelector>,
    pub scalar: ScalarKind,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseCheck {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub json_pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    pub binding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scalar: Option<ScalarKind>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTemplate {
    pub method: Method,
    pub path: String,
    /// Public JSON field to declared Binding input name, never its value.
    pub json_bindings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub header_bindings: BTreeMap<String, HeaderBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub path_bindings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cookie_bindings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub json_types: BTreeMap<String, ScalarKind>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cookies: bool,
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
    pub fn binding_names(&self) -> impl Iterator<Item = &String> {
        self.json_bindings
            .values()
            .chain(self.path_bindings.values())
            .chain(self.cookie_bindings.values())
            .chain(self.header_bindings.values().map(|h| &h.binding))
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.path.starts_with('/')
            || self.path.starts_with("//")
            || self.path.len() > 2048
            || self
                .path
                .bytes()
                .any(|b| !b.is_ascii_graphic() || b"?#\\".contains(&b))
            || self.path.split('/').any(|s| s == "..")
            || (!self.path_bindings.is_empty() && self.path.bytes().any(|b| b == b'%'))
            || self.json_bindings.len() > 32
            || (self.method == Method::Get && !self.json_bindings.is_empty())
            || self
                .json_bindings
                .iter()
                .any(|(field, binding)| !name_valid(field, false) || !name_valid(binding, true))
        {
            return Err("invalid bound HTTP request template");
        }
        if self.header_bindings.len() > 8
            || self.path_bindings.len() > 8
            || self.cookie_bindings.len() > 8
            || (!self.cookies && !self.cookie_bindings.is_empty())
            || self
                .cookie_bindings
                .iter()
                .any(|(cookie, binding)| !cookie_name_valid(cookie) || !name_valid(binding, true))
            || self
                .header_bindings
                .iter()
                .any(|(header, value)| !header_valid(header) || !name_valid(&value.binding, true))
            || self.path_bindings.iter().any(|(slot, binding)| {
                !name_valid(slot, true)
                    || !name_valid(binding, true)
                    || !self
                        .path
                        .split('/')
                        .any(|segment| segment == format!("{{{slot}}}"))
            })
            || self
                .json_types
                .keys()
                .any(|field| !self.json_bindings.contains_key(field))
            || (!self.path_bindings.is_empty()
                && self.path.split('/').any(|segment| {
                    if segment.contains(['{', '}']) {
                        segment
                            .strip_prefix('{')
                            .and_then(|s| s.strip_suffix('}'))
                            .is_none_or(|slot| !self.path_bindings.contains_key(slot))
                    } else {
                        false
                    }
                }))
        {
            return Err("invalid private HTTP binding template");
        }
        Ok(())
    }
}

fn cookie_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn header_valid(header: &str) -> bool {
    !header.is_empty()
        && header.len() <= 64
        && header
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !matches!(
            header,
            "host"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "cookie"
                | "set-cookie"
                | "content-type"
                | "accept"
                | "expect"
                | "upgrade"
                | "te"
                | "trailer"
                | "proxy-authorization"
                | "proxy-connection"
                | "forwarded"
        )
        && !header.starts_with("x-forwarded-")
}
fn pointer_valid(pointer: &str) -> bool {
    pointer.starts_with('/')
        && pointer.len() <= 256
        && pointer
            .split('/')
            .skip(1)
            .all(|part| name_valid(part, false))
}

fn check_valid(check: &ResponseCheck) -> bool {
    name_valid(&check.binding, true)
        && ((!check.json_pointer.is_empty()) as u8 + check.header_name.is_some() as u8 == 1)
        && (check.json_pointer.is_empty() || pointer_valid(&check.json_pointer))
        && check.header_name.as_ref().is_none_or(|header| {
            !header.is_empty()
                && header.len() <= 64
                && header
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && check
                    .scalar
                    .is_none_or(|scalar| scalar == ScalarKind::String)
        })
}

fn capture_valid(capture: &ResponseBinding) -> bool {
    let selectors = usize::from(!capture.json_pointer.is_empty())
        + usize::from(capture.html_text_id.is_some())
        + usize::from(capture.json_object_key.is_some());
    selectors == 1
        && name_valid(&capture.binding, true)
        && (1..=4096).contains(&capture.max_bytes)
        && (capture.json_pointer.is_empty() || pointer_valid(&capture.json_pointer))
        && capture
            .html_text_id
            .as_ref()
            .is_none_or(|id| name_valid(id, false) && capture.scalar == ScalarKind::String)
        && capture.json_object_key.as_ref().is_none_or(|selector| {
            (selector.pointer.is_empty() || pointer_valid(&selector.pointer))
                && pointer_valid(&selector.where_pointer)
                && name_valid(&selector.binding, true)
                && capture.scalar == ScalarKind::String
        })
}

/// The HTTP Adapter owns this domain-specific payload; Kernel remains opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observation {
    RequestTemplate { template: RequestTemplate },
    ResponseStatus { status: u16 },
    ResponseCheck { check: ResponseCheck, matched: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusGuard {
    pub path: String,
    /// Execute only when this explicit GET reports one of these statuses.
    pub statuses: Vec<u16>,
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePortOperation {
    pub port: String,
    pub request: RequestTemplate,
    pub accepted_statuses: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<StatusGuard>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_bindings: Vec<ResponseBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_checks: Vec<ResponseCheck>,
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
    /// The frozen exploration catalog advertises the original GET/POST profile.
    /// Advanced HTTP operations require a separate approved functional plan.
    pub fn legacy_exploration_supported(&self) -> bool {
        matches!(self.request.method, Method::Get | Method::Post)
            && self.request.header_bindings.is_empty()
            && self.request.path_bindings.is_empty()
            && self.request.cookie_bindings.is_empty()
            && self.request.json_types.is_empty()
            && !self.request.cookies
            && self.response_bindings.is_empty()
            && self.response_checks.is_empty()
    }
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
                ..Default::default()
            }
            .validate()?;
            if !statuses_valid(&guard.statuses) {
                return Err("runtime_port_guard_invalid");
            }
        }
        let has_html = self
            .response_bindings
            .iter()
            .any(|capture| capture.html_text_id.is_some());
        if (has_html
            && (self
                .response_bindings
                .iter()
                .any(|capture| capture.html_text_id.is_none())
                || self
                    .response_checks
                    .iter()
                    .any(|check| check.header_name.is_none())))
            || self.response_bindings.len() > 8
            || self.response_checks.len() > 8
            || self
                .response_bindings
                .iter()
                .any(|capture| !capture_valid(capture))
            || self.response_checks.iter().any(|check| !check_valid(check))
        {
            return Err("runtime_port_response_binding_invalid");
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
    let mut captured = BTreeMap::new();
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
        for name in operation
            .request
            .binding_names()
            .chain(operation.response_checks.iter().map(|c| &c.binding))
            .chain(
                operation
                    .response_bindings
                    .iter()
                    .filter_map(|c| c.json_object_key.as_ref().map(|selector| &selector.binding)),
            )
        {
            if !(captured
                .get(name)
                .is_some_and(|(port, _)| port == &operation.port)
                || variables.iter().any(|v| {
                    v.name == *name
                        && v.phase == ExecutionPhase::Runtime
                        && !v.artifact_embedding
                        && v.resource == operation.port
                        && v.operation == ResourceOperation::Execute
                }))
            {
                return Err("runtime_port_input_unbound");
            }
        }
        for (field, kind) in &operation.request.json_types {
            let name = &operation.request.json_bindings[field];
            if captured
                .get(name)
                .is_some_and(|(_, captured_kind)| kind != captured_kind)
            {
                return Err("runtime_port_capture_scalar_mismatch");
            }
        }
        for check in &operation.response_checks {
            if captured.get(&check.binding).is_some_and(|(_, kind)| {
                check.scalar.is_some_and(|requested| requested != *kind)
                    || (check.header_name.is_some() && *kind != ScalarKind::String)
            }) {
                return Err("runtime_port_capture_scalar_mismatch");
            }
        }
        let mut names = BTreeSet::new();
        for capture in &operation.response_bindings {
            if variables.iter().any(|v| v.name == capture.binding)
                || !names.insert(&capture.binding)
                || captured
                    .insert(
                        capture.binding.clone(),
                        (operation.port.clone(), capture.scalar),
                    )
                    .is_some()
                || captured.len() > 16
            {
                return Err("runtime_port_response_binding_conflict");
            }
        }
    }
    Ok(())
}
