//! Strict `ato.capsule/2` authoring frontend for the portable Application v0
//! profile.
//!
//! This is deliberately a separate grammar from `ato.capsule/1`. The v1
//! parser still produces one `AuthoringDraft`; this parser produces one
//! Application, one Contract and N named route drafts. Route labels are
//! authoring metadata and are not part of a bound Derivation identity.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::authoring::{PROCESS_PROTOCOL, STATE_FILESYSTEM_PROTOCOL, WORKSPACE_PROTOCOL};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableDerivationDraftV2 {
    pub label: String,
    pub kind: PortableDerivationKindV2,
    pub runtimes: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub guest_port: u16,
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
    argv: Vec<String>,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    guest_port: u16,
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
    if document.effects.default != "pure" {
        return Err(invalid(
            "effects.default",
            "portable v0 supports only `pure`",
        ));
    }
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
            if derivation.argv.is_empty() || derivation.argv.iter().any(|item| item.contains('\0'))
            {
                return Err(invalid(
                    "derivation.argv",
                    "must be a non-empty NUL-free argv array",
                ));
            }
            if derivation.guest_port == 0 {
                return Err(invalid(
                    "derivation.guest_port",
                    "must be between 1 and 65535",
                ));
            }
            validate_relative(&derivation.cwd, "derivation.cwd")?;
            if derivation.env.iter().any(|(name, value)| {
                name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0')
            }) {
                return Err(invalid("derivation.env", "contains an invalid name or NUL"));
            }
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
                    let image = derivation
                        .runtimes
                        .get("oci.image")
                        .map(String::as_str)
                        .unwrap_or("");
                    let platform = derivation
                        .runtimes
                        .get("oci.platform")
                        .map(String::as_str)
                        .unwrap_or("");
                    if !image.contains("@sha256:")
                        || !matches!(platform, "linux/amd64" | "linux/arm64")
                    {
                        return Err(invalid(
                            "derivation.runtimes",
                            "OCI requires a digest-pinned image and linux/amd64 or linux/arm64",
                        ));
                    }
                    PortableDerivationKindV2::Oci
                }
                other => {
                    return Err(invalid(
                        "derivation.use",
                        format!(
                            "{other:?} is unsupported; use {PROCESS_PROTOCOL:?} or {OCI_PROTOCOL:?}"
                        ),
                    ));
                }
            };
            Ok(PortableDerivationDraftV2 {
                label: derivation.id,
                kind,
                runtimes: derivation.runtimes,
                argv: derivation.argv,
                cwd: derivation.cwd,
                env: derivation.env,
                guest_port: derivation.guest_port,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(PortableAuthoringDraftV2 {
        title: document.application.title,
        surface_path: document.application.surface_path,
        bindings,
        state,
        observations,
        derivations,
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
}
