use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use capsule_core::execution_plan::derive::{self, PlatformSnapshot};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::guard::{self, RuntimeGuardMode, RuntimeGuardResult};
use capsule_core::execution_plan::model::{ExecutionPlan, ExecutionTier};
use capsule_core::lock_runtime::{self, LockCompilerOverlay};
use capsule_core::lockfile;
use capsule_core::router::{ManifestData, RuntimeDecision};
use capsule_core::CapsuleReporter;
use tracing::debug;

use super::launch_context::RuntimeLaunchContext;
use crate::application::pipeline::phases::run::{
    CompatibilityLegacyLockContext, PreparedRunContext,
};
use crate::ipc::inject::IpcContext;
use crate::reporters::CliReporter;
use crate::runtime::overrides as runtime_overrides;

#[derive(Debug, Clone)]
pub struct TargetLaunchOptions {
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub assume_yes: bool,
    pub preview_mode: bool,
    pub defer_consent: bool,
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
    prepared: &PreparedRunContext,
    reporter: &Arc<CliReporter>,
) -> Result<RuntimeLaunchContext> {
    let diagnostics =
        crate::ipc::validate::validate_manifest(&prepared.raw_manifest, &plan.manifest_dir)
            .map_err(|err| {
                AtoExecutionError::execution_contract_invalid(
                    format!("IPC validation failed: {err}"),
                    None,
                    None,
                )
            })?;

    if crate::ipc::validate::has_errors(&diagnostics) {
        return Err(AtoExecutionError::execution_contract_invalid(
            crate::ipc::validate::format_diagnostics(&diagnostics),
            None,
            None,
        )
        .into());
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
    prepared: &PreparedRunContext,
    launch_ctx: RuntimeLaunchContext,
    options: &TargetLaunchOptions,
) -> Result<PreparedTargetExecution> {
    preflight_required_environment_variables(plan, &launch_ctx)?;
    preflight_web_services_requirements(plan)?;
    verify_lockfile_integrity(
        &plan.manifest_path,
        prepared.compatibility_legacy_lock.as_ref(),
    )?;

    let validation_mode = prepared.validation_mode;
    let guard_mode = if options.preview_mode {
        RuntimeGuardMode::Preview
    } else {
        RuntimeGuardMode::Strict
    };

    let (execution_plan, runtime_decision, tier) =
        if let Some(lock) = prepared.authoritative_lock.as_ref() {
            let resolved =
                lock_runtime::resolve_lock_runtime_model(lock, Some(plan.selected_target_label()))?;
            let execution_plan = derive::compile_execution_plan_from_lock(
                lock,
                &resolved,
                &LockCompilerOverlay::default(),
                &PlatformSnapshot::current(),
            )?;
            let tier =
                derive::derive_tier(execution_plan.target.runtime, execution_plan.target.driver)?;
            let kind = match execution_plan.target.runtime {
                capsule_core::execution_plan::model::ExecutionRuntime::Oci => {
                    capsule_core::router::RuntimeKind::Oci
                }
                capsule_core::execution_plan::model::ExecutionRuntime::Wasm => {
                    capsule_core::router::RuntimeKind::Wasm
                }
                capsule_core::execution_plan::model::ExecutionRuntime::Web => {
                    capsule_core::router::RuntimeKind::Web
                }
                capsule_core::execution_plan::model::ExecutionRuntime::Source => {
                    capsule_core::router::RuntimeKind::Source
                }
            };
            (
                execution_plan,
                RuntimeDecision {
                    kind,
                    reason: format!("lock-derived target {}", plan.selected_target_label()),
                    plan: plan.clone(),
                },
                tier,
            )
        } else {
            let compiled =
                capsule_core::execution_plan::derive::compile_execution_plan_with_validation_mode(
                    &plan.manifest_path,
                    plan.profile,
                    Some(plan.selected_target_label()),
                    validation_mode,
                )?;
            (
                compiled.execution_plan,
                compiled.runtime_decision,
                compiled.tier,
            )
        };

    let guard_result = guard::evaluate_for_mode(
        &execution_plan,
        &runtime_decision.plan.manifest_dir,
        &options.enforcement,
        options.sandbox_mode,
        options.dangerously_skip_permissions,
        guard_mode,
    )?;

    if !options.defer_consent {
        crate::consent_store::require_consent(&execution_plan, options.assume_yes)?;
    }

    Ok(PreparedTargetExecution {
        execution_plan,
        runtime_decision,
        tier,
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

    Err(AtoExecutionError::missing_required_env(
        format!(
            "missing required environment variables for target '{}': {} (set them before `ato run`)",
            plan.selected_target_label(),
            missing.join(", ")
        ),
        missing,
        Some(plan.selected_target_label()),
    )
    .into())
}

fn preflight_web_services_requirements(plan: &ManifestData) -> Result<()> {
    if !plan.is_web_services_mode() || plan.is_orchestration_mode() {
        return Ok(());
    }

    let services = plan.services();
    if !services.contains_key("main") {
        return Err(AtoExecutionError::execution_contract_invalid(
            "web/deno services mode requires top-level [services.main]",
            Some("services.main"),
            Some("main"),
        )
        .into());
    }

    Ok(())
}

fn verify_lockfile_integrity(
    manifest_path: &Path,
    compatibility_legacy_lock: Option<&CompatibilityLegacyLockContext>,
) -> Result<()> {
    let Some(compatibility_legacy_lock) = compatibility_legacy_lock else {
        return Ok(());
    };

    let validation_manifest_path = if manifest_path == compatibility_legacy_lock.manifest_path {
        manifest_path
    } else {
        &compatibility_legacy_lock.manifest_path
    };

    lockfile::verify_lockfile_manifest(validation_manifest_path, &compatibility_legacy_lock.path)
        .map_err(|err| {
        if err.to_string().contains("manifest hash mismatch") {
            AtoExecutionError::lockfile_tampered(err.to_string(), Some("capsule.lock"))
        } else {
            AtoExecutionError::lock_incomplete(err.to_string(), Some("capsule.lock"))
        }
    })?;
    debug!("capsule.lock integrity verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        preflight_required_environment_variables, resolve_launch_context, verify_lockfile_integrity,
    };
    use crate::application::pipeline::phases::run::{
        CompatibilityLegacyLockContext, PreparedRunContext,
    };
    use crate::executors::launch_context::RuntimeLaunchContext;
    use crate::reporters::CliReporter;
    use capsule_core::lockfile::{CapsuleLock, LockMeta};
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

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

    #[tokio::test]
    async fn resolve_launch_context_uses_prepared_manifest_without_file_read() {
        let temp_dir = tempdir().expect("tempdir");
        let manifest_path = temp_dir.path().join("missing-capsule.toml");
        let manifest_dir = temp_dir.path().to_path_buf();

        let raw_manifest = toml::from_str::<toml::Value>(
            r#"
name = "demo"
default_target = "default"

[targets.default]
runtime = "source"
driver = "node"
entrypoint = "index.js"
"#,
        )
        .expect("parse manifest");

        let plan = ManifestData {
            manifest: raw_manifest.clone(),
            manifest_path,
            manifest_dir,
            profile: ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: HashMap::new(),
        };
        let prepared = PreparedRunContext {
            authoritative_lock: None,
            raw_manifest,
            validation_mode: capsule_core::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
        };

        let launch_ctx =
            resolve_launch_context(&plan, &prepared, &Arc::new(CliReporter::new(false)))
                .await
                .expect("resolve launch context without reading manifest file");

        assert!(launch_ctx.ipc().is_none());
    }

    #[test]
    fn verify_lockfile_integrity_ignores_stray_lock_without_compatibility_context() {
        let temp_dir = tempdir().expect("tempdir");
        let manifest_path = temp_dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "node"
entrypoint = "index.js"
"#,
        )
        .expect("write manifest");
        let legacy_lock_path = temp_dir.path().join("capsule.lock.json");
        let lock = CapsuleLock {
            version: "1".to_string(),
            meta: LockMeta {
                created_at: "2026-03-25T00:00:00Z".to_string(),
                manifest_hash: "sha256:not-the-real-manifest".to_string(),
            },
            allowlist: None,
            capsule_dependencies: Vec::new(),
            injected_data: HashMap::new(),
            tools: None,
            runtimes: None,
            targets: HashMap::new(),
        };
        std::fs::write(
            &legacy_lock_path,
            serde_json::to_string_pretty(&lock).expect("serialize lock"),
        )
        .expect("write legacy lock");

        verify_lockfile_integrity(&manifest_path, None)
            .expect("stray legacy lock must not affect non-compatibility contexts");

        let compatibility_legacy_lock = CompatibilityLegacyLockContext {
            manifest_path: manifest_path.clone(),
            path: legacy_lock_path,
            lock,
        };
        let error = verify_lockfile_integrity(&manifest_path, Some(&compatibility_legacy_lock))
            .expect_err("compatibility legacy lock must be validated when explicitly provided");

        assert!(error.to_string().contains("ATO_ERR_LOCKFILE_TAMPERED"));
    }
}
