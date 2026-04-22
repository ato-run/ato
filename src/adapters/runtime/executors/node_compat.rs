#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::{
    collections::BTreeMap,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
};

use anyhow::{Context, Result};

use capsule_core::execution_plan::canonical::{
    compute_policy_segment_hash, compute_provisioning_policy_hash,
};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::model::{ExecutionPlan, ExecutionRuntime};
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::router::ManifestData;

use crate::common::proxy;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;
use crate::runtime::provider_workspace;

use super::launch_context::RuntimeLaunchContext;

struct PreparedCommand {
    cmd: Command,
    #[cfg(unix)]
    _secret_fd_guard: Option<std::fs::File>,
}

pub fn execute(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    dangerously_skip_permissions: bool,
) -> Result<i32> {
    verify_execution_plan_hashes(execution_plan)?;

    let launch_spec = derive_launch_spec(plan)?;

    // Package manager entrypoints (npm, yarn, pnpm, bun) run scripts directly — no lockfile needed.
    if is_package_manager_entrypoint(&launch_spec.command) {
        let PreparedCommand {
            mut cmd,
            #[cfg(unix)]
            _secret_fd_guard,
        } = build_package_manager_command(
            plan,
            authoritative_lock,
            execution_plan,
            &launch_spec.working_dir,
            &launch_spec.command,
            &launch_spec.args,
            launch_ctx,
        )?;
        let status = cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("Failed to execute package manager for node compat")?;
        return Ok(status.code().unwrap_or(1));
    }

    let Some(_) = launch_spec.required_lockfile.as_ref() else {
        return Err(AtoExecutionError::lock_incomplete(
            "source/node Tier1 execution requires a Node lockfile",
            Some("package-lock.json|yarn.lock|pnpm-lock.yaml|bun.lock|bun.lockb"),
        )
        .into());
    };

    if let Some(PreparedCommand {
        mut cmd,
        #[cfg(unix)]
        _secret_fd_guard,
    }) = maybe_build_direct_node_command(
        plan,
        authoritative_lock,
        execution_plan,
        &launch_spec.working_dir,
        &launch_spec.command,
        &launch_spec.args,
        launch_ctx,
    )? {
        let status = cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context("Failed to execute direct node runtime for node compat")?;

        return Ok(status.code().unwrap_or(1));
    }

    let deno_bin = runtime_manager::ensure_deno_binary_with_authority(plan, authoritative_lock)?;

    let use_compat_flag = deno_supports_compat_flag(&deno_bin)?;

    run_provisioning(
        &deno_bin,
        &launch_spec.working_dir,
        &launch_spec.command,
        launch_ctx,
    )?;
    let PreparedCommand {
        mut cmd,
        #[cfg(unix)]
        _secret_fd_guard,
    } = build_runtime_command(
        &deno_bin,
        plan,
        authoritative_lock,
        execution_plan,
        &launch_spec.working_dir,
        &launch_spec.command,
        &launch_spec.args,
        launch_ctx,
        use_compat_flag,
        dangerously_skip_permissions,
    )?;
    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to execute deno run for node compat")?;

    Ok(status.code().unwrap_or(1))
}

pub fn spawn(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    dangerously_skip_permissions: bool,
) -> Result<Child> {
    verify_execution_plan_hashes(execution_plan)?;

    let launch_spec = derive_launch_spec(plan)?;

    // Package manager entrypoints (npm, yarn, pnpm, bun) run scripts directly — no lockfile needed.
    if is_package_manager_entrypoint(&launch_spec.command) {
        let PreparedCommand {
            mut cmd,
            #[cfg(unix)]
            _secret_fd_guard,
        } = build_package_manager_command(
            plan,
            authoritative_lock,
            execution_plan,
            &launch_spec.working_dir,
            &launch_spec.command,
            &launch_spec.args,
            launch_ctx,
        )?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        return cmd
            .spawn()
            .context("Failed to spawn package manager for node compat orchestration");
    }

    let Some(_) = launch_spec.required_lockfile.as_ref() else {
        return Err(AtoExecutionError::lock_incomplete(
            "source/node Tier1 execution requires a Node lockfile",
            Some("package-lock.json|yarn.lock|pnpm-lock.yaml|bun.lock|bun.lockb"),
        )
        .into());
    };

    if let Some(PreparedCommand {
        mut cmd,
        #[cfg(unix)]
        _secret_fd_guard,
    }) = maybe_build_direct_node_command(
        plan,
        authoritative_lock,
        execution_plan,
        &launch_spec.working_dir,
        &launch_spec.command,
        &launch_spec.args,
        launch_ctx,
    )? {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        return cmd
            .spawn()
            .context("Failed to spawn direct node runtime for orchestration");
    }

    let deno_bin = runtime_manager::ensure_deno_binary_with_authority(plan, authoritative_lock)?;

    let use_compat_flag = deno_supports_compat_flag(&deno_bin)?;

    run_provisioning(
        &deno_bin,
        &launch_spec.working_dir,
        &launch_spec.command,
        launch_ctx,
    )?;
    let PreparedCommand {
        mut cmd,
        #[cfg(unix)]
        _secret_fd_guard,
    } = build_runtime_command(
        &deno_bin,
        plan,
        authoritative_lock,
        execution_plan,
        &launch_spec.working_dir,
        &launch_spec.command,
        &launch_spec.args,
        launch_ctx,
        use_compat_flag,
        dangerously_skip_permissions,
    )?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn()
        .context("Failed to spawn node compat runtime for orchestration")
}

/// Spawn a NodeCompat process for background (daemon) execution.
/// Returns a `CapsuleProcess` with port-based readiness detection.
pub fn spawn_background(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    dangerously_skip_permissions: bool,
) -> Result<super::source::CapsuleProcess> {
    verify_execution_plan_hashes(execution_plan)?;

    let launch_spec = derive_launch_spec(plan)?;
    let readiness_port = runtime_overrides::override_port(launch_spec.port);

    let mut cmd = if is_package_manager_entrypoint(&launch_spec.command) {
        let PreparedCommand {
            cmd,
            #[cfg(unix)]
            _secret_fd_guard,
        } = build_package_manager_command(
            plan,
            authoritative_lock,
            execution_plan,
            &launch_spec.working_dir,
            &launch_spec.command,
            &launch_spec.args,
            launch_ctx,
        )?;
        cmd
    } else {
        let Some(_) = launch_spec.required_lockfile.as_ref() else {
            return Err(AtoExecutionError::lock_incomplete(
                "source/node Tier1 execution requires a Node lockfile",
                Some("package-lock.json|yarn.lock|pnpm-lock.yaml|bun.lock|bun.lockb"),
            )
            .into());
        };

        if let Some(PreparedCommand {
            cmd,
            #[cfg(unix)]
            _secret_fd_guard,
        }) = maybe_build_direct_node_command(
            plan,
            authoritative_lock,
            execution_plan,
            &launch_spec.working_dir,
            &launch_spec.command,
            &launch_spec.args,
            launch_ctx,
        )? {
            cmd
        } else {
            let deno_bin =
                runtime_manager::ensure_deno_binary_with_authority(plan, authoritative_lock)?;
            let use_compat_flag = deno_supports_compat_flag(&deno_bin)?;
            run_provisioning(
                &deno_bin,
                &launch_spec.working_dir,
                &launch_spec.command,
                launch_ctx,
            )?;
            let PreparedCommand {
                cmd,
                #[cfg(unix)]
                _secret_fd_guard,
            } = build_runtime_command(
                &deno_bin,
                plan,
                authoritative_lock,
                execution_plan,
                &launch_spec.working_dir,
                &launch_spec.command,
                &launch_spec.args,
                launch_ctx,
                use_compat_flag,
                dangerously_skip_permissions,
            )?;
            cmd
        }
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd
        .spawn()
        .context("Failed to spawn node compat process in background")?;
    let event_rx = super::source::spawn_host_lifecycle_events(child.id(), readiness_port);

    Ok(super::source::CapsuleProcess {
        child,
        cleanup_paths: Vec::new(),
        event_rx: Some(event_rx),
        workload_pid: None,
        log_path: None,
    })
}

fn run_provisioning(
    deno_bin: &Path,
    runtime_dir: &Path,
    entrypoint: &str,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<()> {
    let mut cmd = Command::new(deno_bin);
    cmd.current_dir(runtime_dir)
        .arg("cache")
        .arg("--node-modules-dir")
        .arg(entrypoint)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    launch_ctx.apply_allowlisted_env(&mut cmd)?;
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    let status = cmd
        .status()
        .context("Failed to execute deno cache for node compat")?;
    if status.success() {
        Ok(())
    } else {
        Err(AtoExecutionError::lock_incomplete(
            format!(
                "deno cache for source/node Tier1 failed with exit code {}",
                status.code().unwrap_or(1)
            ),
            Some("package-lock.json|yarn.lock|pnpm-lock.yaml|bun.lock|bun.lockb"),
        )
        .into())
    }
}

fn maybe_build_direct_node_command(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    runtime_dir: &Path,
    entrypoint: &str,
    explicit_script_args: &[String],
    launch_ctx: &RuntimeLaunchContext,
) -> Result<Option<PreparedCommand>> {
    if let Some(package_bin) = entrypoint.strip_prefix("npm:") {
        return build_host_node_package_command(
            plan,
            authoritative_lock,
            execution_plan,
            runtime_dir,
            package_bin,
            explicit_script_args,
            launch_ctx,
        )
        .map(Some);
    }

    if is_provider_backed_node_workspace(runtime_dir) {
        return build_host_node_entrypoint_command(
            plan,
            authoritative_lock,
            execution_plan,
            runtime_dir,
            entrypoint,
            explicit_script_args,
            launch_ctx,
        )
        .map(Some);
    }

    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn build_runtime_command(
    deno_bin: &Path,
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    runtime_dir: &Path,
    entrypoint: &str,
    explicit_script_args: &[String],
    launch_ctx: &RuntimeLaunchContext,
    use_compat_flag: bool,
    dangerously_skip_permissions: bool,
) -> Result<PreparedCommand> {
    if let Some(package_bin) = entrypoint.strip_prefix("npm:") {
        return build_host_node_package_command(
            plan,
            authoritative_lock,
            execution_plan,
            runtime_dir,
            package_bin,
            explicit_script_args,
            launch_ctx,
        );
    }

    let mut cmd = Command::new(deno_bin);
    cmd.current_dir(runtime_dir)
        .arg("run")
        .arg("--node-modules-dir")
        .arg("--no-prompt");
    if !dangerously_skip_permissions {
        cmd.arg("--cached-only");
    }
    if use_compat_flag {
        cmd.arg("--compat");
    }

    let runtime_dir_allow = runtime_dir.to_string_lossy().to_string();

    if dangerously_skip_permissions {
        cmd.arg("-A");
    } else {
        cmd.arg("--allow-env");
        cmd.arg("--allow-sys");

        if !execution_plan.runtime.policy.network.allow_hosts.is_empty() {
            cmd.arg(format!(
                "--allow-net={}",
                execution_plan.runtime.policy.network.allow_hosts.join(",")
            ));
        }

        let mut allow_read = execution_plan.runtime.policy.filesystem.read_only.clone();
        allow_read.extend(execution_plan.runtime.policy.filesystem.read_write.clone());
        if !allow_read.iter().any(|path| path == &runtime_dir_allow) {
            allow_read.push(runtime_dir_allow.clone());
        }
        if !allow_read.is_empty() {
            cmd.arg(format!("--allow-read={}", allow_read.join(",")));
        }

        let mut allow_write = execution_plan.runtime.policy.filesystem.read_write.clone();
        if execution_plan.target.runtime == ExecutionRuntime::Web
            && !allow_write.iter().any(|path| path == &runtime_dir_allow)
        {
            allow_write.push(runtime_dir_allow.clone());
        }
        if !allow_write.is_empty() {
            cmd.arg(format!("--allow-write={}", allow_write.join(",")));
        }
    }

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if execution_plan.target.runtime == ExecutionRuntime::Web {
        if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
            cmd.env("PORT", port.to_string());
        }
        if !dangerously_skip_permissions {
            cmd.arg("--allow-env");
            cmd.arg("--allow-sys");
            cmd.arg(format!("--allow-ffi={runtime_dir_allow}"));
        }
    } else if !dangerously_skip_permissions {
        cmd.arg(format!("--allow-ffi={runtime_dir_allow}"));
    }

    #[cfg(unix)]
    let mut secret_fd_guard: Option<std::fs::File> = None;

    #[cfg(unix)]
    {
        let secrets = collect_runtime_secrets(execution_plan);
        if !secrets.is_empty() {
            secret_fd_guard = Some(inject_secrets_via_fd3(&mut cmd, &secrets)?);
        }
    }

    append_allow_env_permission(&mut cmd, plan, launch_ctx);
    launch_ctx.apply_allowlisted_env(&mut cmd)?;

    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    cmd.arg(entrypoint);
    let args = direct_node_args(plan, execution_plan, explicit_script_args, launch_ctx);
    if !args.is_empty() {
        cmd.args(args);
    }

    Ok(PreparedCommand {
        cmd,
        #[cfg(unix)]
        _secret_fd_guard: secret_fd_guard,
    })
}

fn build_host_node_package_command(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    runtime_dir: &Path,
    package_bin: &str,
    explicit_script_args: &[String],
    launch_ctx: &RuntimeLaunchContext,
) -> Result<PreparedCommand> {
    let node_bin = runtime_manager::ensure_node_binary_with_authority(plan, authoritative_lock)?;
    let bin_path = resolve_node_package_bin(runtime_dir, package_bin)?;
    let command_cwd = launch_ctx
        .effective_cwd()
        .map_or(runtime_dir, |value| value);

    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(command_cwd).arg(&bin_path);

    prepend_managed_node_to_path(&mut cmd, &node_bin);

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        cmd.env("PORT", port.to_string());
    }

    launch_ctx.apply_allowlisted_env(&mut cmd)?;
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    let args = direct_node_args(plan, execution_plan, explicit_script_args, launch_ctx);
    if !args.is_empty() {
        cmd.args(args);
    }

    Ok(PreparedCommand {
        cmd,
        #[cfg(unix)]
        _secret_fd_guard: None,
    })
}

fn build_host_node_entrypoint_command(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    runtime_dir: &Path,
    entrypoint: &str,
    explicit_script_args: &[String],
    launch_ctx: &RuntimeLaunchContext,
) -> Result<PreparedCommand> {
    provider_workspace::ensure_provider_node_execution_inputs(plan, authoritative_lock)?;

    let node_bin = runtime_manager::ensure_node_binary_with_authority(plan, authoritative_lock)?;
    let command_cwd = launch_ctx
        .effective_cwd()
        .map_or(runtime_dir, |value| value);
    let entrypoint_path = if Path::new(entrypoint).is_absolute() {
        Path::new(entrypoint).to_path_buf()
    } else {
        runtime_dir.join(entrypoint)
    };

    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(command_cwd).arg(entrypoint_path);

    prepend_managed_node_to_path(&mut cmd, &node_bin);

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        cmd.env("PORT", port.to_string());
    }

    launch_ctx.apply_allowlisted_env(&mut cmd)?;
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    let args = direct_node_args(plan, execution_plan, explicit_script_args, launch_ctx);
    if !args.is_empty() {
        cmd.args(args);
    }

    Ok(PreparedCommand {
        cmd,
        #[cfg(unix)]
        _secret_fd_guard: None,
    })
}

/// Prepend the managed Node binary's directory to the child process `PATH`.
///
/// Fix for #294 (v0.5 pinpoint): without this, scripts spawned by the managed
/// Node (e.g. `execSync('node ...')`, `child_process.spawn('npm', ...)`) pick up
/// the host `node`/`npm` if one is installed, causing ato-managed runs to leak
/// to host tooling. We prepend unconditionally so the managed bin dir wins.
///
/// This helper intentionally does not abstract `ManagedRuntimePath`; that
/// abstraction lands in v0.5.x minor 1 (see RFC UNIFIED_EXECUTION_MODEL §4.2).
fn prepend_managed_node_to_path(cmd: &mut Command, node_bin: &Path) {
    if let Some(node_dir) = node_bin.parent() {
        let current_path = std::env::var("PATH").unwrap_or_default();
        #[cfg(windows)]
        let separator = ";";
        #[cfg(not(windows))]
        let separator = ":";
        cmd.env(
            "PATH",
            format!("{}{}{}", node_dir.display(), separator, current_path),
        );
    }
}

fn is_package_manager_entrypoint(entrypoint: &str) -> bool {
    matches!(entrypoint, "npm" | "npx" | "yarn" | "pnpm" | "bun" | "deno")
}

fn resolve_package_manager_bin(node_bin: &Path, pm: &str) -> std::path::PathBuf {
    // npm, npx, etc. are bundled alongside node — try sibling first.
    if let Some(dir) = node_bin.parent() {
        let candidate = dir.join(pm);
        if candidate.exists() {
            return candidate;
        }
    }
    std::path::PathBuf::from(pm)
}

#[allow(clippy::too_many_arguments)]
fn build_package_manager_command(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    runtime_dir: &Path,
    entrypoint: &str,
    explicit_script_args: &[String],
    launch_ctx: &RuntimeLaunchContext,
) -> Result<PreparedCommand> {
    let node_bin = runtime_manager::ensure_node_binary_with_authority(plan, authoritative_lock)?;
    let pm_bin = resolve_package_manager_bin(&node_bin, entrypoint);

    let mut cmd = Command::new(&pm_bin);
    // Package managers must run in the project root (manifest_dir), not the caller's CWD.
    cmd.current_dir(runtime_dir);

    // Ensure the managed node binary directory is on PATH so npm can invoke node.
    prepend_managed_node_to_path(&mut cmd, &node_bin);

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        cmd.env("PORT", port.to_string());
    }

    launch_ctx.apply_allowlisted_env(&mut cmd)?;
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    let args = direct_node_args(plan, execution_plan, explicit_script_args, launch_ctx);
    if !args.is_empty() {
        cmd.args(args);
    }

    Ok(PreparedCommand {
        cmd,
        #[cfg(unix)]
        _secret_fd_guard: None,
    })
}

fn is_provider_backed_node_workspace(runtime_dir: &Path) -> bool {
    provider_workspace::is_provider_workspace(runtime_dir)
}

fn provider_resolution_metadata_path(runtime_dir: &Path) -> Option<std::path::PathBuf> {
    provider_workspace::provider_resolution_metadata_path(runtime_dir)
}

fn direct_node_args(
    plan: &ManifestData,
    execution_plan: &ExecutionPlan,
    explicit_script_args: &[String],
    launch_ctx: &RuntimeLaunchContext,
) -> Vec<String> {
    if !execution_plan.runtime.policy.args.is_empty() {
        execution_plan.runtime.policy.args.clone()
    } else if explicit_script_args.is_empty() {
        let mut args = plan.targets_oci_cmd();
        args.extend(launch_ctx.command_args().iter().cloned());
        args
    } else {
        let mut args = explicit_script_args.to_vec();
        args.extend(launch_ctx.command_args().iter().cloned());
        args
    }
}

fn resolve_node_package_bin(runtime_dir: &Path, package_bin: &str) -> Result<std::path::PathBuf> {
    let candidates = [
        runtime_dir
            .join("node_modules")
            .join(".bin")
            .join(package_bin),
        runtime_dir
            .join("node_modules")
            .join(".bin")
            .join(format!("{package_bin}.cmd")),
    ];

    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            AtoExecutionError::lock_incomplete(
                format!(
                    "node package binary '{}' was not materialized under node_modules/.bin",
                    package_bin
                ),
                Some("node_modules/.bin"),
            )
            .into()
        })
}

fn deno_supports_compat_flag(deno_bin: &Path) -> Result<bool> {
    let output = Command::new(deno_bin)
        .arg("run")
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .context("Failed to inspect deno run --help for compat support")?;
    if !output.status.success() {
        return Err(AtoExecutionError::policy_violation(
            "unable to detect deno runtime capabilities for node compat execution",
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.contains("--compat"))
}

fn append_allow_env_permission(
    cmd: &mut Command,
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
) {
    let has_allow_env = cmd
        .get_args()
        .any(|arg| arg.to_string_lossy().starts_with("--allow-env"));
    if has_allow_env {
        return;
    }

    let mut keys = BTreeSet::new();
    keys.extend(runtime_overrides::merged_env(plan.execution_env()).into_keys());
    keys.extend(manifest_allow_env_keys(plan));
    keys.extend(launch_ctx.env_permission_keys());
    if keys.is_empty() {
        return;
    }

    cmd.arg(format!(
        "--allow-env={}",
        keys.into_iter().collect::<Vec<_>>().join(",")
    ));
}

fn manifest_allow_env_keys(plan: &ManifestData) -> Vec<String> {
    plan.manifest
        .get("isolation")
        .and_then(|value| value.get("allow_env"))
        .and_then(|value| value.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str())
                .map(|entry| entry.trim().to_string())
                .filter(|entry| !entry.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[cfg(test)]
fn map_deno_permission_error(stderr: &[u8]) -> Option<AtoExecutionError> {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_ascii_lowercase();

    if !lower.contains("notcapable") && !lower.contains("requires net access") {
        return None;
    }

    let target = extract_deno_net_target(&text);
    let message = if let Some(ref host) = target {
        format!("network policy violation: blocked egress to {}", host)
    } else {
        "network policy violation: blocked egress".to_string()
    };

    Some(AtoExecutionError::policy_violation(message))
}

#[cfg(test)]
fn map_node_compat_error(stderr: &[u8]) -> Option<AtoExecutionError> {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_ascii_lowercase();

    let unsupported = lower.contains("not implemented")
        || lower.contains("not yet implemented")
        || lower.contains("unsupported")
        || lower.contains("n-api modules are currently not supported");

    if !unsupported {
        return None;
    }

    Some(AtoExecutionError::policy_violation(
        "node compat runtime rejected an unsupported node feature (fail-closed)",
    ))
}

#[cfg(test)]
fn extract_deno_net_target(stderr: &str) -> Option<String> {
    let marker = "Requires net access to \"";
    let start = stderr.find(marker)? + marker.len();
    let tail = &stderr[start..];
    let end = tail.find('"')?;
    let host_port = &tail[..end];
    let host = host_port.split(':').next().unwrap_or(host_port).trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(unix)]
fn collect_runtime_secrets(execution_plan: &ExecutionPlan) -> BTreeMap<String, String> {
    let mut keys = BTreeSet::new();

    for key in &execution_plan.runtime.policy.secrets.allow_secret_ids {
        if !key.trim().is_empty() {
            keys.insert(key.trim().to_string());
        }
    }

    if std::env::var_os("OPENAI_API_KEY").is_some() {
        keys.insert("OPENAI_API_KEY".to_string());
    }

    let mut secrets = BTreeMap::new();
    for key in keys {
        if let Ok(value) = std::env::var(&key) {
            if !value.is_empty() {
                secrets.insert(key, value);
            }
        }
    }

    secrets
}

#[cfg(unix)]
fn inject_secrets_via_fd3(
    cmd: &mut Command,
    secrets: &BTreeMap<String, String>,
) -> Result<std::fs::File> {
    let mut fds = [0; 2];
    let pipe_result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if pipe_result != 0 {
        return Err(anyhow::anyhow!("failed to create secret pipe"));
    }

    let read_fd = fds[0];
    let write_fd = fds[1];

    let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd) };
    let payload = serde_json::to_vec(secrets)
        .context("failed to serialize secret payload for fd injection")?;
    writer
        .write_all(&payload)
        .context("failed to write secret payload into fd pipe")?;
    drop(writer);

    let reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let dup_from_fd = reader.as_raw_fd();

    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(dup_from_fd, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if dup_from_fd != 3 {
                libc::close(dup_from_fd);
            }
            Ok(())
        });
    }

    cmd.env("ATO_SECRET_FD", "3");
    for key in secrets.keys() {
        cmd.env(format!("ATO_SECRET_FD_{key}"), "3");
        cmd.env_remove(key);
    }

    Ok(reader)
}

fn verify_execution_plan_hashes(execution_plan: &ExecutionPlan) -> Result<()> {
    let expected_policy_hash = compute_policy_segment_hash(
        &execution_plan.runtime,
        &execution_plan.consent.mount_set_algo_id,
        execution_plan.consent.mount_set_algo_version,
    )?;
    if expected_policy_hash != execution_plan.consent.policy_segment_hash {
        return Err(AtoExecutionError::lockfile_tampered(
            "policy_segment_hash mismatch detected before node compat runtime",
            Some("policy_segment_hash"),
        )
        .into());
    }

    let expected_provisioning_hash =
        compute_provisioning_policy_hash(&execution_plan.provisioning)?;
    if expected_provisioning_hash != execution_plan.consent.provisioning_policy_hash {
        return Err(AtoExecutionError::lockfile_tampered(
            "provisioning_policy_hash mismatch detected before node compat runtime",
            Some("provisioning_policy_hash"),
        )
        .into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        is_provider_backed_node_workspace, map_deno_permission_error, map_node_compat_error,
        prepend_managed_node_to_path, provider_resolution_metadata_path,
    };

    use capsule_core::launch_spec::{derive_launch_spec, LaunchSpecSource};
    use capsule_core::router::{
        execution_descriptor_from_manifest_parts, ExecutionProfile, ManifestData,
    };
    use std::collections::HashMap;

    fn plan_from_manifest(tmp: &tempfile::TempDir, manifest_fragment: &str) -> ManifestData {
        let manifest_path = tmp.path().join("capsule.toml");
        let manifest = format!(
            r#"
schema_version = "0.3"
name = "app"
version = "0.1.0"
type = "app"
{manifest_fragment}
"#
        );
        std::fs::write(&manifest_path, &manifest).expect("write manifest");
        let parsed: toml::Value = toml::from_str(&manifest).expect("parse manifest");
        execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path,
            tmp.path().to_path_buf(),
            ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    #[test]
    fn node_lock_path_falls_back_to_source_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(
            tmp.path().join("source").join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'",
        )
        .expect("write source pnpm-lock");

        let plan = plan_from_manifest(
            &tmp,
            r#"
runtime = "source/node"
run = "node main.js"
"#,
        );
        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(
            spec.required_lockfile,
            Some(tmp.path().join("source").join("pnpm-lock.yaml"))
        );
    }

    #[test]
    fn provider_workspace_detection_accepts_manifest_root() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let resolution = tmp.path().join("resolution.json");
        std::fs::write(&resolution, "{}\n").expect("write provider metadata");

        assert!(is_provider_backed_node_workspace(tmp.path()));
        assert_eq!(
            provider_resolution_metadata_path(tmp.path()),
            Some(resolution.clone())
        );
    }

    #[test]
    fn provider_workspace_detection_accepts_source_subdir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let source = tmp.path().join("source");
        let resolution = tmp.path().join("resolution.json");
        std::fs::create_dir_all(&source).expect("create source directory");
        std::fs::write(&resolution, "{}\n").expect("write provider metadata");

        assert!(is_provider_backed_node_workspace(&source));
        assert_eq!(
            provider_resolution_metadata_path(&source),
            Some(resolution.clone())
        );
    }

    #[test]
    fn node_lock_path_accepts_yarn_lock_in_source_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(
            tmp.path().join("source").join("yarn.lock"),
            "# yarn lockfile v1\n",
        )
        .expect("write source yarn lock");

        let plan = plan_from_manifest(
            &tmp,
            r#"
runtime = "source/node"
run = "node main.js"
"#,
        );
        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(
            spec.required_lockfile,
            Some(tmp.path().join("source").join("yarn.lock"))
        );
    }

    #[test]
    fn run_command_spec_resolves_node_entrypoint_and_args() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(
            tmp.path().join("source").join("lib.js"),
            "console.log('ok');",
        )
        .expect("write source script");

        let plan = plan_from_manifest(
            &tmp,
            r#"
runtime = "source/node"
run = "node lib.js fixtures/db.json --port 3000"
"#,
        );
        let spec = derive_launch_spec(&plan).expect("derive launch spec");

        assert_eq!(spec.working_dir, tmp.path().join("source"));
        assert_eq!(spec.command, "lib.js");
        assert_eq!(spec.args, vec!["fixtures/db.json", "--port", "3000"]);
        assert_eq!(spec.source, LaunchSpecSource::RunCommand);
    }

    #[test]
    fn map_permission_error_returns_policy_violation() {
        let stderr = b"error: Uncaught (in promise) PermissionDenied: Requires net access to \"api.example.com:443\"";
        let err = map_deno_permission_error(stderr).expect("must map");
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
        assert!(err.message.contains("blocked egress"));
    }

    #[test]
    fn map_node_compat_error_returns_policy_violation() {
        let stderr = b"error: This API is not implemented in Deno";
        let err = map_node_compat_error(stderr).expect("must map");
        assert_eq!(err.code, "ATO_ERR_POLICY_VIOLATION");
    }

    /// #294 pinpoint regression test: the managed Node `bin` directory must be
    /// prepended to `PATH` before the command is spawned, so child processes
    /// (`execSync('node ...')`, `spawn('npm', ...)`) resolve to the managed
    /// binary rather than any host-installed version.
    #[test]
    fn prepend_managed_node_to_path_places_managed_bin_first() {
        let tmp = tempfile::tempdir().expect("tmp");
        let node_bin = tmp.path().join("bin").join("node");
        std::fs::create_dir_all(node_bin.parent().unwrap()).expect("create bin dir");
        // No need to touch the file — prepend_managed_node_to_path just uses the
        // parent directory; it does not check the binary exists.

        // Force a deterministic baseline PATH for this test.
        let original_path = std::env::var_os("PATH");
        // SAFETY: tests are currently single-threaded per integration runner for
        // this crate; if that ever changes we'll need a test fixture instead of
        // mutating process env. For a pinpoint regression this is acceptable.
        unsafe {
            std::env::set_var("PATH", "/usr/bin:/bin");
        }

        let mut cmd = std::process::Command::new(&node_bin);
        prepend_managed_node_to_path(&mut cmd, &node_bin);

        let applied_path = cmd
            .get_envs()
            .find_map(|(key, value)| {
                if key == "PATH" {
                    value.map(|v| v.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .expect("PATH must be set on the command");

        // restore before asserting so a panic doesn't poison env
        unsafe {
            match original_path {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
        }

        let managed_dir = node_bin.parent().unwrap().display().to_string();
        let separator = if cfg!(windows) { ";" } else { ":" };
        let expected = format!("{}{}{}", managed_dir, separator, "/usr/bin:/bin");
        assert_eq!(
            applied_path, expected,
            "managed Node bin dir must be first entry of PATH (#294)"
        );
    }
}
