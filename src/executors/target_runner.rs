use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::guard::{self, RuntimeGuardResult};
use capsule_core::execution_plan::model::{ExecutionPlan, ExecutionTier};
use capsule_core::lockfile;
use capsule_core::router::{ManifestData, RuntimeDecision};
use capsule_core::CapsuleReporter;
use tracing::debug;

use super::launch_context::RuntimeLaunchContext;
use crate::ipc::inject::IpcContext;
use crate::reporters::CliReporter;
use crate::runtime_overrides;

#[derive(Debug, Clone)]
pub struct TargetLaunchOptions {
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub assume_yes: bool,
}

#[derive(Debug)]
pub struct PreparedTargetExecution {
    pub execution_plan: ExecutionPlan,
    pub runtime_decision: RuntimeDecision,
    pub tier: ExecutionTier,
    pub guard_result: RuntimeGuardResult,
    pub launch_ctx: RuntimeLaunchContext,
}

pub async fn resolve_launch_context(
    plan: &ManifestData,
    reporter: &Arc<CliReporter>,
) -> Result<RuntimeLaunchContext> {
    let raw_manifest_text = std::fs::read_to_string(&plan.manifest_path).map_err(|err| {
        AtoExecutionError::policy_violation(format!(
            "Failed to read manifest for IPC validation: {}",
            err
        ))
    })?;
    let raw_manifest: toml::Value = toml::from_str(&raw_manifest_text).map_err(|err| {
        AtoExecutionError::policy_violation(format!(
            "Failed to parse manifest for IPC validation: {}",
            err
        ))
    })?;
    let diagnostics = crate::ipc::validate::validate_manifest(&raw_manifest, &plan.manifest_dir)
        .map_err(|err| {
            AtoExecutionError::policy_violation(format!("IPC validation failed: {err}"))
        })?;

    if crate::ipc::validate::has_errors(&diagnostics) {
        return Err(
            AtoExecutionError::policy_violation(crate::ipc::validate::format_diagnostics(
                &diagnostics,
            ))
            .into(),
        );
    }

    for diagnostic in diagnostics {
        reporter.warn(diagnostic.to_string()).await?;
    }

    let ipc_ctx = IpcContext::from_manifest(&plan.manifest)?;
    if ipc_ctx.has_ipc() {
        debug!(
            resolved_services = ipc_ctx.resolved_count,
            injected_env_vars = ipc_ctx.env_vars.len(),
            activation = ?ipc_ctx.activation_mode,
            "IPC resolved"
        );
    }
    for warning in &ipc_ctx.warnings {
        reporter.warn(warning.clone()).await?;
    }

    Ok(RuntimeLaunchContext::from_ipc(ipc_ctx))
}

pub fn prepare_target_execution(
    plan: &ManifestData,
    launch_ctx: RuntimeLaunchContext,
    options: &TargetLaunchOptions,
) -> Result<PreparedTargetExecution> {
    preflight_required_environment_variables(plan, &launch_ctx)?;
    preflight_web_services_requirements(plan)?;
    verify_lockfile_integrity(&plan.manifest_path)?;

    let compiled = capsule_core::execution_plan::derive::compile_execution_plan(
        &plan.manifest_path,
        plan.profile,
        Some(plan.selected_target_label()),
    )?;

    let guard_result = guard::evaluate(
        &compiled.execution_plan,
        &compiled.runtime_decision.plan.manifest_dir,
        &options.enforcement,
        options.sandbox_mode,
        options.dangerously_skip_permissions,
    )?;

    crate::consent_store::require_consent(&compiled.execution_plan, options.assume_yes)?;

    Ok(PreparedTargetExecution {
        execution_plan: compiled.execution_plan,
        runtime_decision: compiled.runtime_decision,
        tier: compiled.tier,
        guard_result,
        launch_ctx,
    })
}

pub fn preflight_required_environment_variables(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<()> {
    let required = plan.execution_required_envs();
    if required.is_empty() {
        return Ok(());
    }

    let base_env = runtime_overrides::merged_env(plan.execution_env());
    let launch_env = launch_ctx.merged_env();
    let missing: Vec<String> = required
        .into_iter()
        .filter(|name| {
            if launch_env
                .get(name)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                return false;
            }
            if base_env
                .get(name)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                return false;
            }
            std::env::var(name)
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        })
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    Err(AtoExecutionError::policy_violation(format!(
        "missing required environment variables for target '{}': {} (set them before `ato run`)",
        plan.selected_target_label(),
        missing.join(", ")
    ))
    .into())
}

fn preflight_web_services_requirements(plan: &ManifestData) -> Result<()> {
    if !plan.is_web_services_mode() || plan.is_orchestration_mode() {
        return Ok(());
    }

    let services = plan.services();
    if !services.contains_key("main") {
        return Err(AtoExecutionError::policy_violation(
            "web/deno services mode requires top-level [services.main]",
        )
        .into());
    }

    Ok(())
}

fn verify_lockfile_integrity(manifest_path: &Path) -> Result<()> {
    let lock_path = manifest_path
        .parent()
        .map(|parent| parent.join("capsule.lock"))
        .filter(|path| path.exists());

    let Some(lock_path) = lock_path else {
        return Ok(());
    };

    lockfile::verify_lockfile_manifest(manifest_path, &lock_path).map_err(|err| {
        if err.to_string().contains("manifest hash mismatch") {
            AtoExecutionError::lockfile_tampered(err.to_string(), Some("capsule.lock"))
        } else {
            AtoExecutionError::policy_violation(err.to_string())
        }
    })?;
    debug!("capsule.lock integrity verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::preflight_required_environment_variables;
    use crate::executors::launch_context::RuntimeLaunchContext;
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn injected_env_satisfies_required_env() {
        let mut manifest = toml::map::Map::new();
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );

        let mut target = toml::map::Map::new();
        target.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target.insert(
            "driver".to_string(),
            toml::Value::String("native".to_string()),
        );
        target.insert(
            "entrypoint".to_string(),
            toml::Value::String("main.py".to_string()),
        );
        target.insert(
            "required_env".to_string(),
            toml::Value::Array(vec![toml::Value::String("DATABASE_URL".to_string())]),
        );

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        let plan = ManifestData {
            manifest: toml::Value::Table(manifest),
            manifest_path: PathBuf::from("/tmp/capsule.toml"),
            manifest_dir: PathBuf::from("/tmp"),
            profile: ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: HashMap::new(),
        };

        let launch_ctx = RuntimeLaunchContext::empty().with_injected_env(
            [(
                "DATABASE_URL".to_string(),
                "mysql://127.0.0.1:3306/app".to_string(),
            )]
            .into_iter()
            .collect(),
        );

        assert!(preflight_required_environment_variables(&plan, &launch_ctx).is_ok());
    }
}
