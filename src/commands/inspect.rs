use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Error as AnyhowError;
use capsule_core::manifest;
use capsule_core::types::{
    CapsuleManifest, EgressIdType, ServiceSpec, StateAttach, StateDurability, StateKind,
};
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;

const REQUIREMENTS_SCHEMA_VERSION: &str = "1";
const NETWORK_REQUIREMENT_KEY: &str = "external-network";
const CONSENT_FILESYSTEM_WRITE_KEY: &str = "filesystem.write";
const CONSENT_NETWORK_EGRESS_KEY: &str = "network.egress";
const CONSENT_SECRETS_ACCESS_KEY: &str = "secrets.access";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectRequirementsResult {
    pub schema_version: &'static str,
    pub target: InspectTarget,
    pub requirements: RequirementCategories,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectTarget {
    pub input: String,
    pub kind: &'static str,
    pub resolved: ResolvedTarget,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ResolvedTarget {
    Local { path: String },
    Remote { publisher: String, slug: String },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RequirementCategories {
    pub secrets: Vec<SecretRequirement>,
    pub state: Vec<StateRequirementItem>,
    pub env: Vec<EnvRequirement>,
    pub network: Vec<NetworkRequirement>,
    pub services: Vec<ServiceRequirement>,
    pub consent: Vec<ConsentRequirement>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateRequirementItem {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<StateKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability: Option<StateDurability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attach: Option<StateAttach>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<NetworkIdentity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkIdentity {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConsentRequirement {
    pub key: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct InspectRequirementsError {
    code: &'static str,
    message: String,
    details: Value,
}

#[derive(Debug, Serialize)]
struct InspectRequirementsErrorEnvelope<'a> {
    error: InspectRequirementsErrorPayload<'a>,
}

#[derive(Debug, Serialize)]
struct InspectRequirementsErrorPayload<'a> {
    code: &'a str,
    message: &'a str,
    details: &'a Value,
}

struct ResolvedInspection {
    target: InspectTarget,
    manifest: CapsuleManifest,
}

#[derive(Debug, Default)]
struct EnvRequirementAccumulator {
    required: bool,
    required_targets: BTreeSet<String>,
    allowlisted: bool,
}

pub async fn execute_requirements(
    input: String,
    registry: Option<String>,
    json_output: bool,
) -> Result<InspectRequirementsResult, InspectRequirementsError> {
    let resolved = resolve_target(&input, registry.as_deref()).await?;
    let result = InspectRequirementsResult {
        schema_version: REQUIREMENTS_SCHEMA_VERSION,
        target: resolved.target,
        requirements: build_requirements(&resolved.manifest),
    };

    if json_output {
        let payload = serde_json::to_string_pretty(&result).map_err(|err| {
            InspectRequirementsError::requirements_resolution_failed(
                &input,
                format!("Failed to serialize requirements JSON: {err}"),
            )
        })?;
        println!("{payload}");
    } else {
        print_human_readable(&result);
    }

    Ok(result)
}

pub fn try_emit_json_error(err: &AnyhowError) -> bool {
    let Some(inspect_err) = err.downcast_ref::<InspectRequirementsError>() else {
        return false;
    };

    inspect_err.emit_json();
    true
}

impl InspectRequirementsError {
    fn target_not_found(input: &str, reason: impl Into<String>) -> Self {
        Self {
            code: "TARGET_NOT_FOUND",
            message: "Could not resolve target".to_string(),
            details: json!({
                "input": input,
                "reason": reason.into(),
            }),
        }
    }

    fn capsule_toml_not_found(input: &str, reason: impl Into<String>) -> Self {
        Self {
            code: "CAPSULE_TOML_NOT_FOUND",
            message: "capsule.toml was not found".to_string(),
            details: json!({
                "input": input,
                "reason": reason.into(),
            }),
        }
    }

    fn requirements_resolution_failed(input: &str, reason: impl Into<String>) -> Self {
        Self {
            code: "REQUIREMENTS_RESOLUTION_FAILED",
            message: "Could not resolve requirements from capsule.toml".to_string(),
            details: json!({
                "input": input,
                "reason": reason.into(),
            }),
        }
    }

    fn emit_json(&self) {
        let payload = InspectRequirementsErrorEnvelope {
            error: InspectRequirementsErrorPayload {
                code: self.code,
                message: &self.message,
                details: &self.details,
            },
        };

        if let Ok(serialized) = serde_json::to_string(&payload) {
            eprintln!("{serialized}");
        }
    }
}

async fn resolve_target(
    input: &str,
    registry: Option<&str>,
) -> Result<ResolvedInspection, InspectRequirementsError> {
    let expanded_path = expand_local_path(input);
    let should_treat_as_local = expanded_path.exists() || is_explicit_local_path_input(input);
    if should_treat_as_local {
        return resolve_local_target(input, &expanded_path);
    }

    resolve_remote_target(input, registry).await
}

fn resolve_local_target(
    input: &str,
    expanded_path: &Path,
) -> Result<ResolvedInspection, InspectRequirementsError> {
    if !expanded_path.exists() {
        return Err(InspectRequirementsError::target_not_found(
            input,
            format!("Local path does not exist: {}", expanded_path.display()),
        ));
    }

    let resolved_path = expanded_path.canonicalize().map_err(|err| {
        InspectRequirementsError::target_not_found(
            input,
            format!(
                "Failed to resolve local path '{}': {err}",
                expanded_path.display()
            ),
        )
    })?;

    let manifest_path = if resolved_path.is_dir() {
        resolved_path.join("capsule.toml")
    } else {
        resolved_path.clone()
    };
    if !manifest_path.exists() {
        return Err(InspectRequirementsError::capsule_toml_not_found(
            input,
            format!("Expected manifest at {}", manifest_path.display()),
        ));
    }

    let loaded = manifest::load_manifest(&manifest_path).map_err(|err| {
        InspectRequirementsError::requirements_resolution_failed(input, err.to_string())
    })?;

    Ok(ResolvedInspection {
        target: InspectTarget {
            input: input.to_string(),
            kind: "local",
            resolved: ResolvedTarget::Local {
                path: resolved_path.display().to_string(),
            },
        },
        manifest: loaded.model,
    })
}

async fn resolve_remote_target(
    input: &str,
    registry: Option<&str>,
) -> Result<ResolvedInspection, InspectRequirementsError> {
    let scoped_ref = crate::install::parse_capsule_ref(input).map_err(|err| {
        InspectRequirementsError::target_not_found(input, format!("Invalid remote ref: {err}"))
    })?;
    let manifest_toml = crate::install::fetch_capsule_manifest_toml(input, registry)
        .await
        .map_err(|err| classify_remote_error(input, err))?;
    let manifest = parse_remote_manifest(input, &manifest_toml)?;

    Ok(ResolvedInspection {
        target: InspectTarget {
            input: input.to_string(),
            kind: "remote",
            resolved: ResolvedTarget::Remote {
                publisher: scoped_ref.publisher,
                slug: scoped_ref.slug,
            },
        },
        manifest,
    })
}

fn parse_remote_manifest(
    input: &str,
    manifest_toml: &str,
) -> Result<CapsuleManifest, InspectRequirementsError> {
    let mut manifest = CapsuleManifest::from_toml(manifest_toml).map_err(|err| {
        InspectRequirementsError::requirements_resolution_failed(
            input,
            format!("Failed to parse remote capsule.toml: {err}"),
        )
    })?;

    if let Err(errors) = manifest.validate() {
        let details = errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(InspectRequirementsError::requirements_resolution_failed(
            input,
            format!("Remote capsule.toml validation failed: {details}"),
        ));
    }

    if manifest.schema_version.trim().is_empty() {
        manifest.schema_version = "0.2".to_string();
    }

    Ok(manifest)
}

fn classify_remote_error(input: &str, err: anyhow::Error) -> InspectRequirementsError {
    let message = err.to_string();
    if message.contains("Capsule not found") {
        InspectRequirementsError::target_not_found(input, message)
    } else if message.contains("capsule.toml") {
        InspectRequirementsError::capsule_toml_not_found(input, message)
    } else {
        InspectRequirementsError::requirements_resolution_failed(input, message)
    }
}

fn build_requirements(manifest: &CapsuleManifest) -> RequirementCategories {
    let (secrets, env) = build_env_requirements(manifest);
    let state = build_state_requirements(manifest);
    let network = build_network_requirements(manifest);
    let services = build_service_requirements(manifest);
    let consent = build_consent_requirements(&secrets, &state, &network);

    RequirementCategories {
        secrets,
        state,
        env,
        network,
        services,
        consent,
    }
}

fn build_env_requirements(
    manifest: &CapsuleManifest,
) -> (Vec<SecretRequirement>, Vec<EnvRequirement>) {
    let mut entries = BTreeMap::<String, EnvRequirementAccumulator>::new();

    if let Some(targets) = manifest.targets.as_ref() {
        for (target_label, target) in &targets.named {
            for key in &target.required_env {
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                let entry = entries.entry(key.to_string()).or_default();
                entry.required = true;
                entry.required_targets.insert(target_label.clone());
            }
        }
    }

    if let Some(isolation) = manifest.isolation.as_ref() {
        for key in &isolation.allow_env {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            entries.entry(key.to_string()).or_default().allowlisted = true;
        }
    }

    let mut secrets = Vec::new();
    let mut env = Vec::new();

    for (key, entry) in entries {
        let description = env_requirement_description(&entry);
        if is_secret_like_key(&key) {
            secrets.push(SecretRequirement {
                key,
                required: entry.required,
                description,
            });
        } else {
            env.push(EnvRequirement {
                key,
                required: entry.required,
                description,
            });
        }
    }

    (secrets, env)
}

fn build_state_requirements(manifest: &CapsuleManifest) -> Vec<StateRequirementItem> {
    let mut items = manifest
        .state
        .iter()
        .map(|(key, requirement)| StateRequirementItem {
            key: key.clone(),
            required: true,
            description: None,
            kind: Some(requirement.kind),
            durability: Some(requirement.durability),
            purpose: (!requirement.purpose.trim().is_empty()).then(|| requirement.purpose.clone()),
            attach: Some(requirement.attach),
            schema_id: requirement.schema_id.clone(),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.key.cmp(&right.key));
    items
}

fn build_network_requirements(manifest: &CapsuleManifest) -> Vec<NetworkRequirement> {
    let Some(network) = manifest.network.as_ref() else {
        return Vec::new();
    };

    let mut hosts = network
        .egress_allow
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    hosts.sort();
    hosts.dedup();

    let mut identities = network
        .egress_id_allow
        .iter()
        .map(|rule| NetworkIdentity {
            kind: egress_id_type_as_str(&rule.rule_type).to_string(),
            value: rule.value.clone(),
        })
        .collect::<Vec<_>>();
    identities.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then(left.value.cmp(&right.value))
    });

    if hosts.is_empty() && identities.is_empty() {
        return Vec::new();
    }

    vec![NetworkRequirement {
        key: NETWORK_REQUIREMENT_KEY.to_string(),
        required: true,
        description: Some("Requires outbound network access".to_string()),
        hosts,
        identities,
    }]
}

fn build_service_requirements(manifest: &CapsuleManifest) -> Vec<ServiceRequirement> {
    let Some(services) = manifest.services.as_ref() else {
        return Vec::new();
    };

    let mut items = services
        .iter()
        .map(|(key, service)| ServiceRequirement {
            key: key.clone(),
            required: true,
            description: Some(service_description(key, service)),
            target: service.target.clone(),
            depends_on: service
                .depends_on
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|dependency| !dependency.trim().is_empty())
                .collect(),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.key.cmp(&right.key));
    items
}

fn build_consent_requirements(
    secrets: &[SecretRequirement],
    state: &[StateRequirementItem],
    network: &[NetworkRequirement],
) -> Vec<ConsentRequirement> {
    let mut items = Vec::new();

    if !network.is_empty() {
        items.push(ConsentRequirement {
            key: CONSENT_NETWORK_EGRESS_KEY.to_string(),
            required: true,
            description: Some("Requires consent for outbound network access".to_string()),
        });
    }

    if !state.is_empty() {
        items.push(ConsentRequirement {
            key: CONSENT_FILESYSTEM_WRITE_KEY.to_string(),
            required: true,
            description: Some("Writes files to bound application state".to_string()),
        });
    }

    if !secrets.is_empty() {
        items.push(ConsentRequirement {
            key: CONSENT_SECRETS_ACCESS_KEY.to_string(),
            required: true,
            description: Some("Requires secret provisioning before launch".to_string()),
        });
    }

    items
}

fn env_requirement_description(entry: &EnvRequirementAccumulator) -> Option<String> {
    if entry.required {
        if entry.required_targets.is_empty() {
            return Some("Required environment variable declared in capsule.toml".to_string());
        }

        return Some(format!(
            "Required environment variable for target(s): {}",
            entry
                .required_targets
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if entry.allowlisted {
        return Some("Optional host environment variable passthrough".to_string());
    }

    None
}

fn is_secret_like_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || upper.ends_with("_KEY")
}

fn service_description(name: &str, service: &ServiceSpec) -> String {
    if name == "main" {
        return "Primary runtime service".to_string();
    }
    if let Some(target) = service
        .target
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return format!("Service declared in capsule.toml targeting '{target}'");
    }
    "Service declared in capsule.toml".to_string()
}

fn egress_id_type_as_str(value: &EgressIdType) -> &'static str {
    match value {
        EgressIdType::Ip => "ip",
        EgressIdType::Cidr => "cidr",
        EgressIdType::Spiffe => "spiffe",
    }
}

fn print_human_readable(result: &InspectRequirementsResult) {
    println!("Requirements for {}", result.target.input);
    print_category(
        "Secrets",
        result.requirements.secrets.iter().map(|item| &item.key),
    );
    print_category(
        "State",
        result.requirements.state.iter().map(|item| &item.key),
    );
    print_category("Env", result.requirements.env.iter().map(|item| &item.key));
    print_category(
        "Network",
        result.requirements.network.iter().map(|item| &item.key),
    );
    print_category(
        "Services",
        result.requirements.services.iter().map(|item| &item.key),
    );
    print_category(
        "Consent",
        result.requirements.consent.iter().map(|item| &item.key),
    );
}

fn print_category<'a>(label: &str, values: impl Iterator<Item = &'a String>) {
    let values = values.cloned().collect::<Vec<_>>();
    if values.is_empty() {
        println!("  {label}: none");
    } else {
        println!("  {label}: {}", values.join(", "));
    }
}

fn expand_local_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn is_explicit_local_path_input(raw: &str) -> bool {
    if raw.is_empty() {
        return false;
    }
    if raw == "." || raw == ".." {
        return true;
    }
    if raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with(".\\")
        || raw.starts_with("..\\")
        || raw.starts_with("~/")
        || raw.starts_with("~\\")
        || raw.starts_with('/')
        || raw.starts_with('\\')
    {
        return true;
    }

    // Check for Windows absolute path: "C:\path" or "C:/path".
    raw.len() >= 3
        && raw.as_bytes()[1] == b':'
        && (raw.as_bytes()[2] == b'/' || raw.as_bytes()[2] == b'\\')
        && raw.as_bytes()[0].is_ascii_alphabetic()
}
