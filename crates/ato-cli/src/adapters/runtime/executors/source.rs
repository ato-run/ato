#![allow(dead_code)]

use anyhow::{Context, Result};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime};
use tracing::debug;

use capsule_core::common::paths::{ato_cache_dir, workspace_artifacts_dir};
use capsule_core::runtime::native::NativeHandle;
use capsule_core::{RuntimeMetadata, SessionRunner, SessionRunnerConfig};

use super::launch_context::RuntimeLaunchContext;
use crate::application::workspace::state::EffectiveLockState;
use crate::common::proxy;
use crate::reporters::CliReporter;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;
use crate::runtime::provider_workspace;

use capsule_core::engine;
use capsule_core::isolation::HostIsolationContext;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::lifecycle::LifecycleEvent;
use capsule_core::lock_runtime;
use capsule_core::python_runtime::{
    extend_python_selector_env, normalized_python_runtime_version, python_selector_env,
};
use capsule_core::router::ManifestData;
use capsule_core::runtime_config;

const NACELLE_MANIFEST_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NacelleManifestOwner {
    pid: u32,
    start_time_unix_ms: Option<u64>,
}

pub struct CapsuleProcess {
    pub child: Child,
    pub cleanup_paths: Vec<PathBuf>,
    pub event_rx: Option<Receiver<LifecycleEvent>>,
    pub workload_pid: Option<u32>,
    pub log_path: Option<PathBuf>,
}

#[derive(Clone)]
pub enum ExecuteMode {
    Foreground,
    Background,
    Piped,
    /// Connect the child's stdout and stderr directly to this log file at
    /// `Command::spawn` time via `Stdio::from(File)`. Use this when the
    /// caller intends to detach (e.g. `ato app session start` exits while
    /// the child keeps running): the kernel keeps the file descriptor wired
    /// to the file, so the child's writes survive the parent's exit. The
    /// older `Piped` + proxy-thread pattern in `attach_process_logs`
    /// silently dropped output once the parent thread died.
    Logged(PathBuf),
}

#[allow(clippy::too_many_arguments)]
pub fn execute(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    effective_state: Option<&EffectiveLockState>,
    nacelle_override: Option<PathBuf>,
    _reporter: std::sync::Arc<CliReporter>,
    enforcement: &str,
    mode: ExecuteMode,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<CapsuleProcess> {
    provider_workspace::ensure_provider_python_execution_inputs(plan, authoritative_lock)?;

    let nacelle = engine::discover_nacelle(engine::EngineRequest {
        explicit_path: nacelle_override,
        manifest_path: Some(plan.manifest_path.clone()),
        compat_input: None,
    })?;

    if let Some(lock) = authoritative_lock {
        let resolved =
            lock_runtime::resolve_lock_runtime_model(lock, Some(plan.selected_target_label()))?;
        let overlay = effective_state
            .map(|state| state.compiler_overlay.clone())
            .unwrap_or_default();
        let config = runtime_config::generate_config_from_lock(
            lock,
            &resolved,
            &overlay,
            Some(enforcement.to_string()),
            false,
        )?;
        if let Some(effective_state) = effective_state {
            crate::application::workspace::state::validate_config_against_policy(
                &config,
                &effective_state.policy,
            )?;
        }
        runtime_config::write_config(&plan.manifest_path, &config)?;
    } else {
        runtime_config::generate_and_write_config(
            &plan.manifest_path,
            Some(enforcement.to_string()),
            false,
        )?;
    }

    let adapter = NacelleExecAdapter::for_plan(plan, mode.clone(), launch_ctx)?;
    let (child, event_rx, exec_meta) =
        spawn_internal_exec(&nacelle, &plan.manifest_dir, &adapter.payload, mode)?;

    Ok(CapsuleProcess {
        child,
        cleanup_paths: adapter.cleanup_paths,
        event_rx: Some(event_rx),
        workload_pid: exec_meta.pid,
        log_path: exec_meta.log_path,
    })
}

pub fn execute_host(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    _reporter: std::sync::Arc<CliReporter>,
    mode: ExecuteMode,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<CapsuleProcess> {
    provider_workspace::ensure_provider_python_execution_inputs(plan, authoritative_lock)?;

    let mut launch_spec = derive_launch_spec(plan)?;
    launch_spec
        .args
        .extend(launch_ctx.command_args().iter().cloned());
    let desktop_open_bundle = desktop_native_open_bundle_path(plan, authoritative_lock);
    let force_python_no_bytecode =
        is_python_launch_spec(plan, &launch_spec.command, launch_spec.language.as_deref());
    let force_node_runtime =
        is_node_launch_spec(plan, &launch_spec.command, launch_spec.language.as_deref());
    let injected_port =
        runtime_overrides::override_port(launch_spec.port).map(|port| port.to_string());
    let readiness_port = runtime_overrides::override_port(launch_spec.port);
    let host_command_path =
        resolve_host_command_path(&launch_spec.working_dir, &launch_spec.command);
    let execution_cwd = resolve_host_execution_cwd(launch_ctx, &launch_spec.working_dir);

    let mut cmd = if let Some(bundle_path) = desktop_open_bundle.as_ref() {
        build_desktop_open_command(bundle_path, &launch_spec.args)
    } else if force_python_no_bytecode {
        // Prefer the venv's interpreter when the build phase produced
        // one, so installed deps are visible without needing `uv run`.
        // Falls back to the ato-managed toolchain if no venv exists
        // (e.g. capsules that ship deps via PYTHONPATH or rely on
        // host-installed packages).
        let python_bin = venv_python_binary(&launch_spec.working_dir).map_or_else(
            || {
                resolve_host_managed_runtime_binary(
                    plan,
                    authoritative_lock,
                    ManagedRuntimeKind::Python,
                )
            },
            Ok,
        )?;
        let mut python = Command::new(python_bin);
        python.arg(&host_command_path);
        python
    } else if force_node_runtime {
        let node_bin = resolve_host_managed_runtime_binary(
            plan,
            authoritative_lock,
            ManagedRuntimeKind::Node,
        )?;
        let mut node = Command::new(node_bin);
        node.arg(&host_command_path);
        node
    } else {
        Command::new(&host_command_path)
    };

    cmd.current_dir(&execution_cwd);
    if desktop_open_bundle.is_none() {
        apply_host_isolation(
            &mut cmd,
            plan,
            &launch_spec.env_vars,
            launch_spec.port,
            launch_ctx,
        )?;
        apply_python_runtime_hardening(&mut cmd, force_python_no_bytecode);

        if let Some(port) = injected_port {
            cmd.env("PORT", port);
        }

        if !launch_spec.args.is_empty() {
            cmd.args(&launch_spec.args);
        }
    }

    match mode {
        ExecuteMode::Foreground => {
            cmd.stdin(Stdio::inherit());
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }
        ExecuteMode::Background => {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
        ExecuteMode::Piped => {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        }
        ExecuteMode::Logged(log_path) => {
            apply_logged_stdio(&mut cmd, &log_path)?;
        }
    }

    // Run the host-native consumer in its own process group on Unix
    // so a parent SIGKILL doesn't strand it as a PID-1 orphan still
    // bound to the consumer port. Mirrors the change the orchestrator
    // applies to provider spawns; the session-start sweep (and the
    // UI-stop pgroup-kill that already lives in session.rs) can then
    // reap the consumer's subtree atomically. See ato-run/ato#121.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }

    let child = cmd
        .spawn()
        .context("Failed to execute host process with --dangerously-skip-permissions")?;
    let event_rx = if desktop_open_bundle.is_some() {
        None
    } else {
        Some(spawn_host_lifecycle_events(child.id(), readiness_port))
    };

    Ok(CapsuleProcess {
        child,
        cleanup_paths: Vec::new(),
        event_rx,
        workload_pid: None,
        log_path: None,
    })
}

pub fn execute_open_path(app_path: &Path, mode: ExecuteMode) -> Result<CapsuleProcess> {
    let mut cmd = build_desktop_open_command(app_path, &[]);

    match mode {
        ExecuteMode::Foreground => {
            cmd.stdin(Stdio::inherit());
            cmd.stdout(Stdio::inherit());
            cmd.stderr(Stdio::inherit());
        }
        ExecuteMode::Background => {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
        ExecuteMode::Piped => {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        }
        ExecuteMode::Logged(log_path) => {
            apply_logged_stdio(&mut cmd, &log_path)?;
        }
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to open desktop app: {}", app_path.display()))?;

    Ok(CapsuleProcess {
        child,
        cleanup_paths: Vec::new(),
        event_rx: None,
        workload_pid: None,
        log_path: None,
    })
}

/// Open `log_path` for append (creating it and any parent directories) and
/// return an owning `File`. Used to wire `Stdio::from(file)` directly into
/// the child at spawn time so the redirection survives the parent process's
/// exit — see `ExecuteMode::Logged`.
fn open_log_file_for_stdio(log_path: &Path) -> Result<fs::File> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))
}

/// Wire stdin=null and connect stdout+stderr to `log_path` at spawn time.
/// The kernel keeps the file descriptor connected to the file across the
/// parent's exit, so detached children (`ato app session start` returning
/// after spawn) keep writing to the log without a proxy thread.
fn apply_logged_stdio(cmd: &mut Command, log_path: &Path) -> Result<()> {
    let stdout_handle = open_log_file_for_stdio(log_path)?;
    let stderr_handle = stdout_handle
        .try_clone()
        .with_context(|| format!("failed to clone log file {}", log_path.display()))?;
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(stdout_handle));
    cmd.stderr(Stdio::from(stderr_handle));
    Ok(())
}

fn apply_host_isolation(
    cmd: &mut Command,
    _plan: &ManifestData,
    launch_env: &std::collections::HashMap<String, String>,
    launch_port: Option<u16>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<()> {
    // Host isolation dirs (HOME, TMPDIR, npm/pip/pnpm caches) live under
    // ~/.ato/cache/run-host/ so the user's project directory stays clean.
    let isolation_root = ato_cache_dir().join("run-host");
    fs::create_dir_all(&isolation_root).with_context(|| {
        format!(
            "Failed to create host isolation root: {}",
            isolation_root.display()
        )
    })?;
    let isolation = HostIsolationContext::new(&isolation_root, "run").with_context(|| {
        format!(
            "Failed to prepare isolated host execution context under {}",
            isolation_root.display()
        )
    })?;

    let launch_ctx_env = validated_launch_context_env(launch_ctx)?;
    let mut extra_env = runtime_overrides::merged_env(launch_env.clone())
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(port) = runtime_overrides::override_port(launch_port) {
        extra_env.push(("PORT".to_string(), port.to_string()));
    }
    extra_env.extend(launch_ctx_env);
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        extra_env.extend([
            ("HTTP_PROXY".to_string(), proxy_env.http_proxy),
            ("HTTPS_PROXY".to_string(), proxy_env.https_proxy),
            ("ALL_PROXY".to_string(), proxy_env.all_proxy),
            ("NO_PROXY".to_string(), proxy_env.no_proxy),
        ]);
    }
    isolation.apply_to_command(cmd, extra_env);

    Ok(())
}

fn validated_launch_context_env(
    launch_ctx: &RuntimeLaunchContext,
) -> Result<Vec<(String, String)>> {
    if let Some(env) = launch_ctx.ipc_env_vars() {
        for key in env.keys() {
            if key.starts_with("CAPSULE_IPC_") || key == "ATO_BRIDGE_TOKEN" {
                continue;
            }

            return Err(
                capsule_core::execution_plan::error::AtoExecutionError::policy_violation(format!(
                    "session_token env '{}' is not allowlisted",
                    key
                ))
                .into(),
            );
        }
    }

    Ok(launch_ctx.merged_env().into_iter().collect())
}

#[derive(Clone, Copy)]
enum ManagedRuntimeKind {
    Node,
    Python,
}

fn resolve_host_managed_runtime_binary(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
    runtime: ManagedRuntimeKind,
) -> Result<PathBuf> {
    match runtime {
        ManagedRuntimeKind::Node => {
            runtime_manager::ensure_node_binary_with_authority(plan, authoritative_lock)
        }
        ManagedRuntimeKind::Python => {
            runtime_manager::ensure_python_binary_with_authority(plan, authoritative_lock)
        }
    }
}

fn executable_extensions() -> Vec<String> {
    if cfg!(windows) {
        env::var_os("PATHEXT")
            .map(|value| {
                env::split_paths(&value)
                    .map(|path| path.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| vec![".exe".to_string(), ".cmd".to_string(), ".bat".to_string()])
    } else {
        Vec::new()
    }
}

#[allow(dead_code)]
pub fn should_launch_desktop_native_with_host_open(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> bool {
    desktop_native_open_bundle_path(plan, authoritative_lock).is_some()
}

fn desktop_native_open_bundle_path(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        desktop_native_open_bundle_path_from_runtime_lock(&plan.manifest_dir).or_else(|| {
            desktop_native_open_bundle_path_from_authoritative_lock(plan, authoritative_lock)
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = plan;
        let _ = authoritative_lock;
        None
    }
}

#[cfg(target_os = "macos")]
fn desktop_native_open_bundle_path_from_runtime_lock(manifest_dir: &Path) -> Option<PathBuf> {
    let lock_path = manifest_dir.join("capsule.lock.json");
    let lock = serde_json::from_slice::<serde_json::Value>(&fs::read(lock_path).ok()?).ok()?;
    desktop_native_bundle_path_from_value(manifest_dir, &lock)
}

#[cfg(target_os = "macos")]
fn desktop_native_open_bundle_path_from_authoritative_lock(
    plan: &ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> Option<PathBuf> {
    let mut root = serde_json::Map::new();
    let mut contract = serde_json::Map::new();
    contract.insert(
        "delivery".to_string(),
        authoritative_lock?
            .contract
            .entries
            .get("delivery")?
            .clone(),
    );
    root.insert("contract".to_string(), serde_json::Value::Object(contract));
    desktop_native_bundle_path_from_value(&plan.manifest_dir, &serde_json::Value::Object(root))
}

#[cfg(target_os = "macos")]
fn desktop_native_bundle_path_from_value(
    manifest_dir: &Path,
    lock: &serde_json::Value,
) -> Option<PathBuf> {
    let artifact_path = lock
        .get("contract")
        .and_then(|value| value.get("delivery"))
        .and_then(|value| value.get("artifact"))
        .and_then(serde_json::Value::as_object)
        .filter(|artifact| {
            artifact.get("kind").and_then(serde_json::Value::as_str) == Some("desktop-native")
        })
        .and_then(|artifact| artifact.get("path"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let bundle_path = manifest_dir.join(artifact_path);
    bundle_path
        .extension()
        .map(|extension| extension.to_string_lossy().eq_ignore_ascii_case("app"))
        .filter(|is_app| *is_app)
        .map(|_| bundle_path)
}

fn build_desktop_open_command(bundle_path: &Path, args: &[String]) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("/usr/bin/open");
        cmd.arg(bundle_path);
        if !args.is_empty() {
            cmd.arg("--args");
            cmd.args(args);
        }
        cmd
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle_path;
        let _ = args;
        unreachable!("desktop app open command is only used on macOS")
    }
}

/// Locate the Python interpreter inside `<working_dir>/.venv` if the
/// provisioning step created one. Returns `None` when the venv (or its
/// `bin/python`) is missing, so callers can fall back to the
/// ato-managed toolchain.
fn venv_python_binary(working_dir: &Path) -> Option<PathBuf> {
    // POSIX: `.venv/bin/python`. Windows: `.venv/Scripts/python.exe`.
    // We probe both candidates so the same code path works on every
    // platform that ships an `ato` binary.
    let candidates = [
        working_dir.join(".venv").join("bin").join("python"),
        working_dir.join(".venv").join("Scripts").join("python.exe"),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

fn resolve_host_command_path(working_dir: &Path, command: &str) -> PathBuf {
    let command_path = Path::new(command.trim());
    if command_path.is_absolute() {
        return command_path.to_path_buf();
    }
    let relative = working_dir.join(command_path);
    if relative.exists() {
        return fs::canonicalize(&relative).unwrap_or(relative);
    }
    command_path.to_path_buf()
}

/// Decide the cwd for the spawned target process.
///
/// Caller cwd (`launch_ctx.effective_cwd`) and execution cwd are deliberately
/// distinct concepts. effective_cwd is the user's pwd when `ato run` was
/// invoked — it stays useful for relative-path arg resolution, grant
/// inference, and IO candidate detection upstream. The **process** cwd,
/// however, defaults to the manifest-declared `working_dir` so module
/// imports and relative scripts resolve against the capsule's source tree.
///
/// We promote effective_cwd to the execution cwd **only** when the user
/// is plainly invoking from inside the capsule's own workspace (= local
/// one-shot run, e.g. `ato run .` or `ato run ./script.py` with cwd
/// inside the project). For materialized capsules fetched into
/// `<ato_home>/runs/<id>/...` or `<ato_home>/external-capsules/...` the
/// user's caller cwd is unrelated to the capsule's source tree and
/// must not be used as the process cwd.
///
/// One subtlety: when the caller is **exactly** at the workspace root
/// (e.g. `ato run .` from the project directory) and the manifest
/// declares a more specific `working_dir` (e.g. `working_dir = "backend"`
/// for a flat v0.3 layout), the manifest's working_dir wins — otherwise
/// `python -m uvicorn main:app` would be invoked from the project root
/// and fail to import `main` because `main.py` lives in `backend/`. If
/// the user has actually cd'd into a subdirectory of the workspace, the
/// caller cwd still wins (preserves the existing local-one-shot ergonomics).
fn resolve_host_execution_cwd(launch_ctx: &RuntimeLaunchContext, working_dir: &Path) -> PathBuf {
    let working = working_dir.to_path_buf();
    let Some(caller) = launch_ctx.effective_cwd() else {
        return working;
    };
    if launch_ctx.effective_cwd_is_explicit_override() {
        return caller.clone();
    }
    // No workspace_root recorded → conservative fallback to working_dir.
    let Some(workspace) = launch_ctx.workspace_root() else {
        return working;
    };
    // effective_cwd is authoritative only if it lives inside (or equals)
    // the materialized capsule's workspace_root. Compare canonicalized
    // paths so symlinks and relative segments don't slip through.
    let caller_canonical = std::fs::canonicalize(caller).unwrap_or_else(|_| caller.clone());
    let workspace_canonical =
        std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.clone());
    if !caller_canonical.starts_with(&workspace_canonical) {
        return working;
    }
    let working_canonical = std::fs::canonicalize(&working).unwrap_or_else(|_| working.clone());
    if caller_canonical == workspace_canonical && working_canonical != workspace_canonical {
        return working;
    }
    caller.clone()
}

fn is_python_launch_spec(plan: &ManifestData, command: &str, language: Option<&str>) -> bool {
    if language
        .map(|value| value.trim().eq_ignore_ascii_case("python"))
        .unwrap_or(false)
    {
        return true;
    }

    if plan
        .execution_driver()
        .map(|driver| driver.trim().eq_ignore_ascii_case("python"))
        .unwrap_or(false)
    {
        return true;
    }

    command.trim().to_ascii_lowercase().ends_with(".py")
}

fn is_node_launch_spec(plan: &ManifestData, command: &str, language: Option<&str>) -> bool {
    if language
        .map(|value| value.trim().eq_ignore_ascii_case("node"))
        .unwrap_or(false)
    {
        return true;
    }

    if plan
        .execution_driver()
        .map(|driver| driver.trim().eq_ignore_ascii_case("node"))
        .unwrap_or(false)
    {
        return true;
    }

    matches!(
        command.trim().to_ascii_lowercase().as_str(),
        value if value.ends_with(".js")
            || value.ends_with(".cjs")
            || value.ends_with(".mjs")
            || value.ends_with(".ts")
    )
}

fn apply_python_runtime_hardening(cmd: &mut Command, force_python_no_bytecode: bool) {
    if force_python_no_bytecode {
        cmd.env("PYTHONDONTWRITEBYTECODE", "1");
        // Force unbuffered stdout/stderr. Python defaults to block-buffered
        // I/O whenever stdout isn't a TTY (i.e. when we redirect to a pipe
        // or a log file via `Stdio::from(File)`), so short-lived `print()`
        // calls sit in the 8 KB user-space buffer and never reach the log
        // file the desktop tails for `display_strategy=terminal_stream`.
        // Long-running services hide this behind their own framework
        // logging (uvicorn, fastapi, etc.) but a bare `python app.py`
        // surfaces it as "ato Desktop session is ready but the terminal
        // pane is empty". Mirrors `start_guest_session`, which sets the
        // same env var for the same reason.
        cmd.env("PYTHONUNBUFFERED", "1");
    }
}

struct NacelleExecAdapter {
    payload: serde_json::Value,
    cleanup_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NacelleExecMeta {
    pid: Option<u32>,
    log_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct NacelleExecResponse {
    ok: bool,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    log_path: Option<String>,
    #[serde(default)]
    error: Option<NacelleExecError>,
}

#[derive(Debug, Deserialize)]
struct NacelleExecError {
    message: String,
}

impl NacelleExecAdapter {
    fn for_plan(
        plan: &ManifestData,
        _mode: ExecuteMode,
        launch_ctx: &RuntimeLaunchContext,
    ) -> Result<Self> {
        let normalized_manifest_path =
            write_normalized_manifest(plan, launch_ctx.command_args(), launch_ctx.dep_endpoints())?;
        let mut env_map = runtime_overrides::merged_env(plan.execution_env());
        if plan
            .execution_language()
            .or_else(|| plan.execution_driver())
            .or_else(|| plan.execution_runtime())
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("python"))
        {
            extend_python_selector_env(&mut env_map, plan.execution_runtime_version().as_deref());
        }
        let mut env = env_map.into_iter().collect::<Vec<_>>();
        if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
            env.push(("PORT".to_string(), port.to_string()));
        }
        if plan
            .execution_entrypoint()
            .map(|entry| entry.trim().to_ascii_lowercase().ends_with(".py"))
            .unwrap_or(false)
        {
            env.push(("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string()));
        }

        for (key, value) in launch_ctx.injected_env() {
            env.push((key.clone(), value.clone()));
        }

        if let Some(ipc_env) = launch_ctx.ipc_env_vars() {
            for (key, value) in ipc_env {
                env.push((key.clone(), value.clone()));
            }
        }

        if let Some(selected_python_runtime) =
            normalized_python_runtime_version(plan.execution_runtime_version().as_deref())
        {
            debug!(
                selected_python_runtime = %selected_python_runtime,
                "Prepared source/python runtime selector"
            );
        }

        let ipc_env = launch_ctx
            .ipc_env_vars()
            .map(|env| {
                env.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Vec<_>>()
            })
            .filter(|env| !env.is_empty());
        let ipc_socket_paths = launch_ctx
            .socket_paths()
            .map(|paths| {
                paths
                    .values()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|paths| !paths.is_empty());

        // `interactive` in nacelle's ExecEnvelope means "allocate a PTY and
        // treat stdin as TerminalCommand JSON". Source workloads (uvicorn,
        // node, etc.) are never PTY sessions — those go through
        // workload.type=shell. Foreground execute mode means "ato waits on
        // the child"; it does NOT imply PTY. Setting interactive=true here
        // routes to launch_interactive, which returns a pid without
        // registering the child, so the supervisor fails with
        // "lost child handle".
        Ok(Self {
            payload: json!({
                "spec_version": "0.1.0",
                "interactive": false,
                "workload": {
                    "type": "source",
                    "manifest": normalized_manifest_path.display().to_string(),
                },
                "env": env,
                "cwd": runtime_cwd_payload(launch_ctx, &plan.execution_working_directory()),
                "mounts": launch_ctx
                    .injected_mounts()
                    .iter()
                    .map(|mount| json!({
                        "source": mount.source.display().to_string(),
                        "target": mount.target,
                        "readonly": mount.readonly,
                    }))
                    .collect::<Vec<_>>(),
                "ipc_env": ipc_env,
                "ipc_socket_paths": ipc_socket_paths,
            }),
            cleanup_paths: vec![normalized_manifest_path],
        })
    }
}

fn runtime_cwd_payload(launch_ctx: &RuntimeLaunchContext, working_dir: &Path) -> Option<String> {
    if cfg!(target_os = "linux") {
        // The Linux sandbox bind-mounts the materialized workspace at
        // /workspace, so the cwd inside the sandbox is always /workspace
        // regardless of the host effective_cwd. Subdirectories like
        // working_dir.relative_to(workspace) are not yet routed here —
        // the orchestrator's existing /workspace contract preserves the
        // historical behaviour.
        return Some("/workspace".to_string());
    }
    // macOS sandbox runs in the host filesystem namespace, so apply the
    // same caller-vs-workspace resolution the host execution path uses
    // (resolve_host_execution_cwd). Without this, nacelle would receive
    // launch_ctx.effective_cwd() — the user's caller pwd — and uvicorn /
    // node entrypoints would be invoked from outside the materialized
    // workspace, breaking module imports and `--with-requirements`
    // resolution. Callers that explicitly set --cwd, or invoke ato from
    // inside the workspace, still get their pwd honoured.
    Some(
        resolve_host_execution_cwd(launch_ctx, working_dir)
            .display()
            .to_string(),
    )
}

fn write_normalized_manifest(
    plan: &ManifestData,
    explicit_args: &[String],
    dep_endpoints: &[String],
) -> Result<PathBuf> {
    let launch_spec = derive_launch_spec(plan)?;
    let entrypoint = plan
        .execution_entrypoint()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| launch_spec.command.clone());
    let sandbox_entrypoint = if is_python_module_flag(&entrypoint) {
        entrypoint.clone()
    } else {
        sandbox_source_entrypoint(plan, &entrypoint)
    };
    let mut cmd_args = if plan
        .execution_entrypoint()
        .filter(|value| !value.trim().is_empty())
        .is_some()
    {
        plan.targets_oci_cmd()
    } else {
        launch_spec.args
    };
    cmd_args.extend(explicit_args.iter().cloned());
    let language_name = plan
        .execution_language()
        .or_else(|| plan.execution_driver())
        .or_else(|| plan.execution_runtime());
    let runtime_version = plan.execution_runtime_version();
    let is_python = language_name
        .as_deref()
        .map(|value| value.eq_ignore_ascii_case("python"))
        .unwrap_or(false);
    let is_provider_workspace = provider_workspace::is_provider_workspace(&plan.manifest_dir);

    let (normalized_entrypoint, command) = if is_python {
        if is_provider_workspace {
            let mut tokens = vec![sandbox_entrypoint.clone()];
            tokens.extend(cmd_args.iter().cloned());
            (
                "python3".to_string(),
                Some(shell_words::join(tokens.iter().map(String::as_str))),
            )
        } else {
            let mut tokens = vec!["run".to_string()];
            if has_local_uv_cache(plan) {
                tokens.push("--offline".to_string());
            }
            if let Some(requirements) = resolve_python_requirements_argument(plan) {
                tokens.push("--with-requirements".to_string());
                tokens.push(requirements);
            }
            tokens.push("python3".to_string());
            tokens.push(sandbox_entrypoint.clone());
            tokens.extend(cmd_args.iter().cloned());
            (
                "uv".to_string(),
                Some(shell_words::join(tokens.iter().map(String::as_str))),
            )
        }
    } else {
        let command = (!cmd_args.is_empty()).then(|| cmd_args.join(" "));
        (sandbox_entrypoint, command)
    };

    let mut manifest = toml::map::Map::new();
    manifest.insert(
        "name".to_string(),
        plan.manifest
            .get("name")
            .cloned()
            .unwrap_or_else(|| toml::Value::String("capsule".to_string())),
    );
    manifest.insert(
        "version".to_string(),
        plan.manifest
            .get("version")
            .cloned()
            .unwrap_or_else(|| toml::Value::String("0.0.0".to_string())),
    );

    let mut execution = toml::map::Map::new();
    execution.insert(
        "entrypoint".to_string(),
        toml::Value::String(normalized_entrypoint),
    );
    if let Some(command) = command {
        execution.insert("command".to_string(), toml::Value::String(command));
    }
    if is_python {
        if let Some(env_table) = python_runtime_selector_env(runtime_version.as_deref()) {
            execution.insert("env".to_string(), toml::Value::Table(env_table));
        }
    }
    manifest.insert("execution".to_string(), toml::Value::Table(execution));

    let mut isolation_table = plan
        .manifest
        .get("isolation")
        .and_then(|value| value.as_table())
        .cloned()
        .unwrap_or_default();
    // Merge the manifest's top-level `[network]` section under
    // `[isolation.network]` for the synthesized nacelle manifest. v0.3
    // capsule manifests author network policy at the top level; nacelle's
    // `IsolationConfig` parser only sees `[isolation.network]` and
    // otherwise falls back to a deny-by-default `NetworkPermissions::default()`
    // (because the `#[serde(default = ...)]` only fires when the FIELD
    // exists), which silently drops the user's `egress_allow` and
    // produces a Seatbelt profile that denies all networking.
    let mut network_table = plan
        .manifest
        .get("network")
        .and_then(|value| value.as_table())
        .cloned()
        .unwrap_or_default();
    if !network_table.contains_key("enabled") {
        network_table.insert("enabled".to_string(), toml::Value::Boolean(true));
    }
    if !dep_endpoints.is_empty() {
        let existing = network_table
            .remove("egress_allow")
            .and_then(|value| match value {
                toml::Value::Array(values) => Some(values),
                _ => None,
            })
            .unwrap_or_default();
        let mut merged: Vec<toml::Value> = existing;
        let mut seen: std::collections::HashSet<String> = merged
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect();
        for endpoint in dep_endpoints {
            if seen.insert(endpoint.clone()) {
                merged.push(toml::Value::String(endpoint.clone()));
            }
        }
        network_table.insert("egress_allow".to_string(), toml::Value::Array(merged));
    }
    if !network_table.is_empty() {
        isolation_table.insert("network".to_string(), toml::Value::Table(network_table));
    }
    if !isolation_table.is_empty() {
        manifest.insert("isolation".to_string(), toml::Value::Table(isolation_table));
    }

    let language_version = plan.execution_runtime_version();
    if language_name.is_some() || language_version.is_some() {
        let mut language = toml::map::Map::new();
        if let Some(name) = language_name {
            language.insert("language".to_string(), toml::Value::String(name));
        }
        if let Some(version) = language_version {
            language.insert("version".to_string(), toml::Value::String(version));
        }
        manifest.insert("language".to_string(), toml::Value::Table(language));
    }

    // Phase A0 source non-pollution: write the synthetic nacelle manifest into
    // ~/.ato/runs/nacelle-manifests/ instead of plan.manifest_dir so it cannot
    // perturb the source_tree_hash observation. The file lifetime is still
    // managed via cleanup_paths.
    let nacelle_dir = capsule_core::common::paths::ato_runs_dir().join("nacelle-manifests");
    sweep_stale_nacelle_manifests_on_startup_best_effort(&nacelle_dir);
    fs::create_dir_all(&nacelle_dir).with_context(|| {
        format!(
            "Failed to create nacelle manifest dir {}",
            nacelle_dir.display()
        )
    })?;
    let owner = current_nacelle_manifest_owner();
    let path = nacelle_dir.join(format_nacelle_manifest_file_name(
        owner,
        rand::thread_rng().gen::<u64>(),
    ));
    fs::write(&path, toml::to_string(&toml::Value::Table(manifest))?)
        .with_context(|| format!("Failed to write normalized manifest: {}", path.display()))?;
    Ok(path)
}

fn sweep_stale_nacelle_manifests_on_startup_best_effort(dir: &Path) {
    static SWEPT_DIRS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

    let swept_dirs = SWEPT_DIRS.get_or_init(|| Mutex::new(HashSet::new()));
    let should_sweep = {
        let mut swept_dirs = swept_dirs
            .lock()
            .expect("nacelle sweep dirs mutex poisoned");
        swept_dirs.insert(dir.to_path_buf())
    };
    if !should_sweep {
        return;
    }

    if let Err(error) =
        sweep_stale_nacelle_manifests_in(dir, SystemTime::now(), NACELLE_MANIFEST_TTL)
    {
        debug!(dir = %dir.display(), error = %error, "failed to sweep stale nacelle manifests");
    }
}

fn sweep_stale_nacelle_manifests_in(dir: &Path, now: SystemTime, ttl: Duration) -> Result<usize> {
    if !dir.exists() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in fs::read_dir(dir)
        .with_context(|| format!("Failed to read nacelle manifest dir: {}", dir.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable nacelle manifest entry");
                continue;
            }
        };
        let path = entry.path();
        if !is_nacelle_manifest_path(&path) || nacelle_manifest_owner_is_alive(&path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                debug!(path = %path.display(), error = %error, "skipping nacelle manifest with unreadable metadata");
                continue;
            }
        };
        if !metadata.is_file() || !nacelle_manifest_is_stale(&metadata, now, ttl) {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                debug!(path = %path.display(), error = %error, "failed to remove stale nacelle manifest")
            }
        }
    }
    Ok(removed)
}

fn is_nacelle_manifest_path(path: &Path) -> bool {
    parse_nacelle_manifest_owner(path).is_some()
}

fn nacelle_manifest_is_stale(metadata: &std::fs::Metadata, now: SystemTime, ttl: Duration) -> bool {
    let Some(modified) = metadata.modified().ok() else {
        return false;
    };
    now.duration_since(modified)
        .map(|age| age >= ttl)
        .unwrap_or(false)
}

fn current_nacelle_manifest_owner() -> NacelleManifestOwner {
    NacelleManifestOwner {
        pid: std::process::id(),
        start_time_unix_ms: ato_session_core::process::process_start_time_unix_ms(
            std::process::id(),
        ),
    }
}

fn format_nacelle_manifest_file_name(owner: NacelleManifestOwner, nonce: u64) -> String {
    let start_time = owner.start_time_unix_ms.unwrap_or(0);
    format!("nacelle-{}-{}-{}.toml", owner.pid, start_time, nonce)
}

fn parse_nacelle_manifest_owner(path: &Path) -> Option<NacelleManifestOwner> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_prefix("nacelle-")?.strip_suffix(".toml")?;
    let mut parts = stem.split('-');
    let pid = parts.next()?.parse().ok()?;
    let second = parts.next()?;
    let third = parts.next();
    if let Some(_nonce) = third {
        let start_time_unix_ms = second.parse::<u64>().ok().filter(|value| *value > 0);
        Some(NacelleManifestOwner {
            pid,
            start_time_unix_ms,
        })
    } else {
        Some(NacelleManifestOwner {
            pid,
            start_time_unix_ms: None,
        })
    }
}

fn nacelle_manifest_owner_is_alive(path: &Path) -> bool {
    let Some(owner) = parse_nacelle_manifest_owner(path) else {
        return false;
    };
    if owner.pid == 0 || !ato_session_core::process::pid_is_alive(owner.pid) {
        return false;
    }

    let Some(expected_start_time) = owner.start_time_unix_ms else {
        return false;
    };

    ato_session_core::process::process_start_time_unix_ms(owner.pid)
        .is_some_and(|live_start_time| live_start_time == expected_start_time)
}

fn python_runtime_selector_env(
    runtime_version: Option<&str>,
) -> Option<toml::map::Map<String, toml::Value>> {
    let env = python_selector_env(runtime_version)
        .into_iter()
        .map(|(key, value)| (key, toml::Value::String(value)))
        .collect::<toml::map::Map<_, _>>();
    (!env.is_empty()).then_some(env)
}

fn sandbox_source_entrypoint(plan: &ManifestData, entrypoint: &str) -> String {
    let relative = sandbox_source_entrypoint_relative(plan, entrypoint);
    if cfg!(target_os = "linux") {
        Path::new("/workspace").join(relative).display().to_string()
    } else {
        Path::new(".").join(relative).display().to_string()
    }
}

fn is_python_module_flag(entrypoint: &str) -> bool {
    entrypoint.trim() == "-m"
}

fn sandbox_source_entrypoint_relative(plan: &ManifestData, entrypoint: &str) -> PathBuf {
    let entrypoint_path = Path::new(entrypoint.trim());

    if provider_workspace::is_provider_workspace(&plan.manifest_dir) {
        return entrypoint_path.to_path_buf();
    }

    if plan.execution_source_layout().as_deref() == Some("anchored_entrypoint") {
        return entrypoint_path.to_path_buf();
    }

    if plan.manifest_dir.join(entrypoint_path).exists() {
        return entrypoint_path.to_path_buf();
    }

    match plan.execution_working_dir() {
        Some(raw_working_dir) => {
            let trimmed = raw_working_dir.trim();
            if trimmed.is_empty() || trimmed == "." {
                Path::new("source").join(entrypoint_path)
            } else {
                Path::new("source")
                    .join(trimmed.trim_start_matches("./"))
                    .join(entrypoint_path)
            }
        }
        None => Path::new("source").join(entrypoint_path),
    }
}

fn resolve_python_requirements_argument(plan: &ManifestData) -> Option<String> {
    let working_dir = plan.execution_working_directory();
    let candidates = [
        working_dir.join("requirements.txt"),
        plan.manifest_dir.join("requirements.txt"),
        plan.manifest_dir.join("source").join("requirements.txt"),
    ];
    // Return an absolute path. The previous code returned a path relative to
    // manifest_dir, but the consumer is launched via nacelle with cwd=
    // launch_ctx.effective_cwd() — the user's caller pwd, not manifest_dir.
    // For `ato run capsule://github.com/...` the caller pwd has nothing to
    // do with the materialized capsule layout, so a relative path resolves
    // against the wrong tree and uv aborts with `File not found`. The host
    // sandbox already grants read access to source_dir / manifest_dir, so an
    // absolute path is reachable regardless of cwd.
    candidates
        .into_iter()
        .find(|path| path.exists())
        .map(|path| path.to_string_lossy().to_string())
}

fn has_local_uv_cache(plan: &ManifestData) -> bool {
    let roots = [
        plan.execution_working_directory(),
        plan.manifest_dir.clone(),
        plan.manifest_dir.join("source"),
    ];
    roots.into_iter().any(|root| {
        [workspace_artifacts_dir(&root), root.join("artifacts")]
            .into_iter()
            .any(|artifacts_dir| {
                fs::read_dir(&artifacts_dir)
                    .ok()
                    .into_iter()
                    .flat_map(|entries| entries.filter_map(Result::ok))
                    .any(|entry| entry.path().join("uv-cache").is_dir())
            })
    })
}

fn spawn_internal_exec(
    nacelle: &Path,
    manifest_dir: &Path,
    payload: &serde_json::Value,
    mode: ExecuteMode,
) -> Result<(Child, Receiver<LifecycleEvent>, NacelleExecMeta)> {
    let mut cmd = Command::new(nacelle);
    cmd.arg("internal")
        .arg("--input")
        .arg("-")
        .arg("exec")
        .current_dir(manifest_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());

    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }

    match mode {
        ExecuteMode::Foreground => {
            cmd.stderr(Stdio::inherit());
        }
        ExecuteMode::Background => {
            cmd.stderr(Stdio::null());
        }
        ExecuteMode::Piped => {
            cmd.stderr(Stdio::piped());
        }
        ExecuteMode::Logged(log_path) => {
            // Nacelle's `internal exec` protocol talks JSON over stdout, so
            // we cannot redirect stdout to a file here. Only stderr is safe
            // to send to the log; nacelle's worker stdout/stderr already
            // surface via `LifecycleEvent::ProcessOutput` to the supervisor
            // when it routes them downstream.
            let stderr_handle = open_log_file_for_stdio(&log_path)?;
            cmd.stderr(Stdio::from(stderr_handle));
        }
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn nacelle: {}", nacelle.display()))?;

    {
        let mut stdin = child.stdin.take().context("Failed to open nacelle stdin")?;
        let bytes = serde_json::to_vec(payload).context("Failed to serialize exec payload")?;
        stdin
            .write_all(&bytes)
            .context("Failed to write exec payload to nacelle")?;
    }

    let stdout = child
        .stdout
        .take()
        .context("Failed to capture nacelle stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut initial_events = Vec::new();
    let response = loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("Failed to read nacelle exec response")?;
        if read == 0 {
            anyhow::bail!("nacelle exec returned no response");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| format!("Failed to parse nacelle exec response: {trimmed}"))?;
        if value.get("ok").is_some() {
            break serde_json::from_value::<NacelleExecResponse>(value)
                .with_context(|| format!("Failed to parse nacelle exec response: {trimmed}"))?;
        }
        initial_events.push(trimmed.to_string());
    };
    if !response.ok {
        let message = response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "nacelle exec failed".to_string());
        anyhow::bail!(message);
    }

    let exec_meta = NacelleExecMeta {
        pid: response.pid,
        log_path: response.log_path.map(PathBuf::from),
    };

    let (event_tx, event_rx) = mpsc::channel();
    for event in initial_events {
        forward_nacelle_lifecycle_event(&event_tx, &event);
    }
    thread::spawn(move || {
        for maybe_line in reader.lines() {
            let Ok(line) = maybe_line else {
                break;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            forward_nacelle_lifecycle_event(&event_tx, trimmed);
        }
    });

    Ok((child, event_rx, exec_meta))
}

fn forward_nacelle_lifecycle_event(event_tx: &mpsc::Sender<LifecycleEvent>, event: &str) {
    match serde_json::from_str::<LifecycleEvent>(event) {
        Ok(event) => {
            let _ = event_tx.send(event);
        }
        Err(_) => {
            debug!(event, "nacelle exec event");
        }
    }
}

pub(crate) fn spawn_host_lifecycle_events(
    pid: u32,
    readiness_port: Option<u16>,
) -> Receiver<LifecycleEvent> {
    let (event_tx, event_rx) = mpsc::channel();
    let ready_tx = event_tx.clone();
    thread::spawn(move || {
        if let Some(port) = readiness_port {
            let timeout_secs = std::env::var("ATO_BACKGROUND_READY_WAIT_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60);
            let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
            while std::time::Instant::now() < deadline {
                // Stop polling if the process has already exited.
                #[cfg(unix)]
                if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                    break;
                }
                if std::net::TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                    Duration::from_millis(100),
                )
                .is_ok()
                {
                    let _ = ready_tx.send(LifecycleEvent::Ready {
                        service: "main".to_string(),
                        endpoint: Some(format!("tcp://127.0.0.1:{port}")),
                        port: Some(port),
                    });
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        } else {
            let _ = ready_tx.send(LifecycleEvent::Ready {
                service: "main".to_string(),
                endpoint: None,
                port: None,
            });
        }
    });

    event_rx
}

pub async fn wait_for_exit(child: &mut Child) -> Result<i32> {
    wait_for_pid_exit(child.id()).await
}

pub async fn wait_for_pid_exit(pid: u32) -> Result<i32> {
    let session_id = format!("dev-{}", rand::thread_rng().gen::<u64>());
    let handle = NativeHandle::new(session_id, pid);
    let config = SessionRunnerConfig::default();

    let reporter = crate::reporters::CliReporter::new(false);
    let metrics = SessionRunner::new(handle, reporter)
        .with_config(config)
        .run()
        .await?;

    Ok(extract_exit_code(&metrics))
}

fn extract_exit_code(metrics: &capsule_core::UnifiedMetrics) -> i32 {
    match &metrics.metadata {
        RuntimeMetadata::Nacelle { exit_code, .. } => (*exit_code).unwrap_or(1),
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::inject::{IpcContext, SessionActivationMode};
    use filetime::{set_file_mtime, FileTime};
    use std::collections::HashMap;
    use tempfile::tempdir;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn plan_from_manifest(dir: &tempfile::TempDir, manifest: &str, target: &str) -> ManifestData {
        let manifest_path = dir.path().join("capsule.toml");
        let mut parsed: toml::Value = toml::from_str(manifest).expect("manifest");
        let table = parsed
            .as_table_mut()
            .expect("test manifest must parse to a table");
        table
            .entry("schema_version".to_string())
            .or_insert_with(|| toml::Value::String("0.2".to_string()));
        table
            .entry("name".to_string())
            .or_insert_with(|| toml::Value::String("app".to_string()));
        table
            .entry("version".to_string())
            .or_insert_with(|| toml::Value::String("0.1.0".to_string()));
        table
            .entry("type".to_string())
            .or_insert_with(|| toml::Value::String("app".to_string()));
        table
            .entry("default_target".to_string())
            .or_insert_with(|| toml::Value::String(target.to_string()));
        capsule_core::router::execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path,
            dir.path().to_path_buf(),
            capsule_core::router::ExecutionProfile::Dev,
            Some(target),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    #[test]
    fn apply_host_isolation_keeps_home_isolated() {
        let dir = tempdir().expect("tempdir");
        let plan = plan_from_manifest(
            &dir,
            r#"
            [targets.dev]
            runtime = "source"
            language = "python"
            driver = "python"
            entrypoint = "main.py"

            [targets.dev.env]
            HOME = "/unsafe-home"
            APP_MODE = "dev"
            "#,
            "dev",
        );
        let launch_ctx = RuntimeLaunchContext::empty().with_injected_env(
            [
                ("HOME".to_string(), "/still-unsafe".to_string()),
                ("ATO_SERVICE_DB_HOST".to_string(), "127.0.0.1".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        let mut cmd = Command::new("echo");

        apply_host_isolation(&mut cmd, &plan, &plan.execution_env(), None, &launch_ctx)
            .expect("apply host isolation");

        let envs = cmd
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect::<HashMap<_, _>>();

        let isolated_home = envs
            .get("HOME")
            .and_then(|value| value.clone())
            .expect("HOME must be set");
        assert!(isolated_home.contains(".ato-run-host/home"));
        assert_eq!(
            envs.get("ATO_SERVICE_DB_HOST")
                .and_then(|value| value.clone()),
            Some("127.0.0.1".to_string())
        );
        assert_eq!(
            envs.get("APP_MODE").and_then(|value| value.clone()),
            Some("dev".to_string())
        );
    }

    #[test]
    fn resolve_host_command_path_absolutizes_existing_relative_commands() {
        let resolved =
            resolve_host_command_path(Path::new("tests/fixtures/native-shell-capsule"), "run.sh");

        assert!(resolved.is_absolute());
        assert!(resolved.ends_with(Path::new("tests/fixtures/native-shell-capsule/run.sh")));
    }

    #[test]
    #[ignore = "flaky: depends on shared provider-workspace classification state; tracked in #82"]
    fn sandbox_source_entrypoint_keeps_shell_entrypoints_relative_off_linux() {
        let dir = tempdir().expect("tempdir");
        let plan = plan_from_manifest(
            &dir,
            r#"
            [targets.dev]
            runtime = "source/native"
            run = "run.sh"
            "#,
            "dev",
        );

        let entrypoint = sandbox_source_entrypoint(&plan, "run.sh");

        if cfg!(target_os = "linux") {
            assert_eq!(entrypoint, "/workspace/run.sh");
        } else {
            assert!(entrypoint.starts_with("./"));
            assert!(entrypoint.ends_with("run.sh"));
        }
    }

    #[test]
    fn resolve_host_execution_cwd_prefers_caller_cwd_when_inside_workspace() {
        // Local one-shot: user invoked `ato run .` from inside the project
        // tree. effective_cwd lives inside workspace_root → caller cwd is
        // the authoritative process cwd.
        let workspace_root = std::env::temp_dir().join("ato-cwd-test-inside");
        let caller = workspace_root.join("subdir");
        std::fs::create_dir_all(&caller).unwrap();
        let working_dir = workspace_root.join("source");
        std::fs::create_dir_all(&working_dir).unwrap();

        let launch_ctx = RuntimeLaunchContext::empty()
            .with_effective_cwd(caller.clone())
            .with_workspace_root(workspace_root.clone());

        assert_eq!(
            resolve_host_execution_cwd(&launch_ctx, &working_dir),
            caller
        );

        let _ = std::fs::remove_dir_all(&workspace_root);
    }

    #[test]
    fn resolve_host_execution_cwd_prefers_explicit_override_even_outside_workspace() {
        let workspace_root = std::env::temp_dir().join("ato-cwd-test-explicit-override-workspace");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let working_dir = workspace_root.join("backend");
        std::fs::create_dir_all(&working_dir).unwrap();
        let override_cwd = std::env::temp_dir().join("ato-cwd-test-explicit-override-caller");
        std::fs::create_dir_all(&override_cwd).unwrap();

        let launch_ctx = RuntimeLaunchContext::empty()
            .with_effective_cwd_override(override_cwd.clone())
            .with_workspace_root(workspace_root.clone());

        assert_eq!(
            resolve_host_execution_cwd(&launch_ctx, &working_dir),
            override_cwd,
            "explicit --cwd override must win even when outside the materialized workspace"
        );

        let _ = std::fs::remove_dir_all(&workspace_root);
        let _ = std::fs::remove_dir_all(&override_cwd);
    }

    #[test]
    fn resolve_host_execution_cwd_falls_back_to_working_dir_when_caller_outside_workspace() {
        // Materialized capsule run (e.g. `ato run github.com/owner/repo`):
        // the user's caller cwd is unrelated to the fetched workspace,
        // so the process must cd into the manifest-declared working_dir
        // (typically the source/ subdirectory) for module imports to
        // resolve correctly.
        let workspace_root = std::env::temp_dir().join("ato-cwd-test-outside-workspace");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let working_dir = workspace_root.join("backend");
        std::fs::create_dir_all(&working_dir).unwrap();
        let caller = std::env::temp_dir().join("ato-cwd-test-outside-caller");
        std::fs::create_dir_all(&caller).unwrap();

        let launch_ctx = RuntimeLaunchContext::empty()
            .with_effective_cwd(caller.clone())
            .with_workspace_root(workspace_root.clone());

        assert_eq!(
            resolve_host_execution_cwd(&launch_ctx, &working_dir),
            working_dir,
            "materialized capsule must use working_dir, not caller's unrelated pwd"
        );

        let _ = std::fs::remove_dir_all(&workspace_root);
        let _ = std::fs::remove_dir_all(&caller);
    }

    #[test]
    fn resolve_host_execution_cwd_falls_back_to_working_dir() {
        // No effective_cwd at all (e.g. some integration paths) →
        // working_dir wins.
        let working_dir = PathBuf::from("/materialized/root");
        assert_eq!(
            resolve_host_execution_cwd(&RuntimeLaunchContext::empty(), &working_dir),
            working_dir
        );
    }

    #[test]
    fn resolve_host_execution_cwd_falls_back_when_workspace_root_missing() {
        // Defense-in-depth: if the run pipeline forgot to call
        // with_workspace_root, the executor falls back to working_dir
        // rather than blindly trusting the caller's pwd. This makes the
        // brick safe-by-default — only an explicit workspace_root
        // declaration unlocks caller-cwd promotion.
        let working_dir = PathBuf::from("/materialized/root");
        let caller = PathBuf::from("/somewhere/else");
        let launch_ctx = RuntimeLaunchContext::empty().with_effective_cwd(caller);
        assert_eq!(
            resolve_host_execution_cwd(&launch_ctx, &working_dir),
            working_dir
        );
    }

    /// WasedaP2P-style regression: manifest at workspace root, source
    /// code at `backend/`, target's `working_dir = "backend"`. When run
    /// via `ato run github.com/...` the caller cwd is unrelated to the
    /// fetched workspace; the process cwd must be `<workspace>/backend`
    /// so `python -m uvicorn main:app` finds `backend/main.py`.
    #[test]
    fn resolve_host_execution_cwd_wasedap2p_module_import_fixture() {
        let workspace_root = std::env::temp_dir().join("ato-wasedap2p-fixture");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let working_dir = workspace_root.join("backend");
        std::fs::create_dir_all(&working_dir).unwrap();
        std::fs::write(working_dir.join("main.py"), "app = None\n").unwrap();
        let caller = std::env::temp_dir().join("ato-wasedap2p-caller-elsewhere");
        std::fs::create_dir_all(&caller).unwrap();

        let launch_ctx = RuntimeLaunchContext::empty()
            .with_effective_cwd(caller.clone())
            .with_workspace_root(workspace_root.clone());
        let resolved = resolve_host_execution_cwd(&launch_ctx, &working_dir);

        assert_eq!(resolved, working_dir);
        assert!(
            resolved.join("main.py").exists(),
            "execution cwd must contain backend/main.py: {}",
            resolved.display()
        );

        let _ = std::fs::remove_dir_all(&workspace_root);
        let _ = std::fs::remove_dir_all(&caller);
    }

    /// Flat-layout v0.3 regression (#15): manifest at workspace root,
    /// source code at `backend/`, target's `working_dir = "backend"`,
    /// user invokes `ato run .` from the project directory itself.
    /// caller_cwd == workspace_root, so the previous rule promoted
    /// caller_cwd to execution_cwd and `python -m uvicorn main:app`
    /// was launched from the project root with no `main.py` visible.
    /// The manifest-declared working_dir must win in this case.
    #[test]
    fn resolve_host_execution_cwd_flat_v03_layout_prefers_working_dir_at_workspace_root() {
        let workspace_root = std::env::temp_dir().join("ato-cwd-test-flat-v03");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let working_dir = workspace_root.join("backend");
        std::fs::create_dir_all(&working_dir).unwrap();
        std::fs::write(working_dir.join("main.py"), "app = None\n").unwrap();

        let launch_ctx = RuntimeLaunchContext::empty()
            .with_effective_cwd(workspace_root.clone())
            .with_workspace_root(workspace_root.clone());
        let resolved = resolve_host_execution_cwd(&launch_ctx, &working_dir);

        assert_eq!(
            resolved, working_dir,
            "manifest working_dir must override caller_cwd when caller is exactly at workspace_root"
        );
        assert!(
            resolved.join("main.py").exists(),
            "execution cwd must contain backend/main.py: {}",
            resolved.display()
        );

        let _ = std::fs::remove_dir_all(&workspace_root);
    }

    #[test]
    fn test_apply_python_runtime_hardening_sets_env() {
        let mut cmd = Command::new("echo");
        apply_python_runtime_hardening(&mut cmd, true);

        let env_value = |needle: &str| -> Option<String> {
            cmd.get_envs().find_map(|(key, value)| {
                if key == needle {
                    value.map(|v| v.to_string_lossy().to_string())
                } else {
                    None
                }
            })
        };

        assert_eq!(
            env_value("PYTHONDONTWRITEBYTECODE").as_deref(),
            Some("1"),
            "PYTHONDONTWRITEBYTECODE must be set"
        );
        // PYTHONUNBUFFERED must be set so `print()` flushes through to the
        // log file the desktop tails (see `ExecuteMode::Logged`); without
        // it Python defaults to block-buffered I/O whenever stdout isn't a
        // TTY and short-lived output never reaches the file.
        assert_eq!(
            env_value("PYTHONUNBUFFERED").as_deref(),
            Some("1"),
            "PYTHONUNBUFFERED must be set"
        );
    }

    #[test]
    fn test_apply_python_runtime_hardening_noop_when_disabled() {
        let mut cmd = Command::new("echo");
        apply_python_runtime_hardening(&mut cmd, false);

        let has_dontwrite = cmd
            .get_envs()
            .any(|(key, _)| key == "PYTHONDONTWRITEBYTECODE");
        assert!(!has_dontwrite, "PYTHONDONTWRITEBYTECODE must not be set");

        let has_unbuffered = cmd.get_envs().any(|(key, _)| key == "PYTHONUNBUFFERED");
        assert!(!has_unbuffered, "PYTHONUNBUFFERED must not be set");
    }

    #[test]
    #[serial_test::serial]
    fn nacelle_manifest_sweep_removes_stale_files() {
        let temp = tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let dir = capsule_core::common::paths::ato_runs_dir().join("nacelle-manifests");
        fs::create_dir_all(&dir).expect("create nacelle manifest dir");
        let stale = dir.join("nacelle-123-456.toml");
        fs::write(&stale, "schema_version = \"0.3\"\n").expect("write stale manifest");

        let removed = sweep_stale_nacelle_manifests_in(
            &dir,
            SystemTime::now() + Duration::from_secs(48 * 60 * 60),
            Duration::from_secs(24 * 60 * 60),
        )
        .expect("sweep stale nacelle manifests");

        assert_eq!(removed, 1);
        assert!(!stale.exists());
    }

    #[test]
    #[serial_test::serial]
    fn nacelle_manifest_sweep_preserves_fresh_files() {
        let temp = tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let dir = capsule_core::common::paths::ato_runs_dir().join("nacelle-manifests");
        fs::create_dir_all(&dir).expect("create nacelle manifest dir");
        let fresh = dir.join("nacelle-123-789.toml");
        fs::write(&fresh, "schema_version = \"0.3\"\n").expect("write fresh manifest");

        let removed = sweep_stale_nacelle_manifests_in(
            &dir,
            SystemTime::now() + Duration::from_secs(60),
            Duration::from_secs(24 * 60 * 60),
        )
        .expect("sweep fresh nacelle manifests");

        assert_eq!(removed, 0);
        assert!(fresh.exists());
    }

    #[test]
    #[serial_test::serial]
    fn nacelle_manifest_sweep_preserves_active_process_manifest() {
        let temp = tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let dir = capsule_core::common::paths::ato_runs_dir().join("nacelle-manifests");
        fs::create_dir_all(&dir).expect("create nacelle manifest dir");
        let active_manifest = dir.join(format_nacelle_manifest_file_name(
            current_nacelle_manifest_owner(),
            42,
        ));
        fs::write(&active_manifest, "schema_version = \"0.3\"\n").expect("write active manifest");
        set_file_mtime(&active_manifest, FileTime::from_unix_time(1, 0))
            .expect("age active manifest");

        let removed = sweep_stale_nacelle_manifests_in(
            &dir,
            SystemTime::now() + Duration::from_secs(48 * 60 * 60),
            Duration::from_secs(24 * 60 * 60),
        )
        .expect("sweep active nacelle manifests");

        assert_eq!(removed, 0);
        assert!(active_manifest.exists());
    }

    #[test]
    #[serial_test::serial]
    fn nacelle_manifest_sweep_rejects_pid_reuse_when_start_time_mismatches() {
        let temp = tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let dir = capsule_core::common::paths::ato_runs_dir().join("nacelle-manifests");
        fs::create_dir_all(&dir).expect("create nacelle manifest dir");
        let stale = dir.join(format!("nacelle-{}-1-77.toml", std::process::id()));
        fs::write(&stale, "schema_version = \"0.3\"\n").expect("write stale manifest");
        set_file_mtime(&stale, FileTime::from_unix_time(1, 0)).expect("age stale manifest");

        let removed = sweep_stale_nacelle_manifests_in(
            &dir,
            SystemTime::now() + Duration::from_secs(48 * 60 * 60),
            Duration::from_secs(24 * 60 * 60),
        )
        .expect("sweep mismatched active nacelle manifests");

        assert_eq!(removed, 1);
        assert!(!stale.exists());
    }

    #[test]
    #[serial_test::serial]
    fn write_normalized_manifest_sweeps_stale_pool_before_new_write() {
        let temp = tempdir().expect("tempdir");
        let ato_home = temp.path().join("ato-home");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let nacelle_dir = capsule_core::common::paths::ato_runs_dir().join("nacelle-manifests");
        fs::create_dir_all(&nacelle_dir).expect("create nacelle manifest dir");
        let stale = nacelle_dir.join("nacelle-123-stale.toml");
        fs::write(&stale, "schema_version = \"0.3\"\n").expect("write stale manifest");
        set_file_mtime(&stale, FileTime::from_unix_time(1, 0)).expect("age stale manifest");

        let plan = plan_from_manifest(
            &temp,
            r#"
            [targets.dev]
            runtime = "source"
            language = "python"
            entrypoint = "main.py"
            "#,
            "dev",
        );

        let normalized_path = write_normalized_manifest(&plan, &[], &[]).expect("write manifest");

        assert!(
            !stale.exists(),
            "stale manifest should be swept before write"
        );
        assert!(normalized_path.exists());
    }

    #[test]
    fn test_write_normalized_manifest_uses_selected_target() {
        let dir = tempdir().unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            language = "python"
            runtime_version = "3.12"
            entrypoint = "main.py"
            cmd = ["--flag", "value"]

            [isolation]
            sandbox = true
            "#,
            "dev",
        );

        let normalized_path = write_normalized_manifest(&plan, &[], &[]).unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();
        let expected_entrypoint = sandbox_source_entrypoint(&plan, "main.py");

        assert!(normalized.contains("entrypoint = \"uv\""));
        assert!(normalized.contains(&format!(
            "command = \"run python3 {expected_entrypoint} --flag value\""
        )));
        assert!(normalized.contains("UV_MANAGED_PYTHON = \"1\""));
        assert!(normalized.contains("UV_PYTHON = \"3.12\""));
        assert!(normalized.contains("language = \"python\""));
        assert!(normalized.contains("version = \"3.12\""));
    }

    #[test]
    fn test_write_normalized_manifest_anchors_single_script_entrypoint_for_cwd_override() {
        let dir = tempdir().unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            language = "python"
            entrypoint = "main.py"
            source_layout = "anchored_entrypoint"
            "#,
            "dev",
        );

        let normalized_path = write_normalized_manifest(&plan, &[], &[]).unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();
        let expected_entrypoint = sandbox_source_entrypoint(&plan, "main.py");

        assert!(normalized.contains("entrypoint = \"uv\""));
        assert!(normalized.contains(&format!("command = \"run python3 {expected_entrypoint}\"")));
    }

    #[test]
    fn test_write_normalized_manifest_respects_runtime_version_for_anchored_python_targets() {
        let dir = tempdir().unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            language = "python"
            runtime_version = "3.11.10"
            entrypoint = "main.py"
            source_layout = "anchored_entrypoint"
            "#,
            "dev",
        );

        let normalized_path = write_normalized_manifest(&plan, &[], &[]).unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();
        let expected_entrypoint = sandbox_source_entrypoint(&plan, "main.py");

        assert!(normalized.contains(&format!("command = \"run python3 {expected_entrypoint}\"")));
        assert!(normalized.contains("UV_MANAGED_PYTHON = \"1\""));
        assert!(normalized.contains("UV_PYTHON = \"3.11.10\""));
    }

    #[test]
    fn test_write_normalized_manifest_omits_python_selector_without_runtime_version() {
        let dir = tempdir().unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            language = "python"
            entrypoint = "main.py"
            source_layout = "anchored_entrypoint"
            "#,
            "dev",
        );

        let normalized_path = write_normalized_manifest(&plan, &[], &[]).unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();

        assert!(!normalized.contains("UV_MANAGED_PYTHON"));
        assert!(!normalized.contains("UV_PYTHON"));
    }

    #[test]
    fn test_write_normalized_manifest_preserves_python_module_invocation() {
        let dir = tempdir().unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            language = "python"
            driver = "python"
            runtime_version = "3.11.10"
            run_command = "python -m uvicorn main:app --host 127.0.0.1 --port 8765"
            "#,
            "dev",
        );

        let normalized_path = write_normalized_manifest(&plan, &[], &[]).unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();

        assert!(normalized.contains("entrypoint = \"uv\""));
        assert!(normalized.contains(
            "command = \"run python3 -m uvicorn main:app --host 127.0.0.1 --port 8765\""
        ));
        assert!(
            !normalized.contains("source/-m") && !normalized.contains("source\\\\-m"),
            "normalized={normalized}"
        );
    }

    /// Sandbox network-policy regression (#17): consumer→provider TCP on
    /// `127.0.0.1:<port>` must survive `--sandbox`. The synthesized manifest
    /// nacelle parses must (a) carry the manifest's top-level `[network]`
    /// section under `[isolation.network]` so user-declared egress hosts
    /// aren't dropped, and (b) include each provider endpoint that the
    /// orchestrator allocated, so the resulting Seatbelt profile permits
    /// the loopback connection rather than emitting a deny.
    #[test]
    fn test_write_normalized_manifest_carries_network_egress_and_dep_endpoints() {
        let dir = tempdir().unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            language = "python"
            driver = "python"
            runtime_version = "3.11.10"
            entrypoint = "main.py"

            [isolation]
            sandbox = true

            [network]
            egress_allow = ["smtp.gmail.com"]
            "#,
            "dev",
        );

        let normalized_path = write_normalized_manifest(
            &plan,
            &[],
            &["127.0.0.1:54321".to_string(), "127.0.0.1:54322".to_string()],
        )
        .unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();
        let parsed: toml::Value = toml::from_str(&normalized).expect("normalized toml");
        let network = parsed
            .get("isolation")
            .and_then(|v| v.get("network"))
            .and_then(|v| v.as_table())
            .expect("[isolation.network] must exist");
        assert_eq!(
            network.get("enabled").and_then(|v| v.as_bool()),
            Some(true),
            "default network.enabled must be true so seatbelt does not deny"
        );
        let egress: Vec<&str> = network
            .get("egress_allow")
            .and_then(|v| v.as_array())
            .map(|values| values.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(
            egress.contains(&"smtp.gmail.com"),
            "manifest [network].egress_allow must round-trip: {egress:?}"
        );
        assert!(
            egress.contains(&"127.0.0.1:54321") && egress.contains(&"127.0.0.1:54322"),
            "dep endpoints must be appended: {egress:?}"
        );
    }

    #[test]
    fn test_adapter_includes_ipc_socket_paths() {
        let dir = tempdir().unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            entrypoint = "main.py"
            "#,
            "dev",
        );
        let launch_ctx = RuntimeLaunchContext::from_ipc(IpcContext {
            env_vars: std::collections::HashMap::from([(
                "CAPSULE_IPC_GREETER_SOCKET".to_string(),
                "/tmp/capsule-ipc/greeter.sock".to_string(),
            )]),
            resolved_count: 1,
            socket_paths: std::collections::HashMap::from([(
                "greeter".to_string(),
                PathBuf::from("/tmp/capsule-ipc/greeter.sock"),
            )]),
            resolved_services: std::collections::HashMap::new(),
            activation_mode: SessionActivationMode::Lazy,
            warnings: vec![],
        });

        let adapter =
            NacelleExecAdapter::for_plan(&plan, ExecuteMode::Foreground, &launch_ctx).unwrap();

        assert_eq!(
            adapter.payload["ipc_socket_paths"][0].as_str(),
            Some("/tmp/capsule-ipc/greeter.sock")
        );
        assert_eq!(
            adapter.payload["ipc_env"][0][0].as_str(),
            Some("CAPSULE_IPC_GREETER_SOCKET")
        );
    }

    #[test]
    fn test_adapter_includes_mounts_and_trailing_args() {
        let dir = tempdir().unwrap();
        let input_path = dir.path().join("input.txt");
        fs::write(&input_path, "hello").unwrap();
        fs::write(dir.path().join("main.py"), "print('ok')\n").unwrap();
        let plan = plan_from_manifest(
            &dir,
            r#"
            name = "demo"
            version = "1.2.3"

            [targets.dev]
            runtime = "source"
            language = "python"
            driver = "python"
            runtime_version = "3.11.10"
            entrypoint = "main.py"
            "#,
            "dev",
        );
        let effective_cwd = dir.path().join("caller");
        let launch_ctx = RuntimeLaunchContext::empty()
            .with_command_args(vec!["--help".to_string()])
            .with_effective_cwd(effective_cwd.clone())
            .with_injected_mounts(vec![crate::executors::launch_context::InjectedMount {
                source: input_path.clone(),
                target: "/workspace/input.txt".to_string(),
                readonly: true,
            }]);

        let adapter =
            NacelleExecAdapter::for_plan(&plan, ExecuteMode::Foreground, &launch_ctx).unwrap();
        let manifest_path = adapter.payload["workload"]["manifest"]
            .as_str()
            .expect("manifest path");
        let normalized = fs::read_to_string(manifest_path).unwrap();
        let expected_entrypoint = sandbox_source_entrypoint(&plan, "main.py");

        assert!(normalized.contains(&format!(
            "command = \"run python3 {expected_entrypoint} --help\""
        )));
        assert_eq!(
            adapter.payload["mounts"][0]["source"].as_str(),
            Some(input_path.to_string_lossy().as_ref())
        );
        assert_eq!(
            adapter.payload["mounts"][0]["target"].as_str(),
            Some("/workspace/input.txt")
        );
        assert_eq!(
            adapter.payload["mounts"][0]["readonly"].as_bool(),
            Some(true)
        );
        assert_eq!(
            adapter.payload["cwd"].as_str(),
            runtime_cwd_payload(&launch_ctx, &plan.execution_working_directory()).as_deref()
        );
        let payload_env = adapter.payload["env"].as_array().expect("payload env");
        assert!(payload_env.iter().any(|pair| {
            pair[0].as_str() == Some("UV_MANAGED_PYTHON") && pair[1].as_str() == Some("1")
        }));
        assert!(payload_env.iter().any(|pair| {
            pair[0].as_str() == Some("UV_PYTHON") && pair[1].as_str() == Some("3.11.10")
        }));
    }

    #[test]
    fn test_nacelle_exec_event_deserialization() {
        let event: LifecycleEvent = serde_json::from_str(
            r#"{"event":"ipc_ready","service":"main","endpoint":"unix:///tmp/main.sock"}"#,
        )
        .unwrap();

        assert_eq!(
            event,
            LifecycleEvent::Ready {
                service: "main".to_string(),
                endpoint: Some("unix:///tmp/main.sock".to_string()),
                port: None,
            }
        );
    }

    #[test]
    fn desktop_native_lock_uses_host_open_on_macos() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("Minimal.app")).expect("bundle dir");

        let plan = plan_from_manifest(
            &dir,
            r#"
            [targets.desktop]
            runtime = "source"
            driver = "native"
            entrypoint = "./Minimal.app/Contents/MacOS/Minimal"
            "#,
            "desktop",
        );

        let mut lock = capsule_core::ato_lock::AtoLock::default();
        lock.contract.entries.insert(
            "delivery".to_string(),
            json!({
                "artifact": {
                    "kind": "desktop-native",
                    "path": "Minimal.app"
                }
            }),
        );

        #[cfg(target_os = "macos")]
        assert!(should_launch_desktop_native_with_host_open(
            &plan,
            Some(&lock)
        ));

        #[cfg(not(target_os = "macos"))]
        assert!(!should_launch_desktop_native_with_host_open(
            &plan,
            Some(&lock)
        ));
    }
}
