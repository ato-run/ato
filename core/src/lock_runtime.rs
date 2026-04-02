#![allow(clippy::result_large_err)]
use std::collections::HashMap;

use serde_json::Value;

use crate::ato_lock::AtoLock;
use crate::execution_plan::error::AtoExecutionError;
use crate::types::{NetworkConfig, ReadinessProbe, ResolvedTargetRuntime};

#[derive(Debug, Clone, Default)]
pub struct LockCompilerOverlay {
    pub network_allow_hosts: Option<Vec<String>>,
    pub filesystem_read_only: Option<Vec<String>>,
    pub filesystem_read_write: Option<Vec<String>>,
    pub secret_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockContractMetadata {
    pub name: Option<String>,
    pub version: Option<String>,
    pub capsule_type: Option<String>,
    pub default_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockServiceUnit {
    pub name: String,
    pub target_label: String,
    pub runtime: ResolvedTargetRuntime,
    pub depends_on: Vec<String>,
    pub readiness_probe: Option<ReadinessProbe>,
}

#[derive(Debug, Clone)]
pub struct ResolvedLockRuntimeModel {
    pub metadata: LockContractMetadata,
    pub network: Option<NetworkConfig>,
    pub selected: LockServiceUnit,
    pub services: Vec<LockServiceUnit>,
}

pub fn resolve_lock_runtime_model(
    lock: &AtoLock,
    explicit_target_label: Option<&str>,
) -> Result<ResolvedLockRuntimeModel, AtoExecutionError> {
    ensure_execution_ready(lock)?;

    let metadata = metadata_from_lock(lock);
    let target_label = selected_target_label(lock, explicit_target_label, &metadata)?;
    let targets = resolved_targets(lock)?;
    let services = build_services(lock, targets, target_label.as_str())?;
    let selected = services
        .iter()
        .find(|service| service.target_label == target_label)
        .cloned()
        .ok_or_else(|| {
            AtoExecutionError::execution_contract_invalid(
                format!(
                    "selected target '{}' does not correspond to an executable workload",
                    target_label
                ),
                Some("contract.workloads"),
                None,
            )
        })?;

    Ok(ResolvedLockRuntimeModel {
        metadata,
        network: network_from_lock(lock),
        selected,
        services,
    })
}

fn ensure_execution_ready(lock: &AtoLock) -> Result<(), AtoExecutionError> {
    if !lock.contract.entries.contains_key("process") {
        return Err(AtoExecutionError::ambiguous_entrypoint(
            "lock-derived execution requires contract.process to be resolved",
            explicit_candidates(lock),
        ));
    }

    if !lock.resolution.entries.contains_key("runtime") {
        return Err(AtoExecutionError::runtime_not_resolved(
            "lock-derived execution requires resolution.runtime",
            None,
        ));
    }

    if !lock.resolution.entries.contains_key("closure") {
        return Err(AtoExecutionError::lock_incomplete(
            "lock-derived execution requires resolution.closure",
            Some("resolution.closure"),
        ));
    }

    if resolved_targets(lock)?.is_empty() {
        return Err(AtoExecutionError::execution_contract_invalid(
            "lock-derived execution requires at least one resolved target",
            Some("resolution.resolved_targets"),
            None,
        ));
    }

    Ok(())
}

fn metadata_from_lock(lock: &AtoLock) -> LockContractMetadata {
    let metadata = lock.contract.entries.get("metadata");
    LockContractMetadata {
        name: metadata
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string),
        version: metadata
            .and_then(|value| value.get("version"))
            .and_then(Value::as_str)
            .map(str::to_string),
        capsule_type: metadata
            .and_then(|value| value.get("capsule_type"))
            .and_then(Value::as_str)
            .map(str::to_string),
        default_target: metadata
            .and_then(|value| value.get("default_target"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn selected_target_label(
    lock: &AtoLock,
    explicit_target_label: Option<&str>,
    metadata: &LockContractMetadata,
) -> Result<String, AtoExecutionError> {
    if let Some(label) = explicit_target_label
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(label.to_string());
    }

    if let Some(label) = lock
        .resolution
        .entries
        .get("target_selection")
        .and_then(|value| value.get("default_target"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(label.to_string());
    }

    if let Some(label) = lock
        .resolution
        .entries
        .get("runtime")
        .and_then(|value| value.get("selected_target"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(label.to_string());
    }

    if let Some(label) = metadata.default_target.as_ref() {
        return Ok(label.clone());
    }

    let targets = resolved_targets(lock)?;
    if targets.len() == 1 {
        if let Some(label) = targets[0].get("label").and_then(Value::as_str) {
            return Ok(label.to_string());
        }
    }

    Err(AtoExecutionError::execution_contract_invalid(
        "lock-derived execution requires an already selected target label",
        Some("resolution.target_selection.default_target"),
        None,
    ))
}

fn build_services(
    lock: &AtoLock,
    targets: &[Value],
    selected_target_label: &str,
) -> Result<Vec<LockServiceUnit>, AtoExecutionError> {
    let workloads = lock
        .contract
        .entries
        .get("workloads")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if workloads.is_empty() {
        return Ok(vec![synthesized_service(
            lock,
            targets,
            selected_target_label,
        )?]);
    }

    workloads
        .iter()
        .enumerate()
        .map(|(index, workload)| {
            service_from_workload(workload, targets, selected_target_label, index)
        })
        .collect()
}

fn synthesized_service(
    lock: &AtoLock,
    targets: &[Value],
    selected_target_label: &str,
) -> Result<LockServiceUnit, AtoExecutionError> {
    let process = lock.contract.entries.get("process").ok_or_else(|| {
        AtoExecutionError::execution_contract_invalid(
            "contract.process is required for synthesized lock service resolution",
            Some("contract.process"),
            None,
        )
    })?;

    Ok(LockServiceUnit {
        name: "main".to_string(),
        target_label: selected_target_label.to_string(),
        runtime: runtime_from_target(process, targets, selected_target_label)?,
        depends_on: Vec::new(),
        readiness_probe: None,
    })
}

fn service_from_workload(
    workload: &Value,
    targets: &[Value],
    selected_target_label: &str,
    index: usize,
) -> Result<LockServiceUnit, AtoExecutionError> {
    let name = workload
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("workload_{index}"));
    let target_label = workload
        .get("target")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| selected_target_label.to_string());
    let process = workload.get("process").unwrap_or(&Value::Null);

    Ok(LockServiceUnit {
        name,
        target_label: target_label.clone(),
        runtime: runtime_from_target(process, targets, &target_label)?,
        depends_on: workload
            .get("depends_on")
            .and_then(json_string_array)
            .unwrap_or_default(),
        readiness_probe: workload
            .get("readiness_probe")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    })
}

fn runtime_from_target(
    process: &Value,
    targets: &[Value],
    target_label: &str,
) -> Result<ResolvedTargetRuntime, AtoExecutionError> {
    let target = targets
        .iter()
        .find(|candidate| {
            candidate
                .get("label")
                .and_then(Value::as_str)
                .map(|label| label == target_label)
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            AtoExecutionError::execution_contract_invalid(
                format!(
                    "lock-derived target '{}' is missing from resolution.resolved_targets",
                    target_label
                ),
                Some("resolution.resolved_targets"),
                None,
            )
        })?;

    let runtime = target
        .get("runtime")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AtoExecutionError::runtime_not_resolved(
                format!("resolved target '{}' is missing runtime", target_label),
                Some(target_label),
            )
        })?;

    let entrypoint = process
        .get("entrypoint")
        .and_then(Value::as_str)
        .or_else(|| target.get("entrypoint").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();

    let run_command = process
        .get("run_command")
        .and_then(Value::as_str)
        .or_else(|| target.get("run_command").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let cmd = target
        .get("cmd")
        .and_then(json_string_array)
        .or_else(|| process.get("cmd").and_then(json_string_array))
        .or_else(|| process.get("args").and_then(json_string_array))
        .unwrap_or_default();

    let mut env = json_string_map(target.get("env"));
    env.extend(json_string_map(process.get("env")));

    Ok(ResolvedTargetRuntime {
        target: target_label.to_string(),
        runtime: runtime.to_string(),
        driver: target
            .get("driver")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        image: target
            .get("image")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        entrypoint,
        run_command,
        cmd,
        env,
        working_dir: target
            .get("working_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        port: target
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok()),
        required_env: target
            .get("required_env")
            .and_then(json_string_array)
            .unwrap_or_default(),
        mounts: Vec::new(),
    })
}

fn resolved_targets(lock: &AtoLock) -> Result<&[Value], AtoExecutionError> {
    lock.resolution
        .entries
        .get("resolved_targets")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            AtoExecutionError::execution_contract_invalid(
                "resolution.resolved_targets must be present and non-empty for lock-derived execution",
                Some("resolution.resolved_targets"),
                None,
            )
        })
}

fn network_from_lock(lock: &AtoLock) -> Option<NetworkConfig> {
    lock.contract
        .entries
        .get("network")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn json_string_array(value: &Value) -> Option<Vec<String>> {
    let values = value.as_array()?;
    Some(
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    )
}

fn json_string_map(value: Option<&Value>) -> HashMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn explicit_candidates(lock: &AtoLock) -> Vec<String> {
    lock.contract
        .unresolved
        .iter()
        .flat_map(|value| value.candidates.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_lock() -> AtoLock {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "demo", "version": "0.1.0", "default_target": "web"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "cmd": ["deno", "run", "main.ts"]}),
        );
        lock.contract.entries.insert(
            "workloads".to_string(),
            json!([
                {
                    "name": "main",
                    "target": "web",
                    "process": {"entrypoint": "main.ts", "cmd": ["deno", "run", "main.ts"]}
                }
            ]),
        );
        lock.contract.entries.insert(
            "network".to_string(),
            json!({"egress_allow": ["example.com"]}),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "selected_target": "web"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([
                {
                    "label": "web",
                    "runtime": "source",
                    "driver": "deno",
                    "entrypoint": "main.ts",
                    "cmd": ["deno", "run", "main.ts"],
                    "port": 4173,
                    "required_env": ["PORT"]
                }
            ]),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            json!({"kind": "metadata_only", "status": "incomplete"}),
        );
        lock
    }

    #[test]
    fn resolves_selected_model_from_execution_ready_lock() {
        let model = resolve_lock_runtime_model(&sample_lock(), None).expect("resolved");

        assert_eq!(model.selected.name, "main");
        assert_eq!(model.selected.target_label, "web");
        assert_eq!(model.selected.runtime.driver.as_deref(), Some("deno"));
        assert_eq!(model.services.len(), 1);
    }

    #[test]
    fn rejects_lock_without_selected_target() {
        let mut lock = sample_lock();
        lock.resolution.entries.remove("runtime");

        let error = resolve_lock_runtime_model(&lock, None).expect_err("must fail");
        assert_eq!(error.code, "ATO_ERR_RUNTIME_NOT_RESOLVED");
    }

    #[test]
    fn synthesizes_service_when_workloads_are_missing() {
        let mut lock = sample_lock();
        lock.contract.entries.remove("workloads");

        let model = resolve_lock_runtime_model(&lock, Some("web")).expect("resolved");
        assert_eq!(model.selected.name, "main");
        assert_eq!(model.services.len(), 1);
    }
}
