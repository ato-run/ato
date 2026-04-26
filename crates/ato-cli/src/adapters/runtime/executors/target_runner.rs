use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use capsule_core::execution_plan::canonical::{
    compute_policy_segment_hash, compute_provisioning_policy_hash,
};
use capsule_core::execution_plan::derive::{self, PlatformSnapshot};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::guard::{self, RuntimeGuardMode, RuntimeGuardResult};
use capsule_core::execution_plan::model::{ExecutionPlan, ExecutionTier};
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::lock_runtime;
use capsule_core::lockfile;
use capsule_core::router::{ManifestData, RuntimeDecision};
use capsule_core::CapsuleReporter;
use tracing::debug;

use super::launch_context::RuntimeLaunchContext;
use crate::application::pipeline::phases::run::{
    CompatibilityLegacyLockContext, PreparedRunContext,
};
use crate::application::workspace::state;
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
    let diagnostics = crate::ipc::validate::validate_manifest(
        prepared.bridge_manifest.as_toml(),
        &plan.manifest_dir,
    )
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

    let ipc_manifest_path = if prepared.bridge_manifest.as_toml().get("ipc").is_some() {
        None
    } else if plan.manifest_path.is_file()
        && plan
            .manifest_path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("toml"))
    {
        Some(plan.manifest_path.clone())
    } else {
        let workspace_manifest = prepared.workspace_root.join("capsule.toml");
        workspace_manifest.is_file().then_some(workspace_manifest)
    };

    let ipc_manifest = if let Some(path) = ipc_manifest_path {
        Some(
            toml::from_str::<toml::Value>(
                &std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
            )
            .with_context(|| {
                format!(
                    "failed to parse manifest for IPC resolution: {}",
                    path.display()
                )
            })?,
        )
    } else {
        None
    };

    let ipc_ctx = IpcContext::from_manifest(
        ipc_manifest
            .as_ref()
            .unwrap_or_else(|| prepared.bridge_manifest.as_toml()),
    )?;
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

    let (mut execution_plan, runtime_decision, tier) =
        if let Some(lock) = prepared.authoritative_lock.as_ref() {
            let resolved =
                lock_runtime::resolve_lock_runtime_model(lock, Some(plan.selected_target_label()))?;
            let overlay = prepared
                .effective_state
                .as_ref()
                .map(|effective| effective.compiler_overlay.clone())
                .unwrap_or_default();
            let execution_plan = derive::compile_execution_plan_from_lock(
                lock,
                &resolved,
                &overlay,
                &PlatformSnapshot::current(),
            )?;
            if let Some(effective_state) = prepared.effective_state.as_ref() {
                state::validate_execution_plan_against_policy(
                    &execution_plan,
                    &effective_state.policy,
                )?;
            }
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

    let launch_ctx = if let Some(execution_override) = prepared.execution_override.as_ref() {
        if execution_override.target_label != plan.selected_target_label() {
            return Err(AtoExecutionError::execution_contract_invalid(
                format!(
                    "execution override target '{}' does not match selected target '{}'",
                    execution_override.target_label,
                    plan.selected_target_label()
                ),
                None,
                Some(plan.selected_target_label()),
            )
            .into());
        }
        let mut effective_args = derive_launch_spec(plan)?.args;
        effective_args.extend(execution_override.args.clone());
        execution_plan.runtime.policy.args = effective_args;
        refresh_execution_plan_consent_hashes(&mut execution_plan)?;
        launch_ctx.with_command_args(execution_override.args.clone())
    } else {
        launch_ctx
    };

    if !options.defer_consent {
        let guard_manifest_dir = runtime_decision.plan.execution_working_directory();
        let guard_result = guard::evaluate_for_mode_with_authority(
            &execution_plan,
            &guard_manifest_dir,
            &options.enforcement,
            options.sandbox_mode,
            options.dangerously_skip_permissions,
            guard_mode,
            prepared.authoritative_lock.is_some(),
        )?;
        if options.assume_yes && is_transient_provider_workspace(&runtime_decision.plan) {
            crate::consent_store::record_consent(&execution_plan)?;
        } else {
            crate::consent_store::require_consent(&execution_plan, options.assume_yes)?;
        }

        return Ok(PreparedTargetExecution {
            execution_plan,
            runtime_decision,
            tier,
            guard_result,
            launch_ctx,
        });
    }

    let guard_result = guard::evaluate_for_mode_with_authority(
        &execution_plan,
        &runtime_decision.plan.manifest_dir,
        &options.enforcement,
        options.sandbox_mode,
        options.dangerously_skip_permissions,
        guard_mode,
        prepared.authoritative_lock.is_some(),
    )?;

    Ok(PreparedTargetExecution {
        execution_plan,
        runtime_decision,
        tier,
        guard_result,
        launch_ctx,
    })
}

fn is_transient_provider_workspace(plan: &ManifestData) -> bool {
    plan.manifest_dir.join("resolution.json").exists()
}

fn refresh_execution_plan_consent_hashes(execution_plan: &mut ExecutionPlan) -> Result<()> {
    execution_plan.consent.policy_segment_hash = compute_policy_segment_hash(
        &execution_plan.runtime,
        &execution_plan.consent.mount_set_algo_id,
        execution_plan.consent.mount_set_algo_version,
    )?;
    execution_plan.consent.provisioning_policy_hash =
        compute_provisioning_policy_hash(&execution_plan.provisioning)?;
    Ok(())
}

pub fn preflight_required_environment_variables(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<()> {
    let resolved = plan.execution_resolved_config_schema();
    if resolved.is_empty() {
        return Ok(());
    }

    let base_env = runtime_overrides::merged_env(plan.execution_env());
    let launch_env = launch_ctx.merged_env();

    // Single filter pass collecting both the bare names (back-compat with
    // legacy CLI consumers like `dispatch/run.rs::missing_required_env_keys`)
    // and the rich `ConfigField` entries (consumed by the desktop dynamic
    // form via the E103 envelope). The two arrays are constructed from the
    // same iterator so they stay index-aligned by construction:
    // `missing_schema[i].name == missing_keys[i]`.
    let mut missing_keys: Vec<String> = Vec::new();
    let mut missing_schema: Vec<capsule_core::types::ConfigField> = Vec::new();
    for field in resolved {
        if launch_env
            .get(&field.name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            continue;
        }
        if base_env
            .get(&field.name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            continue;
        }
        if std::env::var(&field.name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            continue;
        }
        missing_keys.push(field.name.clone());
        missing_schema.push(field);
    }

    if missing_keys.is_empty() {
        return Ok(());
    }

    Err(AtoExecutionError::missing_required_env(
        format!(
            "missing required environment variables for target '{}': {} (set them before `ato run`)",
            plan.selected_target_label(),
            missing_keys.join(", ")
        ),
        missing_keys,
        missing_schema,
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
        preflight_required_environment_variables, prepare_target_execution, resolve_launch_context,
        verify_lockfile_integrity, TargetLaunchOptions,
    };
    use crate::application::pipeline::phases::run::{
        CompatibilityLegacyLockContext, DerivedBridgeManifest, PreparedRunContext,
        RunExecutionOverride,
    };
    use crate::executors::launch_context::RuntimeLaunchContext;
    use crate::reporters::CliReporter;
    use capsule_core::execution_plan::canonical::{
        compute_policy_segment_hash, compute_provisioning_policy_hash,
    };
    use capsule_core::launch_spec::derive_launch_spec;
    use capsule_core::lockfile::{CapsuleLock, LockMeta};
    use capsule_core::router::{self, ExecutionProfile};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn injected_env_satisfies_required_env() {
        let mut manifest = toml::map::Map::new();
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert("type".to_string(), toml::Value::String("app".to_string()));
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

        let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::Value::Table(manifest),
            PathBuf::from("/tmp/capsule.toml"),
            PathBuf::from("/tmp"),
            ExecutionProfile::Dev,
            Some("default"),
            HashMap::new(),
        )
        .expect("execution descriptor");

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
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "node"
entrypoint = "index.js"
"#,
        )
        .expect("parse manifest");

        let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
            raw_manifest.clone(),
            manifest_path,
            manifest_dir.clone(),
            ExecutionProfile::Dev,
            Some("default"),
            HashMap::new(),
        )
        .expect("execution descriptor");
        let prepared = PreparedRunContext {
            authoritative_lock: None,
            lock_path: None,
            workspace_root: manifest_dir,
            effective_state: None,
            execution_override: None,
            bridge_manifest: crate::application::pipeline::phases::run::DerivedBridgeManifest::new(
                raw_manifest,
            ),
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

    #[tokio::test]
    async fn resolve_launch_context_falls_back_to_manifest_file_for_ipc_imports() {
        let temp_dir = tempdir().expect("tempdir");
        let manifest_path = temp_dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "node"
entrypoint = "index.js"

[ipc.imports.greeter]
from = "./missing-service"
"#,
        )
        .expect("write manifest");

        let raw_manifest = toml::from_str::<toml::Value>(
            &std::fs::read_to_string(&manifest_path).expect("read manifest"),
        )
        .expect("parse manifest");

        let plan = capsule_core::router::execution_descriptor_from_manifest_parts(
            raw_manifest,
            manifest_path.clone(),
            temp_dir.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("default"),
            HashMap::new(),
        )
        .expect("execution descriptor");
        let prepared = PreparedRunContext {
            authoritative_lock: None,
            lock_path: None,
            workspace_root: temp_dir.path().to_path_buf(),
            effective_state: None,
            execution_override: None,
            bridge_manifest: crate::application::pipeline::phases::run::DerivedBridgeManifest::new(
                toml::Value::Table(toml::map::Map::new()),
            ),
            validation_mode: capsule_core::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
        };

        let err = resolve_launch_context(&plan, &prepared, &Arc::new(CliReporter::new(false)))
            .await
            .expect_err("missing required IPC import should fail");

        assert!(err.to_string().contains("Required IPC import 'greeter'"));
    }

    #[test]
    fn prepare_target_execution_keeps_policy_args_in_sync_with_launch_args() {
        let temp_dir = tempdir().expect("tempdir");
        let manifest_path = temp_dir.path().join("capsule.toml");
        let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

default_target = "export"
runtime = "source/python"
runtime_version = "3.11"
run = "python3 tool.py --from-target""#;
        std::fs::write(&manifest_path, manifest_text).expect("write manifest");
        std::fs::write(temp_dir.path().join("uv.lock"), "# uv lock\n").expect("write uv.lock");

        let decision = router::route_manifest_with_state_overrides_and_validation_mode(
            &manifest_path,
            ExecutionProfile::Dev,
            Some("export"),
            HashMap::new(),
            capsule_core::types::ValidationMode::Strict,
        )
        .expect("route manifest");

        let prepared = PreparedRunContext {
            authoritative_lock: None,
            lock_path: None,
            workspace_root: temp_dir.path().to_path_buf(),
            effective_state: None,
            execution_override: Some(RunExecutionOverride {
                target_label: "export".to_string(),
                args: vec!["--from-export".to_string(), "--help".to_string()],
            }),
            bridge_manifest: DerivedBridgeManifest::new(
                toml::from_str(manifest_text).expect("parse manifest toml"),
            ),
            validation_mode: capsule_core::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
        };

        let execution = prepare_target_execution(
            &decision.plan,
            &prepared,
            RuntimeLaunchContext::empty(),
            &TargetLaunchOptions {
                enforcement: "audit".to_string(),
                sandbox_mode: true,
                dangerously_skip_permissions: true,
                assume_yes: true,
                preview_mode: false,
                defer_consent: true,
            },
        )
        .expect("prepare target execution");

        assert_eq!(
            execution.execution_plan.runtime.policy.args,
            vec![
                "--from-target".to_string(),
                "--from-export".to_string(),
                "--help".to_string()
            ]
        );
        assert_eq!(
            execution.launch_ctx.command_args(),
            &["--from-export".to_string(), "--help".to_string()]
        );
        assert_eq!(
            execution.execution_plan.consent.policy_segment_hash,
            compute_policy_segment_hash(
                &execution.execution_plan.runtime,
                &execution.execution_plan.consent.mount_set_algo_id,
                execution.execution_plan.consent.mount_set_algo_version,
            )
            .expect("recompute policy segment hash")
        );
        assert_eq!(
            execution.execution_plan.consent.provisioning_policy_hash,
            compute_provisioning_policy_hash(&execution.execution_plan.provisioning)
                .expect("recompute provisioning hash")
        );
    }

    #[test]
    fn prepare_target_execution_preserves_default_target_trailing_args() {
        let temp_dir = tempdir().expect("tempdir");
        let manifest_path = temp_dir.path().join("capsule.toml");
        let manifest_text = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

default_target = "default"
runtime = "source/python"
runtime_version = "3.11"
run = "python3 default.py --from-default""#;
        std::fs::write(&manifest_path, manifest_text).expect("write manifest");
        std::fs::write(temp_dir.path().join("uv.lock"), "# uv lock\n").expect("write uv.lock");

        let decision = router::route_manifest_with_state_overrides_and_validation_mode(
            &manifest_path,
            ExecutionProfile::Dev,
            Some("default"),
            HashMap::new(),
            capsule_core::types::ValidationMode::Strict,
        )
        .expect("route manifest");

        let prepared = PreparedRunContext {
            authoritative_lock: None,
            lock_path: None,
            workspace_root: temp_dir.path().to_path_buf(),
            effective_state: None,
            execution_override: Some(RunExecutionOverride {
                target_label: "default".to_string(),
                args: vec!["--help".to_string()],
            }),
            bridge_manifest: DerivedBridgeManifest::new(
                toml::from_str(manifest_text).expect("parse manifest toml"),
            ),
            validation_mode: capsule_core::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
        };

        let execution = prepare_target_execution(
            &decision.plan,
            &prepared,
            RuntimeLaunchContext::empty(),
            &TargetLaunchOptions {
                enforcement: "audit".to_string(),
                sandbox_mode: true,
                dangerously_skip_permissions: true,
                assume_yes: true,
                preview_mode: false,
                defer_consent: true,
            },
        )
        .expect("prepare target execution");

        assert_eq!(
            execution.execution_plan.runtime.policy.args,
            vec!["--from-default".to_string(), "--help".to_string()]
        );
        assert_eq!(execution.launch_ctx.command_args(), &["--help".to_string()]);
        assert_eq!(
            derive_launch_spec(&decision.plan)
                .expect("derive launch spec")
                .args,
            vec!["--from-default".to_string()]
        );
    }

    #[test]
    fn verify_lockfile_integrity_ignores_stray_lock_without_compatibility_context() {
        let temp_dir = tempdir().expect("tempdir");
        let manifest_path = temp_dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
run = "index.js""#,
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
