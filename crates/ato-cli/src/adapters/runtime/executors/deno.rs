use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::{
    collections::BTreeMap,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
};

use anyhow::{Context, Result};
use serde_json::Value;

use capsule_core::execution_plan::canonical::{
    compute_policy_segment_hash, compute_provisioning_policy_hash,
};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::model::{ExecutionPlan, ExecutionRuntime};
use capsule_core::router::ManifestData;

use crate::application::pipeline::cleanup::PipelineAttemptContext;
use crate::common::proxy;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;

use super::launch_context::RuntimeLaunchContext;

enum DependencyLock {
    Deno(PathBuf),
    PackageJson(PathBuf),
}

struct DenoRuntimeEnvPaths {
    home: PathBuf,
    xdg_cache_home: PathBuf,
    deno_dir: PathBuf,
}

struct DenoLaunchSpec {
    runtime_dir: PathBuf,
    entrypoint: String,
    explicit_deno_flags: Vec<String>,
    explicit_script_args: Vec<String>,
}

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
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<i32> {
    verify_execution_plan_hashes(execution_plan)?;
    if plan.is_web_services_mode() && !plan.is_orchestration_mode() {
        return super::web_services::execute(plan, launch_ctx, attempt);
    }

    let deno_bin = runtime_manager::ensure_deno_binary_with_authority(plan, authoritative_lock)?;
    let launch_spec = resolve_deno_launch_spec(plan)?;
    let skip_lock = launch_spec
        .explicit_deno_flags
        .iter()
        .any(|arg| arg == "--no-lock");
    if skip_lock {
        disable_runtime_lockfile(launch_spec.runtime_dir.as_path())?;
    }
    let lock = if skip_lock {
        None
    } else {
        resolve_dependency_lock(&plan.manifest_dir, &launch_spec.runtime_dir)
    };
    if !skip_lock && lock.is_none() {
        return Err(AtoExecutionError::lock_incomplete(
            "deno.lock or package-lock.json is required for source/deno execution",
            Some("deno.lock"),
        )
        .into());
    }

    run_provisioning(
        &deno_bin,
        &launch_spec.runtime_dir,
        &launch_spec.entrypoint,
        &launch_spec.explicit_deno_flags,
        lock.as_ref(),
        launch_ctx,
    )?;
    let prepared = build_runtime_command(
        &deno_bin,
        plan,
        authoritative_lock,
        execution_plan,
        &launch_spec.runtime_dir,
        &launch_spec.entrypoint,
        &launch_spec.explicit_deno_flags,
        &launch_spec.explicit_script_args,
        lock.as_ref(),
        launch_ctx,
        dangerously_skip_permissions,
    )?;
    let (exit_code, stderr) =
        run_and_stream_child(prepared).context("Failed to execute deno run")?;

    if exit_code != 0 {
        if let Some(err) = map_deno_permission_error(&stderr) {
            return Err(err.into());
        }
    }

    Ok(exit_code)
}

pub fn spawn(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    launch_ctx: &RuntimeLaunchContext,
    dangerously_skip_permissions: bool,
) -> Result<Child> {
    verify_execution_plan_hashes(execution_plan)?;
    if plan.is_web_services_mode() && !plan.is_orchestration_mode() {
        anyhow::bail!("legacy inline web services mode is not supported by deno::spawn");
    }

    let deno_bin = runtime_manager::ensure_deno_binary_with_authority(plan, authoritative_lock)?;
    let launch_spec = resolve_deno_launch_spec(plan)?;
    let skip_lock = launch_spec
        .explicit_deno_flags
        .iter()
        .any(|arg| arg == "--no-lock");
    if skip_lock {
        disable_runtime_lockfile(launch_spec.runtime_dir.as_path())?;
    }
    let lock = if skip_lock {
        None
    } else {
        resolve_dependency_lock(&plan.manifest_dir, &launch_spec.runtime_dir)
    };
    if !skip_lock && lock.is_none() {
        return Err(AtoExecutionError::lock_incomplete(
            "deno.lock or package-lock.json is required for source/deno execution",
            Some("deno.lock"),
        )
        .into());
    }

    run_provisioning(
        &deno_bin,
        &launch_spec.runtime_dir,
        &launch_spec.entrypoint,
        &launch_spec.explicit_deno_flags,
        lock.as_ref(),
        launch_ctx,
    )?;

    let mut prepared = build_runtime_command(
        &deno_bin,
        plan,
        authoritative_lock,
        execution_plan,
        &launch_spec.runtime_dir,
        &launch_spec.entrypoint,
        &launch_spec.explicit_deno_flags,
        &launch_spec.explicit_script_args,
        lock.as_ref(),
        launch_ctx,
        dangerously_skip_permissions,
    )?;
    prepared
        .cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    prepared
        .cmd
        .spawn()
        .context("Failed to spawn deno runtime for orchestration")
}

fn run_provisioning(
    deno_bin: &Path,
    runtime_dir: &Path,
    entrypoint: &str,
    explicit_deno_flags: &[String],
    lock: Option<&DependencyLock>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<()> {
    let mut cmd = Command::new(deno_bin);
    let runtime_env_paths = ensure_deno_runtime_env_paths(runtime_dir)?;
    cmd.current_dir(runtime_dir).arg("cache");
    if explicit_deno_flags.iter().any(|arg| arg == "--no-lock") {
        cmd.arg("--no-lock");
    }
    match lock {
        Some(DependencyLock::Deno(lock_path)) => {
            cmd.arg("--lock").arg(lock_path).arg("--frozen");
        }
        Some(DependencyLock::PackageJson(_)) => {
            cmd.arg("--node-modules-dir");
        }
        None => {}
    }
    cmd.arg(entrypoint)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    apply_deno_runtime_env(&mut cmd, &runtime_env_paths);

    launch_ctx.apply_allowlisted_env(&mut cmd)?;
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    let status = cmd.status().context("Failed to execute deno cache")?;
    if status.success() {
        Ok(())
    } else {
        let message = match lock {
            Some(DependencyLock::Deno(_)) => format!(
                "deno cache --lock --frozen failed with exit code {}",
                status.code().unwrap_or(1)
            ),
            Some(DependencyLock::PackageJson(lock_path)) => format!(
                "deno cache with package-lock.json fallback failed ({}): exit code {}",
                lock_path.display(),
                status.code().unwrap_or(1)
            ),
            None => format!(
                "deno cache failed with exit code {}",
                status.code().unwrap_or(1)
            ),
        };
        Err(AtoExecutionError::lock_incomplete(message, Some("deno.lock")).into())
    }
}

#[allow(clippy::too_many_arguments)]
fn build_runtime_command(
    deno_bin: &Path,
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    execution_plan: &ExecutionPlan,
    runtime_dir: &Path,
    entrypoint: &str,
    explicit_deno_flags: &[String],
    explicit_script_args: &[String],
    lock: Option<&DependencyLock>,
    launch_ctx: &RuntimeLaunchContext,
    dangerously_skip_permissions: bool,
) -> Result<PreparedCommand> {
    let mut cmd = Command::new(deno_bin);
    let runtime_env_paths = ensure_deno_runtime_env_paths(runtime_dir)?;
    let execution_env = runtime_overrides::merged_env(plan.execution_env());
    cmd.current_dir(runtime_dir).arg("run").arg("--no-prompt");
    for arg in explicit_deno_flags {
        if arg == "-A" || arg == "--allow-all" || arg == "run" {
            continue;
        }
        cmd.arg(arg);
    }
    if !dangerously_skip_permissions {
        cmd.arg("--cached-only");
    }

    match lock {
        Some(DependencyLock::Deno(lock_path)) => {
            cmd.arg("--lock").arg(lock_path).arg("--frozen");
        }
        Some(DependencyLock::PackageJson(_)) => {
            cmd.arg("--node-modules-dir");
        }
        None => {}
    }

    if dangerously_skip_permissions {
        cmd.arg("-A");
    } else {
        let is_web_runtime = execution_plan.target.runtime == ExecutionRuntime::Web;
        let is_fresh_runtime = runtime_dir.join("fresh.gen.ts").exists();

        if is_web_runtime {
            // web/deno orchestration spawns subprocesses and probes loopback ports dynamically.
            // Use broad runtime permissions to avoid false policy failures in orchestrator internals.
            cmd.arg("--allow-net")
                .arg("--allow-read")
                .arg("--allow-write")
                .arg("--allow-env")
                .arg("--allow-run")
                .arg("--allow-sys")
                .arg("--allow-ffi");
        } else {
            if !execution_plan.runtime.policy.network.allow_hosts.is_empty()
                && !has_explicit_deno_permission(explicit_deno_flags, "--allow-net")
            {
                cmd.arg(format!(
                    "--allow-net={}",
                    execution_plan.runtime.policy.network.allow_hosts.join(",")
                ));
            }

            let mut allow_read = execution_plan.runtime.policy.filesystem.read_only.clone();
            allow_read.extend(execution_plan.runtime.policy.filesystem.read_write.clone());
            let runtime_dir_str = runtime_dir.to_string_lossy().to_string();
            let runtime_home_str = runtime_env_paths.home.to_string_lossy().to_string();
            if !allow_read.iter().any(|path| path == &runtime_dir_str) {
                allow_read.push(runtime_dir_str);
            }
            if !allow_read.iter().any(|path| path == &runtime_home_str) {
                allow_read.push(runtime_home_str.clone());
            }
            if !allow_read.is_empty() {
                cmd.arg(format!("--allow-read={}", allow_read.join(",")));
            }

            let mut allow_write = execution_plan.runtime.policy.filesystem.read_write.clone();
            if !allow_write.iter().any(|path| path == &runtime_home_str) {
                allow_write.push(runtime_home_str);
            }
            if !allow_write.is_empty() {
                cmd.arg(format!("--allow-write={}", allow_write.join(",")));
            }

            if is_fresh_runtime {
                cmd.arg("--allow-run");
            }
        }
    }

    for (key, value) in execution_env {
        cmd.env(key, value);
    }
    apply_deno_runtime_env(&mut cmd, &runtime_env_paths);
    cmd.env("ATO_RUNTIME_DENO_BIN", deno_bin);
    if execution_plan.target.runtime == ExecutionRuntime::Web {
        let web_host = launch_ctx
            .merged_env()
            .get("ATO_WEB_HOST")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("ATO_WEB_HOST")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| "127.0.0.1".to_string());
        cmd.env("ATO_WEB_HOST", &web_host);
        cmd.env("HOST", &web_host);

        let needs_node_runtime = has_runtime_tool(plan, &["node", "nodejs"]);
        let needs_python_runtime = has_runtime_tool(plan, &["python", "python3"]);
        let needs_uv_runtime = has_runtime_tool(plan, &["uv"]) || needs_python_runtime;

        if needs_node_runtime {
            let node_bin =
                runtime_manager::ensure_node_binary_with_authority(plan, authoritative_lock)?;
            cmd.env("ATO_RUNTIME_NODE_BIN", node_bin);
        }
        if needs_python_runtime {
            let python_bin =
                runtime_manager::ensure_python_binary_with_authority(plan, authoritative_lock)?;
            cmd.env("ATO_RUNTIME_PYTHON_BIN", python_bin);
        }
        if needs_uv_runtime {
            let uv_bin = runtime_manager::ensure_uv_binary(plan)?;
            cmd.env("ATO_RUNTIME_UV_BIN", uv_bin);
        }
        if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
            cmd.env("PORT", port.to_string());
            cmd.env("ATO_WEB_PORT", port.to_string());
            cmd.env("ATO_WEB_ORIGIN", format!("http://{}:{}", web_host, port));
        }
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
    let args = if plan.execution_run_command().is_some()
        || selected_target_cmd(plan)
            .first()
            .map(|arg| arg == "deno")
            .unwrap_or(false)
    {
        explicit_script_args.to_vec()
    } else {
        plan.targets_oci_cmd()
    };
    if !args.is_empty() {
        cmd.args(args);
    }

    Ok(PreparedCommand {
        cmd,
        #[cfg(unix)]
        _secret_fd_guard: secret_fd_guard,
    })
}

fn has_explicit_deno_permission(flags: &[String], permission: &str) -> bool {
    flags.iter().any(|flag| {
        flag == permission
            || flag.starts_with(&format!("{permission}="))
            || flag == "-A"
            || flag == "--allow-all"
    })
}

fn resolve_deno_launch_spec(plan: &ManifestData) -> Result<DenoLaunchSpec> {
    if let Some(entrypoint) = plan.execution_entrypoint().filter(|v| !v.trim().is_empty()) {
        let runtime_dir = resolve_deno_runtime_dir(&plan.manifest_dir, &entrypoint);
        let (explicit_deno_flags, explicit_script_args) =
            selected_deno_cmd_parts(plan, &entrypoint);
        return Ok(DenoLaunchSpec {
            runtime_dir,
            entrypoint,
            explicit_deno_flags,
            explicit_script_args,
        });
    }

    let run_command = plan
        .execution_run_command()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AtoExecutionError::policy_violation(
                "source/deno target requires entrypoint or run_command",
            )
        })?;

    resolve_deno_launch_spec_from_run_command(&plan.manifest_dir, &run_command)
}

fn resolve_deno_launch_spec_from_run_command(
    manifest_dir: &Path,
    run_command: &str,
) -> Result<DenoLaunchSpec> {
    let tokens = shell_words::split(run_command).unwrap_or_else(|_| vec![run_command.to_string()]);
    let Some(first) = tokens.first() else {
        return Err(anyhow::anyhow!("source/deno run_command is empty"));
    };
    if first != "deno" {
        return Err(anyhow::anyhow!(
            "source/deno run_command must start with 'deno', got '{}'",
            first
        ));
    }

    if tokens.get(1).map(String::as_str) == Some("task") {
        let task_name = tokens.get(2).map(String::as_str).ok_or_else(|| {
            anyhow::anyhow!("source/deno run_command 'deno task' requires a task name")
        })?;
        let task_command = read_deno_task_command(manifest_dir, task_name)?;
        return resolve_deno_launch_spec_from_run_command(manifest_dir, &task_command);
    }

    parse_deno_run_tokens(manifest_dir, &tokens)
}

fn parse_deno_run_tokens(manifest_dir: &Path, tokens: &[String]) -> Result<DenoLaunchSpec> {
    let mut iter = tokens.iter().skip(1).peekable();
    if matches!(iter.peek().map(|value| value.as_str()), Some("run")) {
        let _ = iter.next();
    }

    let mut explicit_deno_flags = Vec::new();
    let mut entrypoint = None;
    let mut explicit_script_args = Vec::new();

    for arg in iter {
        if entrypoint.is_none() {
            if arg.starts_with('-') {
                explicit_deno_flags.push(arg.to_string());
                continue;
            }
            entrypoint = Some(arg.to_string());
            continue;
        }

        explicit_script_args.push(arg.to_string());
    }

    let entrypoint = entrypoint.ok_or_else(|| {
        anyhow::anyhow!("source/deno run_command must include a script entrypoint")
    })?;
    let runtime_dir = resolve_deno_runtime_dir(manifest_dir, &entrypoint);

    Ok(DenoLaunchSpec {
        runtime_dir,
        entrypoint,
        explicit_deno_flags,
        explicit_script_args,
    })
}

fn read_deno_task_command(manifest_dir: &Path, task_name: &str) -> Result<String> {
    let deno_json_path = [
        manifest_dir.join("deno.json"),
        manifest_dir.join("source").join("deno.json"),
    ]
    .into_iter()
    .find(|path| path.exists())
    .ok_or_else(|| {
        anyhow::anyhow!(
            "source/deno task '{}' requires deno.json in {} or {}/source",
            task_name,
            manifest_dir.display(),
            manifest_dir.display()
        )
    })?;
    let raw = std::fs::read_to_string(&deno_json_path).with_context(|| {
        format!(
            "Failed to read {} for source/deno task resolution",
            deno_json_path.display()
        )
    })?;
    let parsed: Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse {} for source/deno task resolution",
            deno_json_path.display()
        )
    })?;
    let command = parsed
        .get("tasks")
        .and_then(|tasks| tasks.get(task_name))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "source/deno task '{}' was not found in {}",
                task_name,
                deno_json_path.display()
            )
        })?;

    Ok(command.to_string())
}

fn ensure_deno_runtime_env_paths(runtime_dir: &Path) -> Result<DenoRuntimeEnvPaths> {
    let home = runtime_dir.join(".ato-home");
    let xdg_cache_home = home.join(".cache");
    let deno_dir = home.join(".deno");
    let macos_cache_root = home.join("Library").join("Caches");

    for dir in [&home, &xdg_cache_home, &deno_dir, &macos_cache_root] {
        std::fs::create_dir_all(dir).with_context(|| {
            format!(
                "Failed to create Deno runtime cache directory: {}",
                dir.display()
            )
        })?;
    }

    Ok(DenoRuntimeEnvPaths {
        home,
        xdg_cache_home,
        deno_dir,
    })
}

fn apply_deno_runtime_env(cmd: &mut Command, env_paths: &DenoRuntimeEnvPaths) {
    cmd.env("HOME", &env_paths.home);
    cmd.env("XDG_CACHE_HOME", &env_paths.xdg_cache_home);
    cmd.env("DENO_DIR", &env_paths.deno_dir);
}

fn has_runtime_tool(plan: &ManifestData, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        plan.execution_runtime_tool_version(key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    })
}

fn run_and_stream_child(mut prepared: PreparedCommand) -> Result<(i32, Vec<u8>)> {
    let mut child = prepared
        .cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = std::thread::spawn(move || -> std::io::Result<()> {
        let Some(mut reader) = stdout else {
            return Ok(());
        };
        let mut writer = std::io::stdout();
        let mut buf = [0u8; 8192];
        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buf[..read])?;
            writer.flush()?;
        }
        Ok(())
    });

    let stderr_handle = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let Some(mut reader) = stderr else {
            return Ok(Vec::new());
        };
        let mut writer = std::io::stderr();
        let mut captured = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buf[..read])?;
            writer.flush()?;
            captured.extend_from_slice(&buf[..read]);
        }
        Ok(captured)
    });

    let status = child.wait()?;
    let stdout_result = stdout_handle
        .join()
        .map_err(|_| anyhow::anyhow!("stdout streaming thread panicked"))?;
    stdout_result?;
    let stderr_result = stderr_handle
        .join()
        .map_err(|_| anyhow::anyhow!("stderr streaming thread panicked"))?;
    let stderr = stderr_result?;

    let exit_code = status.code().unwrap_or(1);
    Ok((exit_code, stderr))
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
    keys.extend(default_deno_env_permission_keys());
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

fn default_deno_env_permission_keys() -> Vec<String> {
    ["CI", "TERM", "NO_COLOR", "FORCE_COLOR"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn resolve_deno_runtime_dir(manifest_dir: &Path, entrypoint: &str) -> PathBuf {
    let source_dir = manifest_dir.join("source");
    if source_dir.is_dir() && source_dir.join(entrypoint).exists() {
        return source_dir;
    }
    manifest_dir.to_path_buf()
}

fn resolve_deno_lock_path(manifest_dir: &Path, runtime_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        runtime_dir.join("deno.lock"),
        manifest_dir.join("deno.lock"),
        manifest_dir.join("source").join("deno.lock"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_package_lock_path(manifest_dir: &Path, runtime_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        runtime_dir.join("package-lock.json"),
        manifest_dir.join("package-lock.json"),
        manifest_dir.join("source").join("package-lock.json"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn disable_runtime_lockfile(runtime_dir: &Path) -> Result<()> {
    let lock_path = runtime_dir.join("deno.lock");
    let disabled_path = runtime_dir.join(".ato-deno.lock.disabled");

    if !lock_path.exists() || disabled_path.exists() {
        return Ok(());
    }

    std::fs::rename(&lock_path, &disabled_path).with_context(|| {
        format!(
            "Failed to disable runtime deno.lock for --no-lock execution: {}",
            lock_path.display()
        )
    })?;

    Ok(())
}

fn selected_target_cmd(plan: &ManifestData) -> Vec<String> {
    plan.manifest
        .get("targets")
        .and_then(|targets| targets.get(&plan.selected_target))
        .and_then(|target| target.get("cmd"))
        .and_then(|cmd| cmd.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(|value| value.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn selected_deno_cmd_parts(plan: &ManifestData, entrypoint: &str) -> (Vec<String>, Vec<String>) {
    let cmd = selected_target_cmd(plan);
    if cmd.first().map(|arg| arg != "deno").unwrap_or(true) {
        return (Vec::new(), Vec::new());
    }

    let mut iter = cmd.into_iter().skip(1).peekable();
    if matches!(iter.peek().map(String::as_str), Some("run")) {
        let _ = iter.next();
    }

    let mut flags = Vec::new();
    let mut script_args = Vec::new();
    let mut reached_entrypoint = false;

    for arg in iter {
        if !reached_entrypoint {
            if arg == entrypoint {
                reached_entrypoint = true;
                continue;
            }
            flags.push(arg);
        } else {
            script_args.push(arg);
        }
    }

    (flags, script_args)
}

fn resolve_dependency_lock(manifest_dir: &Path, runtime_dir: &Path) -> Option<DependencyLock> {
    if let Some(lock_path) = resolve_deno_lock_path(manifest_dir, runtime_dir) {
        return Some(DependencyLock::Deno(lock_path));
    }

    resolve_package_lock_path(manifest_dir, runtime_dir).map(DependencyLock::PackageJson)
}

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

    Some(AtoExecutionError::security_policy_violation(
        message,
        Some("network"),
        target.as_deref(),
    ))
}

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
            "policy_segment_hash mismatch detected before deno runtime",
            Some("policy_segment_hash"),
        )
        .into());
    }

    let expected_provisioning_hash =
        compute_provisioning_policy_hash(&execution_plan.provisioning)?;
    if expected_provisioning_hash != execution_plan.consent.provisioning_policy_hash {
        return Err(AtoExecutionError::lockfile_tampered(
            "provisioning_policy_hash mismatch detected before deno runtime",
            Some("provisioning_policy_hash"),
        )
        .into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        default_deno_env_permission_keys, disable_runtime_lockfile, ensure_deno_runtime_env_paths,
        resolve_deno_launch_spec_from_run_command, resolve_deno_lock_path,
        resolve_deno_runtime_dir, resolve_package_lock_path,
    };

    #[test]
    fn default_deno_env_permission_keys_include_color_detection_vars() {
        let keys = default_deno_env_permission_keys();

        assert!(keys.contains(&"CI".to_string()));
        assert!(keys.contains(&"TERM".to_string()));
        assert!(keys.contains(&"NO_COLOR".to_string()));
        assert!(keys.contains(&"FORCE_COLOR".to_string()));
    }

    #[test]
    fn deno_runtime_dir_uses_source_when_entrypoint_exists_only_there() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(
            tmp.path().join("source").join("main.ts"),
            "console.log('ok');",
        )
        .expect("write source entrypoint");

        let runtime_dir = resolve_deno_runtime_dir(tmp.path(), "main.ts");
        assert_eq!(runtime_dir, tmp.path().join("source"));
    }

    #[test]
    fn deno_lock_path_falls_back_to_source_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(tmp.path().join("source").join("deno.lock"), "{}")
            .expect("write source deno lock");

        let runtime_dir = resolve_deno_runtime_dir(tmp.path(), "main.ts");
        let lock_path =
            resolve_deno_lock_path(tmp.path(), &runtime_dir).expect("must resolve deno.lock");
        assert_eq!(lock_path, tmp.path().join("source").join("deno.lock"));
    }

    #[test]
    fn package_lock_path_falls_back_to_source_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(tmp.path().join("source").join("package-lock.json"), "{}")
            .expect("write source package-lock");

        let runtime_dir = resolve_deno_runtime_dir(tmp.path(), "main.ts");
        let lock_path = resolve_package_lock_path(tmp.path(), &runtime_dir)
            .expect("must resolve package-lock.json");
        assert_eq!(
            lock_path,
            tmp.path().join("source").join("package-lock.json")
        );
    }

    #[test]
    fn deno_runtime_env_paths_stay_within_runtime_dir() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let runtime_dir = tmp.path().join("source");
        std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");

        let paths = ensure_deno_runtime_env_paths(&runtime_dir).expect("build runtime env paths");

        assert_eq!(paths.home, runtime_dir.join(".ato-home"));
        assert_eq!(
            paths.xdg_cache_home,
            runtime_dir.join(".ato-home").join(".cache")
        );
        assert_eq!(paths.deno_dir, runtime_dir.join(".ato-home").join(".deno"));
        assert!(paths.home.exists());
        assert!(paths.xdg_cache_home.exists());
        assert!(paths.deno_dir.exists());
        assert!(runtime_dir
            .join(".ato-home")
            .join("Library")
            .join("Caches")
            .exists());
    }

    #[test]
    fn run_command_spec_resolves_deno_task_entrypoint() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::write(
            tmp.path().join("deno.json"),
            r#"{
  "tasks": {
    "start": "deno run --allow-net main.ts"
  }
}"#,
        )
        .expect("write deno.json");
        std::fs::write(tmp.path().join("main.ts"), "console.log('ok');").expect("write main.ts");

        let spec = resolve_deno_launch_spec_from_run_command(tmp.path(), "deno task start")
            .expect("resolve deno task start");

        assert_eq!(spec.runtime_dir, tmp.path());
        assert_eq!(spec.entrypoint, "main.ts");
        assert_eq!(spec.explicit_deno_flags, vec!["--allow-net".to_string()]);
        assert!(spec.explicit_script_args.is_empty());
    }

    #[test]
    fn run_command_spec_resolves_deno_task_from_source_dir_config() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(
            tmp.path().join("source").join("deno.json"),
            r#"{
  "tasks": {
    "start": "deno run --allow-net main.ts"
  }
}"#,
        )
        .expect("write source deno.json");
        std::fs::write(
            tmp.path().join("source").join("main.ts"),
            "console.log('ok');",
        )
        .expect("write source main.ts");

        let spec = resolve_deno_launch_spec_from_run_command(tmp.path(), "deno task start")
            .expect("resolve deno task start from source/");

        assert_eq!(spec.runtime_dir, tmp.path().join("source"));
        assert_eq!(spec.entrypoint, "main.ts");
        assert_eq!(spec.explicit_deno_flags, vec!["--allow-net".to_string()]);
        assert!(spec.explicit_script_args.is_empty());
    }

    #[test]
    fn run_command_spec_resolves_direct_deno_run_entrypoint() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::write(tmp.path().join("main.ts"), "console.log('ok');").expect("write main.ts");

        let spec = resolve_deno_launch_spec_from_run_command(
            tmp.path(),
            "deno run --allow-net main.ts --port 8000",
        )
        .expect("resolve deno run command");

        assert_eq!(spec.runtime_dir, tmp.path());
        assert_eq!(spec.entrypoint, "main.ts");
        assert_eq!(spec.explicit_deno_flags, vec!["--allow-net".to_string()]);
        assert_eq!(
            spec.explicit_script_args,
            vec!["--port".to_string(), "8000".to_string()]
        );
    }

    #[test]
    fn disable_runtime_lockfile_renames_deno_lock() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let runtime_dir = tmp.path().join("source");
        std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
        let lock_path = runtime_dir.join("deno.lock");
        std::fs::write(&lock_path, "{}").expect("write deno.lock");

        disable_runtime_lockfile(&runtime_dir).expect("disable deno.lock");

        assert!(!lock_path.exists());
        assert!(runtime_dir.join(".ato-deno.lock.disabled").exists());
    }
}
