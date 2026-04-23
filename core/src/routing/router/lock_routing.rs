//! Private lock-routing helpers.
//!
//! Translates an `AtoLock` into a synthesized `toml::Value` manifest used by
//! the public `route_lock_*` family of functions.

use crate::ato_lock::AtoLock;
use crate::lock_runtime::ResolvedLockRuntimeModel;

/// Build a synthesized `capsule.toml`-shaped `toml::Value` from an `AtoLock`
/// plus its resolved runtime model. The resulting value is fed into
/// `CompatManifestBridge::from_manifest_value` to produce a full bridge.
pub(super) fn synthesize_manifest_from_lock(
    lock: &AtoLock,
    runtime_model: &ResolvedLockRuntimeModel,
) -> toml::Value {
    let mut manifest = toml::map::Map::new();
    if let Some(name) = runtime_model.metadata.name.as_ref() {
        manifest.insert("name".to_string(), toml::Value::String(name.clone()));
    }
    if let Some(version) = runtime_model.metadata.version.as_ref() {
        manifest.insert("version".to_string(), toml::Value::String(version.clone()));
    }
    manifest.insert(
        "schema_version".to_string(),
        toml::Value::String("0.3".to_string()),
    );
    manifest.insert(
        "type".to_string(),
        toml::Value::String(
            runtime_model
                .metadata
                .capsule_type
                .clone()
                .unwrap_or_else(|| "app".to_string()),
        ),
    );
    manifest.insert(
        "default_target".to_string(),
        toml::Value::String(runtime_model.selected.target_label.clone()),
    );

    if let Some(network) = runtime_model.network.as_ref() {
        if let Ok(value) = toml::Value::try_from(network.clone()) {
            manifest.insert("network".to_string(), value);
        }
    }

    let mut targets = toml::map::Map::new();
    for service in &runtime_model.services {
        let mut target = toml::map::Map::new();
        let runtime = &service.runtime;
        target.insert(
            "runtime".to_string(),
            toml::Value::String(runtime.runtime.clone()),
        );
        if let Some(driver) = runtime.driver.as_ref() {
            target.insert("driver".to_string(), toml::Value::String(driver.clone()));
        }
        if let Some(image) = runtime.image.as_ref() {
            target.insert("image".to_string(), toml::Value::String(image.clone()));
        }
        // schema_version "0.3" rejects legacy `entrypoint`/`cmd` fields.
        // Execution entrypoint is read from the lock runtime model directly, not
        // from this synthesized manifest, so these fields can be omitted here.
        if let Some(run_command) = runtime.run_command.as_ref() {
            if !run_command.trim().is_empty() {
                target.insert(
                    "run_command".to_string(),
                    toml::Value::String(run_command.clone()),
                );
            }
        }
        if !runtime.env.is_empty() {
            let env = runtime
                .env
                .iter()
                .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
                .collect();
            target.insert("env".to_string(), toml::Value::Table(env));
        }
        if let Some(working_dir) = runtime.working_dir.as_ref() {
            target.insert(
                "working_dir".to_string(),
                toml::Value::String(working_dir.clone()),
            );
        }
        if let Some(source_layout) = runtime.source_layout.as_ref() {
            target.insert(
                "source_layout".to_string(),
                toml::Value::String(source_layout.clone()),
            );
        }
        if let Some(port) = runtime.port {
            target.insert("port".to_string(), toml::Value::Integer(i64::from(port)));
        }
        if !runtime.required_env.is_empty() {
            target.insert(
                "required_env".to_string(),
                toml::Value::Array(
                    runtime
                        .required_env
                        .iter()
                        .cloned()
                        .map(toml::Value::String)
                        .collect(),
                ),
            );
        }
        if let Some(runtime_version) =
            resolved_target_string_from_lock(lock, &service.target_label, "runtime_version")
        {
            target.insert(
                "runtime_version".to_string(),
                toml::Value::String(runtime_version),
            );
        }
        if let Some(runtime_tools) =
            resolved_target_table_from_lock(lock, &service.target_label, &["runtime_tools"])
        {
            target.insert(
                "runtime_tools".to_string(),
                toml::Value::Table(runtime_tools),
            );
        }
        if let Some(readiness_probe) = service.readiness_probe.as_ref() {
            if let Ok(value) = toml::Value::try_from(readiness_probe.clone()) {
                target.insert("readiness_probe".to_string(), value);
            }
        }
        targets.insert(service.target_label.clone(), toml::Value::Table(target));
    }
    manifest.insert("targets".to_string(), toml::Value::Table(targets));

    if runtime_model.services.len() > 1 {
        let mut services = toml::map::Map::new();
        for service in &runtime_model.services {
            let mut service_table = toml::map::Map::new();
            service_table.insert(
                "target".to_string(),
                toml::Value::String(service.target_label.clone()),
            );
            if !service.depends_on.is_empty() {
                service_table.insert(
                    "depends_on".to_string(),
                    toml::Value::Array(
                        service
                            .depends_on
                            .iter()
                            .cloned()
                            .map(toml::Value::String)
                            .collect(),
                    ),
                );
            }
            if let Some(readiness_probe) = service.readiness_probe.as_ref() {
                if let Ok(value) = toml::Value::try_from(readiness_probe.clone()) {
                    service_table.insert("readiness_probe".to_string(), value);
                }
            }
            services.insert(service.name.clone(), toml::Value::Table(service_table));
        }
        manifest.insert("services".to_string(), toml::Value::Table(services));
    }

    toml::Value::Table(manifest)
}

fn resolved_target_string_from_lock(
    lock: &AtoLock,
    target_label: &str,
    key: &str,
) -> Option<String> {
    let target_value = lock
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(|value| value.as_array())
        .and_then(|targets| {
            targets.iter().find(|target| {
                target
                    .get("label")
                    .and_then(|value| value.as_str())
                    .map(|label| label == target_label)
                    .unwrap_or(false)
            })
        })
        .and_then(|target| target.get(key))
        .and_then(|value| value.as_str())
        .map(str::to_string);

    if target_value.is_some() || key != "runtime_version" {
        return target_value;
    }

    lock.resolution
        .entries
        .get("runtime")
        .and_then(|runtime| runtime.get("version"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn resolved_target_table_from_lock(
    lock: &AtoLock,
    target_label: &str,
    keys: &[&str],
) -> Option<toml::value::Table> {
    let mut current = lock
        .resolution
        .entries
        .get("resolved_targets")
        .and_then(|value| value.as_array())
        .and_then(|targets| {
            targets.iter().find(|target| {
                target
                    .get("label")
                    .and_then(|value| value.as_str())
                    .map(|label| label == target_label)
                    .unwrap_or(false)
            })
        })?;
    for key in keys {
        current = current.get(*key)?;
    }
    let object = current.as_object()?;
    Some(
        object
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), toml::Value::String(value.to_string())))
            })
            .collect(),
    )
}
