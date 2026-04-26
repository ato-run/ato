mod archive;
mod cache;
mod lockfile;
mod materialize;
mod resolve;

use std::collections::HashMap;

use anyhow::{Context, Result};
use capsule_core::lockfile::LockedInjectedData;
use capsule_core::router::ManifestData;

use crate::executors::launch_context::InjectedMount;

use self::lockfile::persist_lockfile_injected_data;
use self::materialize::materialize_injection;
use self::resolve::parse_cli_bindings;

const ENV_INJECTED_DATA_CACHE_DIR: &str = "ATO_INJECTED_DATA_CACHE_DIR";

#[derive(Debug, Clone, Default)]
pub struct ResolvedDataInjection {
    pub env: HashMap<String, String>,
    pub mounts: Vec<InjectedMount>,
}

pub async fn resolve_and_record(
    plan: &ManifestData,
    raw_bindings: &[String],
) -> Result<ResolvedDataInjection> {
    let contract = plan.selected_target_external_injection();
    if contract.is_empty() {
        if raw_bindings.is_empty() {
            return Ok(ResolvedDataInjection::default());
        }
        anyhow::bail!(
            "target '{}' does not declare [external_injection], but --inject was provided",
            plan.selected_target_label()
        );
    }

    let bindings = parse_cli_bindings(raw_bindings)?;
    for key in bindings.keys() {
        if !contract.contains_key(key) {
            anyhow::bail!(
                "target '{}' does not declare external_injection.{}",
                plan.selected_target_label(),
                key
            );
        }
    }

    let mut env = HashMap::new();
    let mut mounts = Vec::new();
    let mut locked: HashMap<String, LockedInjectedData> = HashMap::new();
    for (key, spec) in &contract {
        let source = bindings.get(key).cloned().or_else(|| spec.default.clone());
        let Some(source) = source else {
            if spec.required {
                anyhow::bail!(
                    "target '{}' requires injection for {}",
                    plan.selected_target_label(),
                    key
                );
            }
            continue;
        };

        let materialized = materialize_injection(plan, key, spec, &source)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve external injection {} for target '{}'",
                    key,
                    plan.selected_target_label()
                )
            })?;
        env.insert(key.clone(), materialized.env_value);
        if let Some(mount) = materialized.mount {
            mounts.push(mount);
        }
        locked.insert(key.clone(), materialized.locked);
    }

    persist_lockfile_injected_data(&plan.manifest_path, &locked)?;
    Ok(ResolvedDataInjection { env, mounts })
}

#[cfg(test)]
mod tests;
