#![allow(clippy::result_large_err)]
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::ato_lock::{AtoLock, UnresolvedValue};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::lock_runtime::LockCompilerOverlay;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Workspace-local mutable state is resolved once before command pipelines consume
// lock-derived plan/config data.
//
// Invariants:
// - binding precedence is CLI > workspace-local > embedded lock state
// - policy precedence is workspace-local > embedded lock policy > default local allow
// - default local allow means "no extra local restriction"; it does not grant
//   capabilities beyond the lock-derived contract/plan/config
// - deny wins over allow
// - attestations/observations are consumed from the workspace-local attestation
//   store only; embedded lock attestations are not an authoritative read source
// - an empty attestation store means no approvals or observations have been
//   recorded yet
// - distribution sanitization strips only mutable/local sections
//   (binding/attestations); embedded policy remains part of the distributable lock
pub(crate) const WORKSPACE_SOURCE_INFERENCE_DIR: &str = ".ato/source-inference";
pub(crate) const WORKSPACE_BINDING_SEED_PATH: &str = ".ato/binding/seed.json";
pub(crate) const WORKSPACE_POLICY_BUNDLE_PATH: &str = ".ato/policy/bundle.json";
pub(crate) const WORKSPACE_ATTESTATION_STORE_PATH: &str = ".ato/attestations/store.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkspaceBindingSeed {
    pub(crate) schema_version: String,
    pub(crate) lock_path: PathBuf,
    pub(crate) provenance_cache_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lock_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) entries: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) unresolved: Vec<UnresolvedValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct WorkspacePolicyBundle {
    #[serde(default = "default_schema_version")]
    pub(crate) schema_version: String,
    #[serde(default)]
    pub(crate) network: WorkspaceNetworkPolicy,
    #[serde(default)]
    pub(crate) filesystem: WorkspaceFilesystemPolicy,
    #[serde(default)]
    pub(crate) secrets: WorkspaceSecretsPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct WorkspaceNetworkPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) allow_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) deny_hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct WorkspaceFilesystemPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) allow_read_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) deny_read_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) allow_read_write: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) deny_read_write: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct WorkspaceSecretsPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) allow_secret_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) deny_secret_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct WorkspaceAttestationStore {
    pub(crate) schema_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lock_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lock_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) approvals: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) observations: Vec<Value>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EffectiveLockState {
    pub(crate) state_source_overrides: HashMap<String, String>,
    pub(crate) compiler_overlay: LockCompilerOverlay,
    pub(crate) policy: WorkspacePolicyBundle,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkspaceStatePaths {
    pub(crate) binding_seed_path: PathBuf,
    pub(crate) policy_bundle_path: PathBuf,
    pub(crate) attestation_store_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct BindingPayload {
    state_overrides: HashMap<String, String>,
    overlay: LockCompilerOverlay,
}

fn default_schema_version() -> String {
    "1".to_string()
}

pub(crate) fn workspace_state_paths(project_root: &Path) -> WorkspaceStatePaths {
    WorkspaceStatePaths {
        binding_seed_path: project_root.join(WORKSPACE_BINDING_SEED_PATH),
        policy_bundle_path: project_root.join(WORKSPACE_POLICY_BUNDLE_PATH),
        attestation_store_path: project_root.join(WORKSPACE_ATTESTATION_STORE_PATH),
    }
}

pub(crate) fn write_default_policy_bundle(path: &Path) -> Result<()> {
    let bundle = WorkspacePolicyBundle {
        schema_version: "1".to_string(),
        ..WorkspacePolicyBundle::default()
    };
    write_json_file(path, &bundle, "workspace policy bundle")
}

pub(crate) fn write_default_attestation_store(
    path: &Path,
    lock_path: &Path,
    lock: &AtoLock,
) -> Result<()> {
    let store = WorkspaceAttestationStore {
        schema_version: "1".to_string(),
        lock_path: Some(lock_path.to_path_buf()),
        lock_id: lock
            .lock_id
            .as_ref()
            .map(|value| value.as_str().to_string()),
        approvals: Vec::new(),
        observations: Vec::new(),
    };
    write_json_file(path, &store, "workspace attestation store")
}

pub(crate) fn resolve_effective_lock_state(
    project_root: &Path,
    lock: &AtoLock,
    cli_state_bindings: &[String],
) -> Result<EffectiveLockState> {
    // Binding and policy are resolved exactly once at this boundary so run,
    // install, and lock-derived execution reuse the same precedence rules.
    // Attestations/observations remain workspace-local evidence only; they are
    // affinity-checked here but are not loaded from embedded lock state.
    let paths = workspace_state_paths(project_root);
    let embedded_binding = parse_binding_entries(&lock.binding.entries)?;
    let workspace_binding = load_workspace_binding_seed(&paths.binding_seed_path)?
        .map(|seed| validate_workspace_binding_seed_affinity(&paths.binding_seed_path, &seed, lock))
        .transpose()?
        .map(|seed| parse_binding_entries(&seed.entries))
        .transpose()?
        .unwrap_or_default();
    let cli_state_overrides = parse_cli_state_overrides(cli_state_bindings)?;

    let mut state_source_overrides = embedded_binding.state_overrides;
    state_source_overrides.extend(workspace_binding.state_overrides);
    state_source_overrides.extend(cli_state_overrides);

    let compiler_overlay = merge_overlays(&embedded_binding.overlay, &workspace_binding.overlay);

    let workspace_policy = load_workspace_policy_bundle(&paths.policy_bundle_path)?;
    let policy = if let Some(bundle) = workspace_policy {
        bundle
    } else if let Some(bundle) = parse_embedded_policy_bundle(&lock.policy.entries)? {
        bundle
    } else {
        WorkspacePolicyBundle {
            schema_version: "1".to_string(),
            ..WorkspacePolicyBundle::default()
        }
    };
    load_workspace_attestation_store(&paths.attestation_store_path)?
        .map(|store| {
            validate_workspace_attestation_store_affinity(
                &paths.attestation_store_path,
                &store,
                lock,
            )
        })
        .transpose()?;

    Ok(EffectiveLockState {
        state_source_overrides,
        compiler_overlay,
        policy,
    })
}

pub(crate) fn sanitize_lock_for_distribution(lock: &AtoLock) -> AtoLock {
    // Distribution keeps the canonical lock plus embedded policy, but strips
    // mutable/local sections that must not be published by default.
    let mut sanitized = lock.clone();
    sanitized.binding.entries.clear();
    sanitized.binding.unresolved.clear();
    sanitized.attestations.entries.clear();
    sanitized.attestations.unresolved.clear();
    sanitized
}

pub(crate) fn validate_execution_plan_against_policy(
    plan: &ExecutionPlan,
    policy: &WorkspacePolicyBundle,
) -> Result<(), AtoExecutionError> {
    validate_string_scope(
        "network.allow_hosts",
        &plan.runtime.policy.network.allow_hosts,
        &policy.network.allow_hosts,
        &policy.network.deny_hosts,
    )?;
    validate_string_scope(
        "filesystem.read_only",
        &plan.runtime.policy.filesystem.read_only,
        &policy.filesystem.allow_read_only,
        &policy.filesystem.deny_read_only,
    )?;
    validate_string_scope(
        "filesystem.read_write",
        &plan.runtime.policy.filesystem.read_write,
        &policy.filesystem.allow_read_write,
        &policy.filesystem.deny_read_write,
    )?;
    validate_string_scope(
        "secrets.allow_secret_ids",
        &plan.runtime.policy.secrets.allow_secret_ids,
        &policy.secrets.allow_secret_ids,
        &policy.secrets.deny_secret_ids,
    )?;
    Ok(())
}

pub(crate) fn validate_config_against_policy(
    config: &capsule_core::runtime_config::ConfigJson,
    policy: &WorkspacePolicyBundle,
) -> Result<(), AtoExecutionError> {
    let mut allow_hosts = Vec::new();
    if let Some(egress) = config.sandbox.network.egress.as_ref() {
        if let Some(rules) = egress.rules.as_ref() {
            allow_hosts.extend(rules.iter().map(|rule| rule.value.clone()));
        }
    }
    validate_string_scope(
        "network.allow_hosts",
        &allow_hosts,
        &policy.network.allow_hosts,
        &policy.network.deny_hosts,
    )?;
    if let Some(filesystem) = config.sandbox.filesystem.as_ref() {
        validate_string_scope(
            "filesystem.read_only",
            filesystem.read_only.as_deref().unwrap_or(&[]),
            &policy.filesystem.allow_read_only,
            &policy.filesystem.deny_read_only,
        )?;
        validate_string_scope(
            "filesystem.read_write",
            filesystem.read_write.as_deref().unwrap_or(&[]),
            &policy.filesystem.allow_read_write,
            &policy.filesystem.deny_read_write,
        )?;
    }
    Ok(())
}

fn validate_string_scope(
    field: &str,
    values: &[String],
    allow: &[String],
    deny: &[String],
) -> Result<(), AtoExecutionError> {
    if let Some(value) = values
        .iter()
        .find(|value| deny.iter().any(|deny| deny == *value))
    {
        return Err(AtoExecutionError::security_policy_violation(
            format!(
                "policy denied {field} entry '{value}'; contract/runtime request must fail closed"
            ),
            Some(field),
            Some(value.as_str()),
        ));
    }

    if allow.is_empty() {
        return Ok(());
    }

    if let Some(value) = values
        .iter()
        .find(|value| !allow.iter().any(|allow| allow == *value))
    {
        return Err(AtoExecutionError::security_policy_violation(
            format!(
                "policy allowlist for {field} does not permit '{value}'; contract/runtime request must fail closed"
            ),
            Some(field),
            Some(value.as_str()),
        ));
    }

    Ok(())
}

fn load_workspace_binding_seed(path: &Path) -> Result<Option<WorkspaceBindingSeed>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let seed = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(seed))
}

fn load_workspace_policy_bundle(path: &Path) -> Result<Option<WorkspacePolicyBundle>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let bundle = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(bundle))
}

fn load_workspace_attestation_store(path: &Path) -> Result<Option<WorkspaceAttestationStore>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let store = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(store))
}

fn validate_workspace_binding_seed_affinity(
    path: &Path,
    seed: &WorkspaceBindingSeed,
    lock: &AtoLock,
) -> Result<WorkspaceBindingSeed> {
    if seed.entries.is_empty() && seed.unresolved.is_empty() {
        return Ok(seed.clone());
    }

    let current_lock_id = lock
        .lock_id
        .as_ref()
        .map(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "workspace binding seed at {} requires lock.lock_id to be present before local bindings can be applied fail-closed",
                path.display()
            )
        })?;
    let seed_lock_id = seed
        .lock_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "workspace binding seed at {} is missing lock_id; regenerate the workspace state before applying local bindings",
                path.display()
            )
        })?;

    if seed_lock_id != current_lock_id {
        anyhow::bail!(
            "workspace binding seed at {} targets lock_id '{}' but the authoritative lock resolved to '{}'; refusing to apply stale workspace bindings",
            path.display(),
            seed_lock_id,
            current_lock_id,
        );
    }

    Ok(seed.clone())
}

fn validate_workspace_attestation_store_affinity(
    path: &Path,
    store: &WorkspaceAttestationStore,
    lock: &AtoLock,
) -> Result<WorkspaceAttestationStore> {
    if store.approvals.is_empty() && store.observations.is_empty() {
        return Ok(store.clone());
    }

    let current_lock_id = lock
        .lock_id
        .as_ref()
        .map(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "workspace attestation store at {} requires lock.lock_id to be present before host-local attestations can be consumed",
                path.display()
            )
        })?;
    let store_lock_id = store
        .lock_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "workspace attestation store at {} is missing lock_id; regenerate the workspace state before consuming attestations",
                path.display()
            )
        })?;

    if store_lock_id != current_lock_id {
        anyhow::bail!(
            "workspace attestation store at {} targets lock_id '{}' but the authoritative lock resolved to '{}'; refusing to consume stale attestations",
            path.display(),
            store_lock_id,
            current_lock_id,
        );
    }

    Ok(store.clone())
}

fn parse_cli_state_overrides(raw_bindings: &[String]) -> Result<HashMap<String, String>> {
    let mut overrides = HashMap::new();
    for raw in raw_bindings {
        let (state_name, locator) = raw.split_once('=').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --state binding '{}'; expected data=/absolute/path or data=state-...",
                raw
            )
        })?;
        let state_name = state_name.trim();
        let locator = locator.trim();
        if state_name.is_empty() || locator.is_empty() {
            anyhow::bail!(
                "invalid --state binding '{}'; expected data=/absolute/path or data=state-...",
                raw
            );
        }
        overrides.insert(state_name.to_string(), locator.to_string());
    }
    Ok(overrides)
}

fn parse_binding_entries(entries: &BTreeMap<String, Value>) -> Result<BindingPayload> {
    let mut payload = BindingPayload::default();

    if let Some(state_overrides) = entries.get("state_overrides") {
        let object = state_overrides.as_object().ok_or_else(|| {
            anyhow::anyhow!("binding.state_overrides must be an object of state -> locator")
        })?;
        for (state_name, locator) in object {
            let locator = locator.as_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "binding.state_overrides.{} must be a string locator",
                    state_name
                )
            })?;
            payload
                .state_overrides
                .insert(state_name.clone(), locator.to_string());
        }
    }

    if let Some(overlay) = entries.get("overlay") {
        payload.overlay = parse_overlay_value(overlay)?;
    }

    Ok(payload)
}

fn parse_overlay_value(value: &Value) -> Result<LockCompilerOverlay> {
    let overlay = value.as_object().ok_or_else(|| {
        anyhow::anyhow!("binding.overlay must be an object when embedded in lock state")
    })?;
    Ok(LockCompilerOverlay {
        network_allow_hosts: overlay
            .get("network_allow_hosts")
            .map(parse_string_array)
            .transpose()?,
        filesystem_read_only: overlay
            .get("filesystem_read_only")
            .map(parse_string_array)
            .transpose()?,
        filesystem_read_write: overlay
            .get("filesystem_read_write")
            .map(parse_string_array)
            .transpose()?,
        secret_ids: overlay
            .get("secret_ids")
            .map(parse_string_array)
            .transpose()?,
    })
}

fn parse_embedded_policy_bundle(
    entries: &BTreeMap<String, Value>,
) -> Result<Option<WorkspacePolicyBundle>> {
    if entries.is_empty() {
        return Ok(None);
    }

    let payload = Value::Object(
        entries
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let bundle: WorkspacePolicyBundle =
        serde_json::from_value(payload).context("Failed to parse embedded lock policy bundle")?;
    Ok(Some(bundle))
}

fn parse_string_array(value: &Value) -> Result<Vec<String>> {
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("policy/binding arrays must be JSON arrays of strings"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("policy/binding arrays must contain strings"))
        })
        .collect()
}

fn merge_overlays(
    base: &LockCompilerOverlay,
    preferred: &LockCompilerOverlay,
) -> LockCompilerOverlay {
    LockCompilerOverlay {
        network_allow_hosts: preferred
            .network_allow_hosts
            .clone()
            .or_else(|| base.network_allow_hosts.clone()),
        filesystem_read_only: preferred
            .filesystem_read_only
            .clone()
            .or_else(|| base.filesystem_read_only.clone()),
        filesystem_read_write: preferred
            .filesystem_read_write
            .clone()
            .or_else(|| base.filesystem_read_write.clone()),
        secret_ids: preferred
            .secret_ids
            .clone()
            .or_else(|| base.secret_ids.clone()),
    }
}

fn write_json_file<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(value)
        .with_context(|| format!("Failed to serialize {label}"))?;
    fs::write(path, raw).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn sample_lock() -> AtoLock {
        let mut lock = AtoLock {
            lock_id: Some(capsule_core::ato_lock::LockId::new(
                "blake3:1111111111111111111111111111111111111111111111111111111111111111",
            )),
            ..AtoLock::default()
        };
        lock.binding.entries.insert(
            "state_overrides".to_string(),
            json!({"data": "state-embedded"}),
        );
        lock.binding.entries.insert(
            "overlay".to_string(),
            json!({"network_allow_hosts": ["embedded.example.com"]}),
        );
        lock.policy.entries.insert(
            "network".to_string(),
            json!({"allow_hosts": ["embedded.example.com"], "deny_hosts": ["blocked.example.com"]}),
        );
        lock
    }

    #[test]
    fn binding_precedence_prefers_cli_then_workspace_then_embedded() {
        let dir = tempdir().expect("tempdir");
        let paths = workspace_state_paths(dir.path());
        let seed = WorkspaceBindingSeed {
            schema_version: "1".to_string(),
            lock_path: dir.path().join("ato.lock.json"),
            provenance_cache_path: dir
                .path()
                .join(".ato/source-inference/provenance-cache.json"),
            lock_id: Some(
                "blake3:1111111111111111111111111111111111111111111111111111111111111111"
                    .to_string(),
            ),
            entries: BTreeMap::from([
                (
                    "state_overrides".to_string(),
                    json!({"data": "state-workspace", "cache": "/workspace/cache"}),
                ),
                (
                    "overlay".to_string(),
                    json!({"network_allow_hosts": ["workspace.example.com"]}),
                ),
            ]),
            unresolved: Vec::new(),
        };
        write_json_file(&paths.binding_seed_path, &seed, "binding seed").expect("write seed");

        let effective = resolve_effective_lock_state(
            dir.path(),
            &sample_lock(),
            &["data=/cli/data".to_string()],
        )
        .expect("effective state");

        assert_eq!(
            effective
                .state_source_overrides
                .get("data")
                .map(String::as_str),
            Some("/cli/data")
        );
        assert_eq!(
            effective
                .state_source_overrides
                .get("cache")
                .map(String::as_str),
            Some("/workspace/cache")
        );
        assert_eq!(
            effective
                .compiler_overlay
                .network_allow_hosts
                .as_ref()
                .expect("overlay"),
            &vec!["workspace.example.com".to_string()]
        );
    }

    #[test]
    fn workspace_policy_bundle_overrides_embedded_policy_source() {
        let dir = tempdir().expect("tempdir");
        let paths = workspace_state_paths(dir.path());
        let bundle = WorkspacePolicyBundle {
            schema_version: "1".to_string(),
            network: WorkspaceNetworkPolicy {
                deny_hosts: vec!["workspace.example.com".to_string()],
                ..WorkspaceNetworkPolicy::default()
            },
            ..WorkspacePolicyBundle::default()
        };
        write_json_file(
            &paths.policy_bundle_path,
            &bundle,
            "workspace policy bundle",
        )
        .expect("write policy");

        let effective =
            resolve_effective_lock_state(dir.path(), &sample_lock(), &[]).expect("effective");
        assert_eq!(
            effective.policy.network.deny_hosts,
            vec!["workspace.example.com"]
        );
        assert!(effective.policy.network.allow_hosts.is_empty());
    }

    #[test]
    fn empty_workspace_policy_bundle_is_explicit_default_allow_override() {
        let dir = tempdir().expect("tempdir");
        let paths = workspace_state_paths(dir.path());
        write_default_policy_bundle(&paths.policy_bundle_path).expect("write policy");

        let effective =
            resolve_effective_lock_state(dir.path(), &sample_lock(), &[]).expect("effective");

        assert!(effective.policy.network.allow_hosts.is_empty());
        assert!(effective.policy.network.deny_hosts.is_empty());
    }

    #[test]
    fn workspace_binding_seed_fails_closed_on_lock_id_mismatch() {
        let dir = tempdir().expect("tempdir");
        let paths = workspace_state_paths(dir.path());
        let seed = WorkspaceBindingSeed {
            schema_version: "1".to_string(),
            lock_path: dir.path().join("ato.lock.json"),
            provenance_cache_path: dir
                .path()
                .join(".ato/source-inference/provenance-cache.json"),
            lock_id: Some(
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            entries: BTreeMap::from([(
                "state_overrides".to_string(),
                json!({"data": "state-workspace"}),
            )]),
            unresolved: Vec::new(),
        };
        write_json_file(&paths.binding_seed_path, &seed, "binding seed").expect("write seed");

        let error =
            resolve_effective_lock_state(dir.path(), &sample_lock(), &[]).expect_err("mismatch");
        assert!(error
            .to_string()
            .contains("refusing to apply stale workspace bindings"));
    }

    #[test]
    fn workspace_attestation_store_fails_closed_on_lock_id_mismatch() {
        let dir = tempdir().expect("tempdir");
        let paths = workspace_state_paths(dir.path());
        let store = WorkspaceAttestationStore {
            schema_version: "1".to_string(),
            lock_path: Some(dir.path().join("ato.lock.json")),
            lock_id: Some(
                "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_string(),
            ),
            approvals: vec![json!({"kind": "approval"})],
            observations: Vec::new(),
        };
        write_json_file(&paths.attestation_store_path, &store, "attestation store")
            .expect("write store");

        let error =
            resolve_effective_lock_state(dir.path(), &sample_lock(), &[]).expect_err("mismatch");
        assert!(error
            .to_string()
            .contains("refusing to consume stale attestations"));
    }

    #[test]
    fn policy_deny_wins_over_allow() {
        let policy = WorkspacePolicyBundle {
            schema_version: "1".to_string(),
            network: WorkspaceNetworkPolicy {
                allow_hosts: vec!["example.com".to_string()],
                deny_hosts: vec!["example.com".to_string()],
            },
            ..WorkspacePolicyBundle::default()
        };

        let plan = capsule_core::execution_plan::model::ExecutionPlan {
            schema_version: "1".to_string(),
            capsule: capsule_core::execution_plan::model::CapsuleRef {
                scoped_id: "local/test".to_string(),
                version: "0.0.0".to_string(),
            },
            target: capsule_core::execution_plan::model::TargetRef {
                label: "default".to_string(),
                runtime: capsule_core::execution_plan::model::ExecutionRuntime::Source,
                driver: capsule_core::execution_plan::model::ExecutionDriver::Native,
                language: None,
            },
            provisioning: capsule_core::execution_plan::model::Provisioning {
                network: capsule_core::execution_plan::model::ProvisioningNetwork {
                    allow_registry_hosts: Vec::new(),
                },
                lock_required: true,
                integrity_required: true,
                allowed_registries: Vec::new(),
            },
            runtime: capsule_core::execution_plan::model::Runtime {
                policy: capsule_core::execution_plan::model::RuntimePolicy {
                    network: capsule_core::execution_plan::model::RuntimeNetworkPolicy {
                        allow_hosts: vec!["example.com".to_string()],
                    },
                    filesystem: capsule_core::execution_plan::model::RuntimeFilesystemPolicy {
                        read_only: Vec::new(),
                        read_write: Vec::new(),
                    },
                    secrets: capsule_core::execution_plan::model::RuntimeSecretsPolicy {
                        allow_secret_ids: Vec::new(),
                        delivery: capsule_core::execution_plan::model::SecretDelivery::Fd,
                    },
                    args: Vec::new(),
                },
                fail_closed: true,
                non_interactive_behavior:
                    capsule_core::execution_plan::model::NonInteractiveBehavior::DenyIfUnconsented,
            },
            consent: capsule_core::execution_plan::model::Consent {
                key: capsule_core::execution_plan::model::ConsentKey {
                    scoped_id: "local/test".to_string(),
                    version: "0.0.0".to_string(),
                    target_label: "default".to_string(),
                },
                policy_segment_hash: "policy".to_string(),
                provisioning_policy_hash: "provisioning".to_string(),
                mount_set_algo_id: "algo".to_string(),
                mount_set_algo_version: 1,
            },
            reproducibility: capsule_core::execution_plan::model::Reproducibility {
                platform: capsule_core::execution_plan::model::Platform {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "system".to_string(),
                },
            },
        };

        let error = validate_execution_plan_against_policy(&plan, &policy).expect_err("deny");
        assert!(error
            .to_string()
            .contains("policy denied network.allow_hosts entry"));
    }

    #[test]
    fn sanitize_lock_for_distribution_strips_binding_and_attestations() {
        let mut lock = sample_lock();
        lock.binding.entries.insert(
            "state.data".to_string(),
            Value::String("/tmp/data".to_string()),
        );
        lock.attestations
            .entries
            .insert("approval".to_string(), Value::String("granted".to_string()));

        let sanitized = sanitize_lock_for_distribution(&lock);

        assert!(sanitized.binding.entries.is_empty());
        assert!(sanitized.binding.unresolved.is_empty());
        assert!(sanitized.attestations.entries.is_empty());
        assert!(sanitized.attestations.unresolved.is_empty());
        assert_eq!(sanitized.policy, lock.policy);
        assert_eq!(sanitized.contract, lock.contract);
        assert_eq!(sanitized.resolution, lock.resolution);
    }
}
