//! Private manifest-routing helpers.
//!
//! Translates a `toml::Value` capsule manifest into a `ResolvedLockRuntimeModel`
//! used by the public `route_manifest_*` family of functions.

use std::collections::HashMap;

use crate::error::{CapsuleError, Result};
use crate::lock_runtime::{LockContractMetadata, LockServiceUnit, ResolvedLockRuntimeModel};
use crate::types::{Mount, NamedTarget, ReadinessProbe, ResolvedTargetRuntime};

/// Build a `ResolvedLockRuntimeModel` from a manifest with a `[targets]` table.
pub(super) fn synthesize_runtime_model_from_manifest(
    manifest: &toml::Value,
    selected_target: &str,
) -> Result<ResolvedLockRuntimeModel> {
    let metadata = LockContractMetadata {
        name: manifest
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        version: manifest
            .get("version")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        capsule_type: manifest
            .get("type")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        default_target: manifest
            .get("default_target")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    };
    let target = manifest
        .get("targets")
        .and_then(|targets| targets.get(selected_target))
        .cloned()
        .ok_or_else(|| CapsuleError::Config(format!("Missing required [targets.{}] table", selected_target)))?;
    let named_target: NamedTarget = target
        .try_into()
        .map_err(|_| CapsuleError::Config(format!("targets.{} is not a valid target table", selected_target)))?;
    let runtime = ResolvedTargetRuntime {
        target: selected_target.to_string(),
        runtime: named_target.runtime,
        driver: named_target.driver,
        runtime_version: named_target.runtime_version,
        image: named_target.image,
        entrypoint: named_target.entrypoint,
        run_command: named_target.run_command,
        cmd: named_target.cmd,
        env: named_target.env,
        working_dir: named_target.working_dir,
        source_layout: named_target.source_layout,
        port: named_target.port,
        required_env: named_target.required_env,
        mounts: Vec::<Mount>::new(),
    };
    let selected = LockServiceUnit {
        name: "main".to_string(),
        target_label: selected_target.to_string(),
        runtime: runtime.clone(),
        depends_on: Vec::new(),
        readiness_probe: named_target.readiness_probe,
    };
    Ok(ResolvedLockRuntimeModel {
        metadata,
        network: None,
        selected: selected.clone(),
        services: vec![selected],
    })
}

/// Determine the effective target label for a v0.3 manifest.
///
/// * Single-app manifests use `default_target` if present, otherwise `"app"`.
/// * Workspace manifests (`[packages]`) use `default_target` or the first
///   runnable package name.
pub(super) fn resolve_v03_target(manifest: &toml::Value, explicit: Option<&str>) -> Result<String> {
    if let Some(label) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(label.to_string());
    }

    if let Some(dt) = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(dt.to_string());
    }

    if let Some(packages) = manifest.get("packages").and_then(|v| v.as_table()) {
        for (name, pkg) in packages {
            if pkg.get("runtime").and_then(|v| v.as_str()).is_some() {
                return Ok(name.clone());
            }
        }
        if let Some((name, _)) = packages.iter().next() {
            return Ok(name.clone());
        }
    }

    Ok("app".to_string())
}

/// Split a v0.3 `runtime` selector (e.g. `"web/static"`, `"source/node"`)
/// into `(runtime, driver)` pair using the same logic as the v0.3 normalizer.
pub(super) fn split_v03_runtime(value: &str) -> (String, Option<String>) {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "web/static" => ("web".to_string(), Some("static".to_string())),
        "web/node" | "source/node" => ("source".to_string(), Some("node".to_string())),
        "web/deno" | "source/deno" => ("source".to_string(), Some("deno".to_string())),
        "web/python" | "source/python" => ("source".to_string(), Some("python".to_string())),
        "source/native" | "source/go" => ("source".to_string(), Some("native".to_string())),
        "source" | "web" | "oci" | "wasm" => (normalized, None),
        other => {
            if let Some((runtime, driver)) = other.split_once('/') {
                let runtime = runtime.trim();
                let driver = driver.trim();
                let runtime = if runtime == "web" && driver != "static" {
                    "source"
                } else {
                    runtime
                };
                (
                    runtime.to_string(),
                    (!driver.is_empty()).then(|| driver.to_string()),
                )
            } else {
                (other.to_string(), None)
            }
        }
    }
}

/// Infer a `language` hint from a v0.3 driver token.
pub(super) fn infer_language_from_driver(driver: Option<&str>) -> Option<String> {
    match driver.map(|v| v.trim().to_ascii_lowercase()) {
        Some(d) if matches!(d.as_str(), "node" | "python" | "deno" | "bun") => Some(d),
        _ => None,
    }
}

/// Build a `ResolvedLockRuntimeModel` directly from a v0.3 `toml::Value`.
///
/// For single-app manifests the runtime fields are read from the top level.
/// For workspace manifests they are read from `packages.<selected>`.
pub(super) fn synthesize_runtime_model_from_v03(
    manifest: &toml::Value,
    selected_target: &str,
) -> Result<ResolvedLockRuntimeModel> {
    let metadata = LockContractMetadata {
        name: manifest
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        version: manifest
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        capsule_type: manifest
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        default_target: Some(selected_target.to_string()),
    };

    let source = manifest
        .get("packages")
        .and_then(|pkgs| pkgs.get(selected_target))
        .unwrap_or(manifest);

    let runtime_selector = source
        .get("runtime")
        .and_then(|v| v.as_str())
        .unwrap_or("source");

    let (runtime, selector_driver) = split_v03_runtime(runtime_selector);
    let driver = selector_driver.or_else(|| {
        source
            .get("driver")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    });
    let language = infer_language_from_driver(driver.as_deref());

    let run_command = source
        .get("run")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let image = source
        .get("image")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let port = source
        .get("port")
        .and_then(|v| v.as_integer())
        .map(|v| v as u16);

    let is_web_static = runtime == "web" && driver.as_deref() == Some("static");
    let entrypoint = if is_web_static {
        run_command.clone().unwrap_or_else(|| ".".to_string())
    } else {
        String::new()
    };
    let effective_run_command = if is_web_static { None } else { run_command };

    let mut env = HashMap::new();
    if let Some(env_table) = source.get("env").and_then(|v| v.as_table()) {
        for (k, v) in env_table {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }

    let required_env: Vec<String> = source
        .get("required_env")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let readiness_probe = source.get("readiness_probe").and_then(|v| {
        v.as_str()
            .map(|s| ReadinessProbe {
                http_get: Some(s.to_string()),
                tcp_connect: None,
                port: "PORT".to_string(),
            })
            .or_else(|| {
                v.as_table().map(|table| ReadinessProbe {
                    http_get: table
                        .get("http_get")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    tcp_connect: table
                        .get("tcp_connect")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    port: table
                        .get("port")
                        .and_then(|v| v.as_str())
                        .unwrap_or("PORT")
                        .to_string(),
                })
            })
    });

    let resolved_runtime = ResolvedTargetRuntime {
        target: selected_target.to_string(),
        runtime: runtime.clone(),
        driver: driver.or(language),
        runtime_version: source
            .get("runtime_version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        image,
        entrypoint,
        run_command: effective_run_command,
        cmd: Vec::new(),
        env,
        working_dir: None,
        source_layout: None,
        port,
        required_env,
        mounts: Vec::new(),
    };

    let selected = LockServiceUnit {
        name: "main".to_string(),
        target_label: selected_target.to_string(),
        runtime: resolved_runtime,
        depends_on: Vec::new(),
        readiness_probe,
    };

    Ok(ResolvedLockRuntimeModel {
        metadata,
        network: None,
        selected: selected.clone(),
        services: vec![selected],
    })
}

/// Resolve the target label from a manifest with a `[targets]` table.
pub(super) fn resolve_target_label(
    manifest: &toml::Value,
    target_label: Option<&str>,
) -> Result<String> {
    let targets = manifest
        .get("targets")
        .and_then(|v| v.as_table())
        .ok_or_else(|| CapsuleError::Config("Missing required [targets] table".into()))?;

    let default_target = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CapsuleError::Config("Missing required field: default_target".into()))?;

    let selected = target_label
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_target);

    if !targets.contains_key(selected) {
        return Err(CapsuleError::Config(format!("Target '{}' not found under [targets]", selected)));
    }

    Ok(selected.to_string())
}
