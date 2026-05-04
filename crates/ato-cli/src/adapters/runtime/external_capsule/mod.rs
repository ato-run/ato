mod bindings;
pub(crate) mod cache;
mod probe;
mod spawn;

use std::collections::HashMap;

use anyhow::Result;
use capsule_core::lockfile::{manifest_external_capsule_dependencies, CapsuleLock};
use capsule_core::router::{ExecutionProfile, ManifestData};
use capsule_core::CapsuleReporter;

use crate::reporters::CliReporter;

use self::bindings::{connection_env_vars, merged_dependency_bindings, parse_cli_bindings};
use self::cache::ensure_runtime_tree_for_dependency;
use self::probe::wait_for_dependency_readiness;
use self::spawn::{spawn_external_capsule_child, ExternalCapsuleChild};

const EXTERNAL_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const EXTERNAL_READY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const EXTERNAL_CAPSULE_CACHE_DIR_ENV: &str = "ATO_EXTERNAL_CAPSULE_CACHE_DIR";

#[derive(Debug, Clone)]
pub struct ExternalCapsuleOptions {
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub assume_yes: bool,
}

pub struct ExternalCapsuleGuard {
    caller_env: HashMap<String, String>,
    caller_envs: Vec<(String, HashMap<String, String>)>,
    children: Vec<ExternalCapsuleChild>,
}

impl ExternalCapsuleGuard {
    pub fn caller_envs(&self) -> Vec<(String, HashMap<String, String>)> {
        self.caller_envs.clone()
    }

    pub fn shutdown_now(&mut self) {
        for child in &mut self.children {
            child.shutdown();
        }
    }
}

impl Drop for ExternalCapsuleGuard {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

pub async fn start_external_capsules(
    plan: &ManifestData,
    lockfile: &CapsuleLock,
    cli_inject_bindings: &[String],
    reporter: std::sync::Arc<CliReporter>,
    options: &ExternalCapsuleOptions,
) -> Result<ExternalCapsuleGuard> {
    let dependencies = manifest_external_capsule_dependencies(&plan.manifest)?
        .into_iter()
        .filter(|dependency| dependency.contract.is_none())
        .collect::<Vec<_>>();
    if dependencies.is_empty() {
        return Ok(ExternalCapsuleGuard {
            caller_env: HashMap::new(),
            caller_envs: Vec::new(),
            children: Vec::new(),
        });
    }

    let cli_bindings = parse_cli_bindings(cli_inject_bindings)?;
    let mut guard = ExternalCapsuleGuard {
        caller_env: HashMap::new(),
        caller_envs: Vec::new(),
        children: Vec::new(),
    };

    for dependency in dependencies {
        let locked = lockfile
            .capsule_dependencies
            .iter()
            .find(|item| item.name == dependency.alias)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is missing capsule dependency '{}'",
                    capsule_core::lockfile::CAPSULE_LOCK_FILE_NAME,
                    dependency.alias
                )
            })?;

        let manifest_path = ensure_runtime_tree_for_dependency(&locked).await?;
        let decision =
            capsule_core::router::route_manifest(&manifest_path, ExecutionProfile::Dev, None)?;

        let inject_args = merged_dependency_bindings(&decision.plan, &locked, &cli_bindings);
        let port = decision.plan.execution_port();
        let readiness_probe = decision.plan.selected_target_readiness_probe();

        reporter
            .notify(format!(
                "🔗 Starting external capsule dependency '{}'",
                dependency.alias
            ))
            .await?;

        let mut child =
            spawn_external_capsule_child(&dependency, &manifest_path, &inject_args, options)?;
        wait_for_dependency_readiness(&dependency.alias, &mut child, port, readiness_probe)?;

        if let Some(port) = port {
            let env = connection_env_vars(&dependency.alias, port);
            guard.caller_env.extend(env.clone());
            guard.caller_envs.push((dependency.alias.clone(), env));
        }
        guard.children.push(child);
    }

    Ok(guard)
}

#[cfg(test)]
mod tests;
