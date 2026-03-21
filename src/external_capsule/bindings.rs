use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use capsule_core::lockfile::LockedCapsuleDependency;
use capsule_core::router::ManifestData;

pub(super) fn parse_cli_bindings(raw_bindings: &[String]) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for raw_binding in raw_bindings {
        let Some((key, value)) = raw_binding.split_once('=') else {
            anyhow::bail!("--inject must use KEY=VALUE syntax, got '{}'", raw_binding);
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            anyhow::bail!(
                "--inject must use non-empty KEY=VALUE syntax, got '{}'",
                raw_binding
            );
        }
        bindings.insert(key.to_string(), value.to_string());
    }
    Ok(bindings)
}

pub(super) fn merged_dependency_bindings(
    plan: &ManifestData,
    locked: &LockedCapsuleDependency,
    cli_bindings: &BTreeMap<String, String>,
) -> Vec<String> {
    let contract = plan.selected_target_external_injection();
    let mut merged = locked.injection_bindings.clone();
    for key in contract.keys() {
        if let Some(value) = cli_bindings.get(key) {
            merged.insert(key.clone(), value.clone());
        }
    }

    let mut values: Vec<String> = merged
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect();
    values.sort();
    values
}

pub(super) fn connection_env_vars(alias: &str, port: u16) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let key = sanitize_alias(alias);
    env.insert(format!("ATO_PKG_{}_HOST", key), "127.0.0.1".to_string());
    env.insert(format!("ATO_PKG_{}_PORT", key), port.to_string());
    env.insert(
        format!("ATO_PKG_{}_URL", key),
        format!("http://127.0.0.1:{}", port),
    );
    env.insert(format!("ATO_SERVICE_{}_HOST", key), "127.0.0.1".to_string());
    env.insert(format!("ATO_SERVICE_{}_PORT", key), port.to_string());
    env
}

pub(super) fn sanitize_alias(alias: &str) -> String {
    alias
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}
