#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::execution_plan::derive::derive_tier;
use crate::execution_plan::error::AtoExecutionError;
use crate::execution_plan::model::{
    ExecutionDriver, ExecutionPlan, ExecutionRuntime, ExecutionTier,
};
use crate::lockfile::{
    lockfile_output_path, resolve_existing_lockfile_path, CAPSULE_LOCK_FILE_NAME,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredLock {
    CapsuleLock,
    DenoLockOrPackageLock,
    NodeDependencyLock,
    UvLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutorKind {
    WebStatic,
    Deno,
    NodeCompat,
    Native,
    Wasm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeGuardResult {
    pub requires_sandbox_opt_in: bool,
    pub required_lock: Option<RequiredLock>,
    pub executor_kind: ExecutorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGuardMode {
    Strict,
    Preview,
}

pub fn evaluate(
    plan: &ExecutionPlan,
    manifest_dir: &Path,
    enforcement: &str,
    sandbox_mode: bool,
    dangerously_skip_permissions: bool,
) -> Result<RuntimeGuardResult, AtoExecutionError> {
    evaluate_for_mode(
        plan,
        manifest_dir,
        enforcement,
        sandbox_mode,
        dangerously_skip_permissions,
        RuntimeGuardMode::Strict,
    )
}

pub fn evaluate_for_mode(
    plan: &ExecutionPlan,
    manifest_dir: &Path,
    enforcement: &str,
    sandbox_mode: bool,
    dangerously_skip_permissions: bool,
    mode: RuntimeGuardMode,
) -> Result<RuntimeGuardResult, AtoExecutionError> {
    evaluate_for_mode_with_authority(
        plan,
        manifest_dir,
        enforcement,
        sandbox_mode,
        dangerously_skip_permissions,
        mode,
        false,
    )
}

pub fn evaluate_for_mode_with_authority(
    plan: &ExecutionPlan,
    manifest_dir: &Path,
    enforcement: &str,
    sandbox_mode: bool,
    dangerously_skip_permissions: bool,
    mode: RuntimeGuardMode,
    has_authoritative_lock: bool,
) -> Result<RuntimeGuardResult, AtoExecutionError> {
    let runtime = plan.target.runtime;
    let driver = plan.target.driver;

    let tier = derive_tier(runtime, driver)?;
    if requires_capsule_lock(runtime, driver)
        && matches!(tier, ExecutionTier::Tier1)
        && !has_authoritative_lock
        && !dangerously_skip_permissions
        && !resolve_capsule_lock_path(manifest_dir).exists()
        && !matches!(mode, RuntimeGuardMode::Preview)
    {
        return Err(AtoExecutionError::lock_incomplete(
            "capsule.lock.json is required for Tier1 execution",
            Some(CAPSULE_LOCK_FILE_NAME),
        ));
    }

    let required_lock = resolve_required_lock(runtime, driver)?;
    match required_lock {
        Some(RequiredLock::DenoLockOrPackageLock) => {
            if resolve_deno_dependency_lock_path(manifest_dir).is_none()
                && !matches!(mode, RuntimeGuardMode::Preview)
            {
                return Err(AtoExecutionError::lock_incomplete(
                    "deno.lock or package-lock.json is required for source/deno execution",
                    Some("deno.lock"),
                ));
            }
        }
        Some(RequiredLock::NodeDependencyLock) => {
            if resolve_node_dependency_lock_path(manifest_dir).is_none()
                && !has_authoritative_lock
                && !dangerously_skip_permissions
                && !matches!(mode, RuntimeGuardMode::Preview)
            {
                return Err(AtoExecutionError::lock_incomplete(
                    "package-lock.json, yarn.lock, pnpm-lock.yaml, bun.lock, or bun.lockb is required for source/node Tier1 execution",
                    Some("package-lock.json"),
                ));
            }
        }
        Some(RequiredLock::UvLock) => {
            if resolve_python_dependency_path(manifest_dir).is_none()
                && !has_authoritative_lock
                && !dangerously_skip_permissions
                && !matches!(mode, RuntimeGuardMode::Preview)
            {
                return Err(AtoExecutionError::lock_incomplete(
                    "uv.lock or requirements.txt is required for source/python execution",
                    Some("uv.lock"),
                ));
            }
        }
        Some(RequiredLock::CapsuleLock) | None => {}
    }

    let requires_sandbox_opt_in =
        matches!(
            (runtime, driver),
            (ExecutionRuntime::Source, ExecutionDriver::Native)
                | (ExecutionRuntime::Source, ExecutionDriver::Python)
                | (ExecutionRuntime::Web, ExecutionDriver::Python)
        ) && !is_desktop_native_delivery_runtime(manifest_dir, runtime, driver);

    if requires_sandbox_opt_in
        && !(sandbox_mode || dangerously_skip_permissions)
        && !matches!(mode, RuntimeGuardMode::Preview)
    {
        return Err(AtoExecutionError::policy_violation(
            "source/native|python execution requires explicit --sandbox opt-in or --dangerously-skip-permissions",
        ));
    }

    if requires_sandbox_opt_in
        && !dangerously_skip_permissions
        && enforcement != "strict"
        && !matches!(mode, RuntimeGuardMode::Preview)
    {
        return Err(AtoExecutionError::policy_violation(
            "source/native|python execution requires strict sandbox enforcement",
        ));
    }

    let executor_kind = resolve_executor_kind(runtime, driver)?;

    Ok(RuntimeGuardResult {
        requires_sandbox_opt_in,
        required_lock,
        executor_kind,
    })
}

fn resolve_executor_kind(
    runtime: ExecutionRuntime,
    driver: ExecutionDriver,
) -> Result<ExecutorKind, AtoExecutionError> {
    match (runtime, driver) {
        (ExecutionRuntime::Web, ExecutionDriver::Static) => Ok(ExecutorKind::WebStatic),
        (ExecutionRuntime::Web, ExecutionDriver::Deno) => Ok(ExecutorKind::Deno),
        (ExecutionRuntime::Web, ExecutionDriver::Node) => Ok(ExecutorKind::NodeCompat),
        (ExecutionRuntime::Web, ExecutionDriver::Python) => Ok(ExecutorKind::Native),
        (ExecutionRuntime::Wasm, ExecutionDriver::Wasmtime) => Ok(ExecutorKind::Wasm),
        (ExecutionRuntime::Source, ExecutionDriver::Deno) => Ok(ExecutorKind::Deno),
        (ExecutionRuntime::Source, ExecutionDriver::Node) => Ok(ExecutorKind::NodeCompat),
        (ExecutionRuntime::Source, ExecutionDriver::Native)
        | (ExecutionRuntime::Source, ExecutionDriver::Python) => Ok(ExecutorKind::Native),
        _ => Err(AtoExecutionError::policy_violation(format!(
            "unsupported runtime/driver pair for guard: runtime='{}' driver='{}'",
            runtime.as_str(),
            driver.as_str()
        ))),
    }
}

fn resolve_required_lock(
    runtime: ExecutionRuntime,
    driver: ExecutionDriver,
) -> Result<Option<RequiredLock>, AtoExecutionError> {
    match (runtime, driver) {
        (ExecutionRuntime::Web, ExecutionDriver::Static) => Ok(None),
        (ExecutionRuntime::Web, ExecutionDriver::Deno)
        | (ExecutionRuntime::Source, ExecutionDriver::Deno) => {
            Ok(Some(RequiredLock::DenoLockOrPackageLock))
        }
        (ExecutionRuntime::Web, ExecutionDriver::Node)
        | (ExecutionRuntime::Source, ExecutionDriver::Node) => {
            Ok(Some(RequiredLock::NodeDependencyLock))
        }
        (ExecutionRuntime::Web, ExecutionDriver::Python)
        | (ExecutionRuntime::Source, ExecutionDriver::Python) => Ok(Some(RequiredLock::UvLock)),
        (ExecutionRuntime::Wasm, ExecutionDriver::Wasmtime) => Ok(Some(RequiredLock::CapsuleLock)),
        (ExecutionRuntime::Source, ExecutionDriver::Native) => Ok(None),
        _ => Err(AtoExecutionError::policy_violation(format!(
            "unsupported runtime/driver pair for lock policy: runtime='{}' driver='{}'",
            runtime.as_str(),
            driver.as_str()
        ))),
    }
}

fn resolve_capsule_lock_path(manifest_dir: &Path) -> PathBuf {
    resolve_existing_lockfile_path(manifest_dir)
        .unwrap_or_else(|| lockfile_output_path(manifest_dir))
}

fn is_desktop_native_delivery_runtime(
    manifest_dir: &Path,
    runtime: ExecutionRuntime,
    driver: ExecutionDriver,
) -> bool {
    if !matches!(
        (runtime, driver),
        (ExecutionRuntime::Source, ExecutionDriver::Native)
    ) {
        return false;
    }

    let lock_path = manifest_dir.join(CAPSULE_LOCK_FILE_NAME);
    let Ok(bytes) = std::fs::read(&lock_path) else {
        return false;
    };
    let Ok(lock): Result<Value, _> = serde_json::from_slice(&bytes) else {
        return false;
    };

    lock.get("contract")
        .and_then(|value| value.get("delivery"))
        .and_then(|value| value.get("artifact"))
        .and_then(Value::as_object)
        .is_some_and(|artifact| {
            artifact.get("kind").and_then(Value::as_str) == Some("desktop-native")
                && artifact
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .is_some_and(|path| !path.is_empty())
        })
}

fn requires_capsule_lock(runtime: ExecutionRuntime, driver: ExecutionDriver) -> bool {
    !matches!(
        (runtime, driver),
        (ExecutionRuntime::Web, ExecutionDriver::Static)
    )
}

fn resolve_deno_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("deno.lock"),
        manifest_dir.join("source").join("deno.lock"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_package_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("package-lock.json"),
        manifest_dir.join("source").join("package-lock.json"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_pnpm_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("pnpm-lock.yaml"),
        manifest_dir.join("source").join("pnpm-lock.yaml"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_yarn_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("yarn.lock"),
        manifest_dir.join("source").join("yarn.lock"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_bun_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("bun.lock"),
        manifest_dir.join("bun.lockb"),
        manifest_dir.join("source").join("bun.lock"),
        manifest_dir.join("source").join("bun.lockb"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_deno_dependency_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    resolve_deno_lock_path(manifest_dir).or_else(|| resolve_node_dependency_lock_path(manifest_dir))
}

fn resolve_node_dependency_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    resolve_package_lock_path(manifest_dir)
        .or_else(|| resolve_yarn_lock_path(manifest_dir))
        .or_else(|| resolve_pnpm_lock_path(manifest_dir))
        .or_else(|| resolve_bun_lock_path(manifest_dir))
}

fn resolve_uv_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("uv.lock"),
        manifest_dir.join("source").join("uv.lock"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_python_requirements_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("requirements.txt"),
        manifest_dir.join("source").join("requirements.txt"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_python_dependency_path(manifest_dir: &Path) -> Option<PathBuf> {
    resolve_uv_lock_path(manifest_dir).or_else(|| resolve_python_requirements_path(manifest_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_plan::model::{
        CapsuleRef, Consent, ConsentKey, NonInteractiveBehavior, Platform, Provisioning,
        ProvisioningNetwork, Reproducibility, Runtime, RuntimeFilesystemPolicy,
        RuntimeNetworkPolicy, RuntimePolicy, RuntimeSecretsPolicy, SecretDelivery, TargetRef,
    };

    fn sample_plan(runtime: ExecutionRuntime, driver: ExecutionDriver) -> ExecutionPlan {
        ExecutionPlan {
            schema_version: "1".to_string(),
            capsule: CapsuleRef {
                scoped_id: "local/sample".to_string(),
                version: "1.0.0".to_string(),
            },
            target: TargetRef {
                label: "cli".to_string(),
                runtime,
                driver,
                language: None,
            },
            provisioning: Provisioning {
                network: ProvisioningNetwork {
                    allow_registry_hosts: Vec::new(),
                },
                lock_required: true,
                integrity_required: true,
                allowed_registries: Vec::new(),
            },
            runtime: Runtime {
                policy: RuntimePolicy {
                    network: RuntimeNetworkPolicy {
                        allow_hosts: Vec::new(),
                    },
                    filesystem: RuntimeFilesystemPolicy {
                        read_only: Vec::new(),
                        read_write: Vec::new(),
                    },
                    secrets: RuntimeSecretsPolicy {
                        allow_secret_ids: Vec::new(),
                        delivery: SecretDelivery::Fd,
                    },
                    args: Vec::new(),
                },
                fail_closed: true,
                non_interactive_behavior: NonInteractiveBehavior::DenyIfUnconsented,
            },
            consent: Consent {
                key: ConsentKey {
                    scoped_id: "local/sample".to_string(),
                    version: "1.0.0".to_string(),
                    target_label: "cli".to_string(),
                },
                policy_segment_hash: "blake3:policy".to_string(),
                provisioning_policy_hash: "blake3:provisioning".to_string(),
                mount_set_algo_id: "lockfile_mountset_v1".to_string(),
                mount_set_algo_version: 1,
            },
            reproducibility: Reproducibility {
                platform: Platform {
                    os: "darwin".to_string(),
                    arch: "arm64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
        }
    }

    #[test]
    fn node_tier1_does_not_require_sandbox_when_locks_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("capsule.lock"), "").expect("write capsule.lock");
        std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write package-lock");

        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Node);
        let result = evaluate(&plan, tmp.path(), "strict", false, false).expect("guard pass");
        assert!(!result.requires_sandbox_opt_in);
        assert_eq!(result.required_lock, Some(RequiredLock::NodeDependencyLock));
        assert_eq!(result.executor_kind, ExecutorKind::NodeCompat);
    }

    #[test]
    fn python_tier2_requires_sandbox_opt_in() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("uv.lock"), "").expect("write uv.lock");
        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Python);

        let err = evaluate(&plan, tmp.path(), "strict", false, false).expect_err("must reject");
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
        assert!(err.message.contains("--sandbox"));
    }

    #[test]
    fn python_tier2_allows_dangerous_skip_permissions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("uv.lock"), "").expect("write uv.lock");
        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Python);

        let result = evaluate(&plan, tmp.path(), "strict", false, true).expect("guard pass");
        assert!(result.requires_sandbox_opt_in);
        assert_eq!(result.executor_kind, ExecutorKind::Native);
    }

    #[test]
    fn desktop_native_delivery_runtime_skips_source_native_opt_in_requirement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join(CAPSULE_LOCK_FILE_NAME),
            r#"{
    "contract": {
        "delivery": {
            "artifact": {
                "kind": "desktop-native",
                "path": "MyApp.app/Contents/MacOS/MyApp"
            }
        }
    }
}"#,
        )
        .expect("write delivery lock");
        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Native);

        let result = evaluate(&plan, tmp.path(), "strict", false, false)
            .expect("desktop-native delivery should bypass explicit opt-in");

        assert!(!result.requires_sandbox_opt_in);
        assert_eq!(result.executor_kind, ExecutorKind::Native);
    }

    #[test]
    fn node_requires_package_lock_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("capsule.lock"), "").expect("write capsule.lock");
        std::fs::write(tmp.path().join("source").join("package-lock.json"), "{}").unwrap_or_else(
            |_| {
                std::fs::create_dir_all(tmp.path().join("source")).expect("create source");
                std::fs::write(tmp.path().join("source").join("package-lock.json"), "{}")
                    .expect("write package-lock in source");
            },
        );

        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Node);
        let result = evaluate(&plan, tmp.path(), "strict", false, false).expect("guard pass");
        assert_eq!(result.required_lock, Some(RequiredLock::NodeDependencyLock));
    }

    #[test]
    fn node_accepts_yarn_lock_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("capsule.lock"), "").expect("write capsule.lock");
        std::fs::write(
            tmp.path().join("source").join("yarn.lock"),
            "# yarn lockfile v1\n",
        )
        .unwrap_or_else(|_| {
            std::fs::create_dir_all(tmp.path().join("source")).expect("create source");
            std::fs::write(
                tmp.path().join("source").join("yarn.lock"),
                "# yarn lockfile v1\n",
            )
            .expect("write yarn lock in source");
        });

        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Node);
        let result = evaluate(&plan, tmp.path(), "strict", false, false).expect("guard pass");
        assert_eq!(result.required_lock, Some(RequiredLock::NodeDependencyLock));
    }

    #[test]
    fn tier1_requires_capsule_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write package-lock");

        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Node);
        let err = evaluate(&plan, tmp.path(), "strict", false, false).expect_err("must reject");
        assert_eq!(err.code, "ATO_ERR_PROVISIONING_LOCK_INCOMPLETE");
        assert!(err.message.contains("capsule.lock"));
    }

    #[test]
    fn authoritative_lock_bypasses_capsule_lock_requirement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write package-lock");

        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Node);
        let result = evaluate_for_mode_with_authority(
            &plan,
            tmp.path(),
            "strict",
            false,
            false,
            RuntimeGuardMode::Strict,
            true,
        )
        .expect("guard pass");

        assert_eq!(result.required_lock, Some(RequiredLock::NodeDependencyLock));
        assert_eq!(result.executor_kind, ExecutorKind::NodeCompat);
    }

    #[test]
    fn web_static_does_not_require_capsule_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plan = sample_plan(ExecutionRuntime::Web, ExecutionDriver::Static);
        let result = evaluate(&plan, tmp.path(), "strict", false, false).expect("guard pass");
        assert_eq!(result.required_lock, None);
        assert_eq!(result.executor_kind, ExecutorKind::WebStatic);
    }

    #[test]
    fn web_node_requires_package_lock() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("capsule.lock"), "").expect("write capsule.lock");
        let plan = sample_plan(ExecutionRuntime::Web, ExecutionDriver::Node);
        let err = evaluate(&plan, tmp.path(), "strict", false, false).expect_err("must reject");
        assert_eq!(err.code, "ATO_ERR_PROVISIONING_LOCK_INCOMPLETE");
        assert!(err.message.contains("package-lock.json"));
    }

    #[test]
    fn web_python_requires_sandbox_opt_in() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("uv.lock"), "").expect("write uv.lock");
        let plan = sample_plan(ExecutionRuntime::Web, ExecutionDriver::Python);
        let err = evaluate(&plan, tmp.path(), "strict", false, false).expect_err("must reject");
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
        assert!(err.message.contains("--sandbox"));
    }

    #[test]
    fn preview_mode_skips_lock_and_sandbox_fail_closed_checks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plan = sample_plan(ExecutionRuntime::Source, ExecutionDriver::Python);

        let result = evaluate_for_mode(
            &plan,
            tmp.path(),
            "best_effort",
            false,
            false,
            RuntimeGuardMode::Preview,
        )
        .expect("preview guard pass");

        assert!(result.requires_sandbox_opt_in);
        assert_eq!(result.required_lock, Some(RequiredLock::UvLock));
        assert_eq!(result.executor_kind, ExecutorKind::Native);
    }
}
