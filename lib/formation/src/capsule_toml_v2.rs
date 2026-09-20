//! Strict `ato.capsule/2` authoring frontend for the portable Application v0
//! profile.
//!
//! This is deliberately a separate grammar from `ato.capsule/1`. The v1
//! parser still produces one `AuthoringDraft`; this parser produces one
//! Application, one Contract and N named route drafts. Route labels are
//! authoring metadata and are not part of a bound Derivation identity.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::authoring::{
    ClientAddressTransport, EffectClass, PROCESS_PROTOCOL, STATE_FILESYSTEM_PROTOCOL,
    TCP_EGRESS_PROTOCOL, WORKSPACE_PROTOCOL,
};

pub const CAPSULE_SCHEMA_V2: &str = "ato.capsule/2";
pub const OCI_PROTOCOL: &str = "ato.oci@1";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapsuleTomlV2Error {
    #[error("capsule.toml is not valid ato.capsule/2 TOML: {0}")]
    Syntax(String),
    #[error("capsule.toml v2 is invalid at {path}: {message}")]
    Invalid { path: String, message: String },
}

impl CapsuleTomlV2Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Syntax(_) => "capsule_toml_v2_syntax",
            Self::Invalid { .. } => "capsule_toml_v2_invalid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableAuthoringDraftV2 {
    pub title: String,
    pub surface_path: String,
    pub bindings: Vec<PortableBindingDraftV2>,
    pub state: Vec<PortableStateDraftV2>,
    pub observations: Vec<PortableHttpObservationDraftV2>,
    pub derivations: Vec<PortableDerivationDraftV2>,
    pub effects: EffectClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableBindingDraftV2 {
    pub id: String,
    pub protocol: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableStateDraftV2 {
    pub id: String,
    pub mount: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableHttpObservationDraftV2 {
    pub id: String,
    pub path: String,
    pub status: u16,
    pub body_digest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortableDerivationKindV2 {
    Process,
    Oci,
    /// Several `ato.oci@1` serving steps realized together on one Runner.
    OciServiceGroup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableDerivationDraftV2 {
    pub label: String,
    pub kind: PortableDerivationKindV2,
    pub runtimes: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    /// Zero for an OCI service group: each service names its own Ports.
    pub guest_port: u16,
    /// Empty unless `kind` is [`PortableDerivationKindV2::OciServiceGroup`].
    pub services: Vec<PortableServiceDraftV2>,
}

/// One serving step of an OCI service group. Authoring shorthand only: it
/// compiles to one `BoundStep` plus the `BoundPort`s it serves, never to a
/// Service object of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableServiceDraftV2 {
    pub id: String,
    pub runtimes: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub ports: Vec<PortableServicePortDraftV2>,
    pub state: Vec<String>,
    pub bindings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableServicePortDraftV2 {
    pub id: String,
    pub guest_port: u16,
    pub client_address_transport: Option<ClientAddressTransport>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    application: Application,
    contract: Contract,
    #[serde(default)]
    input: Vec<Input>,
    #[serde(default)]
    binding: Vec<Binding>,
    #[serde(default)]
    state: Vec<State>,
    derivation: Vec<Derivation>,
    effects: Effects,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Application {
    title: String,
    #[serde(default = "default_surface_path")]
    surface_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    #[serde(default)]
    mode: ContractMode,
    observation: Vec<HttpObservation>,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ContractMode {
    #[default]
    All,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpObservation {
    id: String,
    #[serde(rename = "use")]
    verifier: String,
    port: String,
    #[serde(default = "default_method")]
    method: String,
    path: String,
    status: u16,
    body_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    id: String,
    #[serde(rename = "use")]
    protocol: String,
    #[serde(default = "default_workspace_path")]
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    id: String,
    protocol: String,
    #[serde(default = "default_true")]
    required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    id: String,
    #[serde(rename = "use")]
    protocol: String,
    mount: String,
    access: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Derivation {
    id: String,
    #[serde(rename = "use")]
    protocol: String,
    runtimes: BTreeMap<String, String>,
    // Single-route fields. Absent exactly when `service` is present.
    argv: Option<Vec<String>>,
    cwd: Option<String>,
    env: Option<BTreeMap<String, String>>,
    guest_port: Option<u16>,
    #[serde(default)]
    service: Vec<Service>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    id: String,
    runtimes: BTreeMap<String, String>,
    argv: Vec<String>,
    #[serde(default = "default_workspace_path")]
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    ports: Vec<ServicePort>,
    #[serde(default)]
    state: Vec<String>,
    #[serde(default)]
    bindings: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServicePort {
    id: String,
    guest_port: u16,
    /// Optional Port transport metadata. Omitting it leaves the Contract's
    /// canonical bytes exactly as they were before this existed.
    #[serde(default)]
    client_address_transport: Option<ClientAddressTransport>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Effects {
    default: String,
}

fn default_surface_path() -> String {
    "/".to_owned()
}

fn default_method() -> String {
    "GET".to_owned()
}

fn default_workspace_path() -> String {
    ".".to_owned()
}

fn default_true() -> bool {
    true
}

fn invalid(path: impl Into<String>, message: impl Into<String>) -> CapsuleTomlV2Error {
    CapsuleTomlV2Error::Invalid {
        path: path.into(),
        message: message.into(),
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' => true,
            b'0'..=b'9' | b'.' | b'_' | b'-' => index > 0,
            _ => false,
        })
}

fn validate_id(path: &str, value: &str) -> Result<(), CapsuleTomlV2Error> {
    if valid_id(value) {
        Ok(())
    } else {
        Err(invalid(path, "must match [a-z][a-z0-9._-]{0,127}"))
    }
}

fn validate_relative(path: &str, field: &str) -> Result<(), CapsuleTomlV2Error> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\0')
        || path.split('/').any(|segment| segment == "..")
    {
        return Err(invalid(
            field,
            "must be a non-empty relative path without `..`",
        ));
    }
    Ok(())
}

/// Parse the deliberately bounded portable v0 authoring grammar.
pub fn parse_capsule_toml_v2(text: &str) -> Result<PortableAuthoringDraftV2, CapsuleTomlV2Error> {
    let document: Document =
        toml::from_str(text).map_err(|error| CapsuleTomlV2Error::Syntax(error.to_string()))?;
    if document.schema != CAPSULE_SCHEMA_V2 {
        return Err(invalid("schema", format!("must be {CAPSULE_SCHEMA_V2:?}")));
    }
    if document.application.title.trim().is_empty() {
        return Err(invalid("application.title", "must not be empty"));
    }
    if !document.application.surface_path.starts_with('/') {
        return Err(invalid(
            "application.surface_path",
            "must be an absolute request path",
        ));
    }
    let effects = match document.effects.default.as_str() {
        "pure" => EffectClass::Pure,
        "requires-confirmation" => EffectClass::RequiresConfirmation,
        "non-repeatable" => EffectClass::NonRepeatable,
        _ => {
            return Err(invalid(
                "effects.default",
                "must be `pure`, `requires-confirmation`, or `non-repeatable`",
            ));
        }
    };
    if document.input.len() != 1 {
        return Err(invalid(
            "input",
            "portable v0 requires exactly one workspace input",
        ));
    }
    let input = &document.input[0];
    validate_id("input.id", &input.id)?;
    if input.id != "workspace" || input.protocol != WORKSPACE_PROTOCOL || input.path != "." {
        return Err(invalid(
            "input",
            format!("must declare id=workspace, use={WORKSPACE_PROTOCOL:?}, path=\".\""),
        ));
    }
    if document.state.len() > 1 {
        return Err(invalid(
            "state",
            "portable v0 supports at most one filesystem state",
        ));
    }

    let mut binding_ids = BTreeSet::new();
    let bindings = document
        .binding
        .into_iter()
        .map(|binding| {
            validate_id("binding.id", &binding.id)?;
            if !binding_ids.insert(binding.id.clone()) {
                return Err(invalid(
                    "binding.id",
                    format!("duplicate id {:?}", binding.id),
                ));
            }
            if binding.protocol.trim().is_empty() || binding.protocol.contains('\0') {
                return Err(invalid("binding.protocol", "must be a non-empty protocol"));
            }
            Ok(PortableBindingDraftV2 {
                id: binding.id,
                protocol: binding.protocol,
                required: binding.required,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let has_tcp_egress = bindings
        .iter()
        .any(|binding| binding.protocol == TCP_EGRESS_PROTOCOL);
    if has_tcp_egress
        && !matches!(
            effects,
            EffectClass::RequiresConfirmation | EffectClass::NonRepeatable
        )
    {
        return Err(invalid(
            "effects.default",
            "a tcp-egress Binding requires `requires-confirmation` or `non-repeatable`",
        ));
    }
    if !has_tcp_egress && effects != EffectClass::Pure {
        return Err(invalid(
            "effects.default",
            "must be `pure` when no tcp-egress Binding is declared",
        ));
    }

    let state = document
        .state
        .into_iter()
        .map(|state| {
            validate_id("state.id", &state.id)?;
            if state.protocol != STATE_FILESYSTEM_PROTOCOL || state.access != "read-write" {
                return Err(invalid(
                    "state",
                    format!("must use {STATE_FILESYSTEM_PROTOCOL:?} with access=\"read-write\""),
                ));
            }
            if !state.mount.starts_with('/')
                || state.mount == "/app"
                || state
                    .mount
                    .split('/')
                    .any(|segment| matches!(segment, "." | ".."))
            {
                return Err(invalid("state.mount", "must be an absolute non-/app mount"));
            }
            Ok(PortableStateDraftV2 {
                id: state.id,
                mount: state.mount,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let state_ids = state
        .iter()
        .map(|state| state.id.clone())
        .collect::<BTreeSet<_>>();

    if document.contract.mode != ContractMode::All || document.contract.observation.is_empty() {
        return Err(invalid(
            "contract",
            "portable v0 requires non-empty mode=all observations",
        ));
    }
    let mut observation_ids = BTreeSet::new();
    let observations = document
        .contract
        .observation
        .into_iter()
        .map(|observation| {
            validate_id("contract.observation.id", &observation.id)?;
            if !observation_ids.insert(observation.id.clone()) {
                return Err(invalid(
                    "contract.observation.id",
                    format!("duplicate id {:?}", observation.id),
                ));
            }
            if observation.verifier != "ato.contract.http@1"
                || observation.port != "app.http"
                || observation.method != "GET"
            {
                return Err(invalid(
                    "contract.observation",
                    "portable v0 supports GET ato.contract.http@1 on app.http",
                ));
            }
            if !observation.path.starts_with('/') || !(100..=599).contains(&observation.status) {
                return Err(invalid(
                    "contract.observation",
                    "path or HTTP status is invalid",
                ));
            }
            if let Some(digest) = &observation.body_digest
                && !is_sha256_ref(digest)
            {
                return Err(invalid(
                    "contract.observation.body_digest",
                    "must be sha256:<64 lowercase hex>",
                ));
            }
            Ok(PortableHttpObservationDraftV2 {
                id: observation.id,
                path: observation.path,
                status: observation.status,
                body_digest: observation.body_digest,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if document.derivation.is_empty() || document.derivation.len() > 16 {
        return Err(invalid(
            "derivation",
            "must contain between 1 and 16 routes",
        ));
    }
    let mut derivation_ids = BTreeSet::new();
    let derivations = document
        .derivation
        .into_iter()
        .map(|derivation| {
            validate_id("derivation.id", &derivation.id)?;
            if !derivation_ids.insert(derivation.id.clone()) {
                return Err(invalid(
                    "derivation.id",
                    format!("duplicate id {:?}", derivation.id),
                ));
            }
            if derivation.service.is_empty() {
                parse_single_route(derivation)
            } else {
                parse_service_group(derivation, &state_ids, &binding_ids)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(PortableAuthoringDraftV2 {
        title: document.application.title,
        surface_path: document.application.surface_path,
        bindings,
        state,
        observations,
        derivations,
        effects,
    })
}

fn validate_argv(field: &str, argv: &[String]) -> Result<(), CapsuleTomlV2Error> {
    if argv.is_empty() || argv.iter().any(|item| item.contains('\0')) {
        return Err(invalid(field, "must be a non-empty NUL-free argv array"));
    }
    Ok(())
}

fn validate_env(field: &str, env: &BTreeMap<String, String>) -> Result<(), CapsuleTomlV2Error> {
    if env.iter().any(|(name, value)| {
        name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0')
    }) {
        return Err(invalid(field, "contains an invalid name or NUL"));
    }
    Ok(())
}

fn validate_oci_image(field: &str, image: Option<&String>) -> Result<(), CapsuleTomlV2Error> {
    if !image.is_some_and(|image| image.contains("@sha256:")) {
        return Err(invalid(field, "OCI requires a digest-pinned image"));
    }
    Ok(())
}

fn validate_oci_platform(field: &str, platform: Option<&String>) -> Result<(), CapsuleTomlV2Error> {
    if !platform.is_some_and(|platform| matches!(platform.as_str(), "linux/amd64" | "linux/arm64"))
    {
        return Err(invalid(field, "OCI requires linux/amd64 or linux/arm64"));
    }
    Ok(())
}

fn parse_single_route(
    derivation: Derivation,
) -> Result<PortableDerivationDraftV2, CapsuleTomlV2Error> {
    let argv = derivation.argv.unwrap_or_default();
    let cwd = derivation.cwd.unwrap_or_default();
    let env = derivation.env.unwrap_or_default();
    let guest_port = derivation.guest_port.unwrap_or(0);
    validate_argv("derivation.argv", &argv)?;
    if guest_port == 0 {
        return Err(invalid(
            "derivation.guest_port",
            "must be between 1 and 65535",
        ));
    }
    validate_relative(&cwd, "derivation.cwd")?;
    validate_env("derivation.env", &env)?;
    let kind = match derivation.protocol.as_str() {
        PROCESS_PROTOCOL => {
            if derivation
                .runtimes
                .get("python")
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(invalid(
                    "derivation.runtimes.python",
                    "is required for a process route",
                ));
            }
            PortableDerivationKindV2::Process
        }
        OCI_PROTOCOL => {
            validate_oci_image("derivation.runtimes", derivation.runtimes.get("oci.image"))
                .and_then(|()| {
                    validate_oci_platform(
                        "derivation.runtimes",
                        derivation.runtimes.get("oci.platform"),
                    )
                })
                .map_err(|_| {
                    invalid(
                        "derivation.runtimes",
                        "OCI requires a digest-pinned image and linux/amd64 or linux/arm64",
                    )
                })?;
            PortableDerivationKindV2::Oci
        }
        other => {
            return Err(invalid(
                "derivation.use",
                format!("{other:?} is unsupported; use {PROCESS_PROTOCOL:?} or {OCI_PROTOCOL:?}"),
            ));
        }
    };
    Ok(PortableDerivationDraftV2 {
        label: derivation.id,
        kind,
        runtimes: derivation.runtimes,
        argv,
        cwd,
        env,
        guest_port,
        services: Vec::new(),
    })
}

/// Grammar and reference integrity of an OCI service group. Resource budgets
/// and the Surface/Port exposure rules are the portable profile's to judge:
/// they are checked where the bound Derivation is validated, so a bundle
/// built by any frontend meets the same bar.
fn parse_service_group(
    derivation: Derivation,
    state_ids: &BTreeSet<String>,
    binding_ids: &BTreeSet<String>,
) -> Result<PortableDerivationDraftV2, CapsuleTomlV2Error> {
    if derivation.argv.is_some()
        || derivation.cwd.is_some()
        || derivation.env.is_some()
        || derivation.guest_port.is_some()
    {
        return Err(invalid(
            "derivation",
            "argv, cwd, env and guest_port belong to each service when `service` is declared",
        ));
    }
    if derivation.protocol != OCI_PROTOCOL {
        return Err(invalid(
            "derivation.use",
            format!("services require {OCI_PROTOCOL:?}"),
        ));
    }
    if derivation.runtimes.len() != 1 {
        return Err(invalid(
            "derivation.runtimes",
            "a service group declares only the shared oci.platform",
        ));
    }
    validate_oci_platform(
        "derivation.runtimes",
        derivation.runtimes.get("oci.platform"),
    )?;

    let mut service_ids = BTreeSet::new();
    let mut port_ids = BTreeSet::new();
    let mut state_users = BTreeMap::<String, usize>::new();
    let mut binding_users = BTreeMap::<String, usize>::new();
    let services = derivation
        .service
        .into_iter()
        .map(|service| {
            validate_id("derivation.service.id", &service.id)?;
            if !service_ids.insert(service.id.clone()) {
                return Err(invalid(
                    "derivation.service.id",
                    format!("duplicate id {:?}", service.id),
                ));
            }
            validate_argv("derivation.service.argv", &service.argv)?;
            validate_relative(&service.cwd, "derivation.service.cwd")?;
            validate_env("derivation.service.env", &service.env)?;
            validate_oci_image(
                "derivation.service.runtimes",
                service.runtimes.get("oci.image"),
            )?;
            if service.runtimes.contains_key("oci.platform") {
                return Err(invalid(
                    "derivation.service.runtimes",
                    "oci.platform is shared and belongs to the derivation",
                ));
            }
            if service.ports.is_empty() {
                return Err(invalid(
                    "derivation.service.ports",
                    "every service serves at least one Port",
                ));
            }
            let ports = service
                .ports
                .into_iter()
                .map(|port| {
                    validate_id("derivation.service.ports.id", &port.id)?;
                    if !port_ids.insert(port.id.clone()) {
                        return Err(invalid(
                            "derivation.service.ports.id",
                            format!("duplicate Port {:?}", port.id),
                        ));
                    }
                    if port.guest_port == 0 {
                        return Err(invalid(
                            "derivation.service.ports.guest_port",
                            "must be between 1 and 65535",
                        ));
                    }
                    Ok(PortableServicePortDraftV2 {
                        id: port.id,
                        guest_port: port.guest_port,
                        client_address_transport: port.client_address_transport,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            for (field, references, known, users) in [
                (
                    "derivation.service.state",
                    &service.state,
                    state_ids,
                    &mut state_users,
                ),
                (
                    "derivation.service.bindings",
                    &service.bindings,
                    binding_ids,
                    &mut binding_users,
                ),
            ] {
                let mut seen = BTreeSet::new();
                for reference in references {
                    if !known.contains(reference) || !seen.insert(reference) {
                        return Err(invalid(
                            field,
                            format!("{reference:?} is undeclared or repeated"),
                        ));
                    }
                    *users.entry(reference.clone()).or_default() += 1;
                }
            }
            Ok(PortableServiceDraftV2 {
                id: service.id,
                runtimes: service.runtimes,
                argv: service.argv,
                cwd: service.cwd,
                env: service.env,
                ports,
                state: service.state,
                bindings: service.bindings,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Visibility is scoped per service, so every slot and every secret has
    // exactly one reader. Unreferenced ones would be declared but reachable
    // by nobody; shared ones would widen visibility past one service.
    for (field, known, users) in [
        ("state", state_ids, &state_users),
        ("binding", binding_ids, &binding_users),
    ] {
        if let Some(id) = known.iter().find(|id| users.get(*id) != Some(&1)) {
            return Err(invalid(
                field,
                format!("{id:?} must be used by exactly one service"),
            ));
        }
    }
    if !port_ids.contains("app.http") {
        return Err(invalid(
            "derivation.service.ports",
            "exactly one service must serve the app.http Surface Port",
        ));
    }
    Ok(PortableDerivationDraftV2 {
        label: derivation.id,
        kind: PortableDerivationKindV2::OciServiceGroup,
        runtimes: derivation.runtimes,
        argv: Vec::new(),
        cwd: String::new(),
        env: BTreeMap::new(),
        guest_port: 0,
        services,
    })
}

fn is_sha256_ref(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_PROCESS_ROUTES: &str = r#"
schema = "ato.capsule/2"

[application]
title = "Authored echo"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[derivation]]
id = "python-a"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }
env = { ROUTE = "a" }

[[derivation]]
id = "python-b"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }
env = { ROUTE = "b" }

[effects]
default = "pure"
"#;

    #[test]
    fn parses_multiple_routes_without_putting_labels_in_the_route_body() {
        let draft = parse_capsule_toml_v2(TWO_PROCESS_ROUTES).unwrap();
        assert_eq!(draft.derivations.len(), 2);
        assert_eq!(draft.derivations[0].label, "python-a");
        assert_eq!(draft.derivations[1].env["ROUTE"], "b");
    }

    #[test]
    fn refuses_unknown_fields_and_duplicate_route_labels() {
        assert_eq!(
            parse_capsule_toml_v2(&TWO_PROCESS_ROUTES.replace(
                "surface_path = \"/\"",
                "surface_path = \"/\"\nignored = true",
            ))
            .unwrap_err()
            .code(),
            "capsule_toml_v2_syntax",
        );
        assert!(
            parse_capsule_toml_v2(&TWO_PROCESS_ROUTES.replace("python-b", "python-a"))
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn v2_never_accepts_an_unpinned_oci_image() {
        let oci = TWO_PROCESS_ROUTES
            .replace("use = \"ato.process@1\"", "use = \"ato.oci@1\"")
            .replace("runtimes = { python = \"3.12\" }", "runtimes = { \"oci.image\" = \"python:latest\", \"oci.platform\" = \"linux/amd64\" }");
        assert!(
            parse_capsule_toml_v2(&oci)
                .unwrap_err()
                .to_string()
                .contains("digest-pinned")
        );
    }

    const SERVICE_GROUP: &str = r#"
schema = "ato.capsule/2"

[application]
title = "Group"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[binding]]
id = "admin_secret"
protocol = "ato.secret@1"

[[state]]
id = "data"
use = "ato.state.filesystem@1"
mount = "/data"
access = "read-write"

[contract]
[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[derivation]]
id = "group"
use = "ato.oci@1"
runtimes = { "oci.platform" = "linux/amd64" }

[[derivation.service]]
id = "backend"
runtimes = { "oci.image" = "example/backend@sha256:1111111111111111111111111111111111111111111111111111111111111111" }
argv = ["serve"]
ports = [{ id = "backend.http", guest_port = 80 }]
state = ["data"]
bindings = ["admin_secret"]

[[derivation.service]]
id = "web"
runtimes = { "oci.image" = "example/web@sha256:2222222222222222222222222222222222222222222222222222222222222222" }
argv = ["web"]
ports = [{ id = "app.http", guest_port = 8080 }]

[effects]
default = "pure"
"#;

    #[test]
    fn parses_a_service_group_as_one_route_with_scoped_references() {
        let draft = parse_capsule_toml_v2(SERVICE_GROUP).unwrap();
        let [route] = draft.derivations.as_slice() else {
            panic!("one route");
        };
        assert_eq!(route.kind, PortableDerivationKindV2::OciServiceGroup);
        assert_eq!(route.guest_port, 0);
        assert!(route.argv.is_empty());
        assert_eq!(route.services[0].state, ["data"]);
        assert_eq!(route.services[0].bindings, ["admin_secret"]);
        assert_eq!(route.services[0].cwd, ".");
        assert_eq!(route.services[1].ports[0].id, "app.http");
        assert_eq!(draft.effects, EffectClass::Pure);
    }

    #[test]
    fn couples_tcp_egress_to_an_explicit_external_effect() {
        let egress = SERVICE_GROUP
            .replace(
                "protocol = \"ato.secret@1\"",
                "protocol = \"ato.tcp-egress@1\"",
            )
            .replace("default = \"pure\"", "default = \"requires-confirmation\"");
        let draft = parse_capsule_toml_v2(&egress).unwrap();
        assert_eq!(draft.effects, EffectClass::RequiresConfirmation);

        assert!(
            parse_capsule_toml_v2(
                &egress.replace("default = \"requires-confirmation\"", "default = \"pure\"",)
            )
            .is_err()
        );
        assert!(
            parse_capsule_toml_v2(
                &SERVICE_GROUP.replace("default = \"pure\"", "default = \"non-repeatable\"",)
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_service_groups_that_widen_or_drop_visibility() {
        let refused = [
            ("route-level argv", SERVICE_GROUP.replace(
                "runtimes = { \"oci.platform\" = \"linux/amd64\" }",
                "runtimes = { \"oci.platform\" = \"linux/amd64\" }\nargv = [\"x\"]",
            )),
            ("route-level image", SERVICE_GROUP.replace(
                "{ \"oci.platform\" = \"linux/amd64\" }",
                "{ \"oci.platform\" = \"linux/amd64\", \"oci.image\" = \"x@sha256:1\" }",
            )),
            ("process services", SERVICE_GROUP.replace("use = \"ato.oci@1\"", "use = \"ato.process@1\"")),
            ("service platform", SERVICE_GROUP.replace(
                "example/web@sha256:2222222222222222222222222222222222222222222222222222222222222222\"",
                "example/web@sha256:2222222222222222222222222222222222222222222222222222222222222222\", \"oci.platform\" = \"linux/amd64\"",
            )),
            ("unpinned image", SERVICE_GROUP.replace(
                "example/web@sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "example/web:latest",
            )),
            ("shared state", SERVICE_GROUP.replace("argv = [\"web\"]", "argv = [\"web\"]\nstate = [\"data\"]")),
            ("unused state", SERVICE_GROUP.replace("state = [\"data\"]\n", "")),
            ("shared Binding", SERVICE_GROUP.replace(
                "argv = [\"web\"]",
                "argv = [\"web\"]\nbindings = [\"admin_secret\"]",
            )),
            ("unused Binding", SERVICE_GROUP.replace("bindings = [\"admin_secret\"]\n", "")),
            ("undeclared state", SERVICE_GROUP.replace("state = [\"data\"]", "state = [\"data\", \"ghost\"]")),
            ("duplicate Port", SERVICE_GROUP.replace("backend.http", "app.http")),
            ("no Surface Port", SERVICE_GROUP.replace("id = \"app.http\"", "id = \"web.http\"")),
            ("duplicate service", SERVICE_GROUP.replace("id = \"web\"", "id = \"backend\"")),
            ("unknown service field", SERVICE_GROUP.replace("argv = [\"web\"]", "argv = [\"web\"]\nprivileged = true")),
            ("service without Ports", SERVICE_GROUP.replace(
                "ports = [{ id = \"backend.http\", guest_port = 80 }]",
                "ports = []",
            )),
        ];
        parse_capsule_toml_v2(SERVICE_GROUP).expect("baseline group parses");
        for (name, toml) in refused {
            assert!(parse_capsule_toml_v2(&toml).is_err(), "{name} was accepted");
        }
    }
}
