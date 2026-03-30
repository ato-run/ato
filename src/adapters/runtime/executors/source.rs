use anyhow::{Context, Result};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;
use tracing::debug;

use capsule_core::runtime::native::NativeHandle;
use capsule_core::{RuntimeMetadata, SessionRunner, SessionRunnerConfig};

use super::launch_context::RuntimeLaunchContext;
use crate::application::workspace::state::EffectiveLockState;
use crate::common::proxy;
use crate::reporters::CliReporter;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;

use capsule_core::engine;
use capsule_core::isolation::HostIsolationContext;
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::lifecycle::LifecycleEvent;
use capsule_core::lock_runtime;
use capsule_core::r3_config;
use capsule_core::router::ManifestData;

pub struct CapsuleProcess {
    pub child: Child,
    pub cleanup_paths: Vec<PathBuf>,
    pub event_rx: Option<Receiver<LifecycleEvent>>,
    pub workload_pid: Option<u32>,
    pub log_path: Option<PathBuf>,
}

#[derive(Clone, Copy)]
pub enum ExecuteMode {
    Foreground,
    Background,
    Piped,
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
        let config = r3_config::generate_config_from_lock(
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
        r3_config::write_config(&plan.manifest_path, &config)?;
    } else {
        r3_config::generate_and_write_config(
            &plan.manifest_path,
            Some(enforcement.to_string()),
            false,
        )?;
    }

    let adapter = NacelleExecAdapter::for_plan(plan, mode, launch_ctx)?;
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
    let launch_spec = derive_launch_spec(plan)?;
    let force_python_no_bytecode =
        is_python_launch_spec(plan, &launch_spec.command, launch_spec.language.as_deref());
    let force_node_runtime =
        is_node_launch_spec(plan, &launch_spec.command, launch_spec.language.as_deref());
    let injected_port =
        runtime_overrides::override_port(launch_spec.port).map(|port| port.to_string());
    let readiness_port = runtime_overrides::override_port(launch_spec.port);

    let mut cmd = if force_python_no_bytecode {
        let python_bin = resolve_host_managed_runtime_binary(
            plan,
            authoritative_lock,
            ManagedRuntimeKind::Python,
        )?;
        let mut python = Command::new(python_bin);
        python.arg(&launch_spec.command);
        python
    } else if force_node_runtime {
        let node_bin = resolve_host_managed_runtime_binary(
            plan,
            authoritative_lock,
            ManagedRuntimeKind::Node,
        )?;
        let mut node = Command::new(node_bin);
        node.arg(&launch_spec.command);
        node
    } else {
        Command::new(resolve_host_command_path(
            &launch_spec.working_dir,
            &launch_spec.command,
        ))
    };

    cmd.current_dir(&launch_spec.working_dir);
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
    }

    let child = cmd
        .spawn()
        .context("Failed to execute host process with --dangerously-skip-permissions")?;
    let event_rx = Some(spawn_host_lifecycle_events(child.id(), readiness_port));

    Ok(CapsuleProcess {
        child,
        cleanup_paths: Vec::new(),
        event_rx,
        workload_pid: None,
        log_path: None,
    })
}

fn apply_host_isolation(
    cmd: &mut Command,
    plan: &ManifestData,
    launch_env: &std::collections::HashMap<String, String>,
    launch_port: Option<u16>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<()> {
    let isolation_root = plan.manifest_dir.join(".tmp");
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
    if authoritative_lock.is_none() {
        return match runtime {
            ManagedRuntimeKind::Node => runtime_manager::ensure_node_binary(plan),
            ManagedRuntimeKind::Python => runtime_manager::ensure_python_binary(plan),
        };
    }

    let candidates: &[&str] = match runtime {
        ManagedRuntimeKind::Node => {
            if cfg!(windows) {
                &["node.exe", "node"]
            } else {
                &["node"]
            }
        }
        ManagedRuntimeKind::Python => {
            if cfg!(windows) {
                &["python.exe", "python"]
            } else {
                &["python3", "python"]
            }
        }
    };

    find_command_on_path(candidates).ok_or_else(|| {
        let runtime_name = match runtime {
            ManagedRuntimeKind::Node => "node",
            ManagedRuntimeKind::Python => "python",
        };
        anyhow::anyhow!(
            "lock-derived source execution requires a host-local '{}' runtime on PATH",
            runtime_name
        )
    })
}

fn find_command_on_path(candidates: &[&str]) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let path_exts = executable_extensions();

    for directory in env::split_paths(&path) {
        for candidate in candidates {
            let direct = directory.join(candidate);
            if direct.is_file() {
                return Some(direct);
            }
            for extension in &path_exts {
                let with_extension = directory.join(format!("{}{}", candidate, extension));
                if with_extension.is_file() {
                    return Some(with_extension);
                }
            }
        }
    }

    None
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
        mode: ExecuteMode,
        launch_ctx: &RuntimeLaunchContext,
    ) -> Result<Self> {
        let normalized_manifest_path = write_normalized_manifest(plan)?;
        let mut env = runtime_overrides::merged_env(plan.execution_env())
            .into_iter()
            .collect::<Vec<_>>();
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

        Ok(Self {
            payload: json!({
                "spec_version": "0.1.0",
                "interactive": matches!(mode, ExecuteMode::Foreground),
                "workload": {
                    "type": "source",
                    "manifest": normalized_manifest_path.display().to_string(),
                },
                "env": env,
                "ipc_env": ipc_env,
                "ipc_socket_paths": ipc_socket_paths,
            }),
            cleanup_paths: vec![normalized_manifest_path],
        })
    }
}

fn write_normalized_manifest(plan: &ManifestData) -> Result<PathBuf> {
    let entrypoint = plan
        .execution_entrypoint()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            capsule_core::execution_plan::error::AtoExecutionError::policy_violation(
                "source/native target requires entrypoint",
            )
        })?;
    let cmd_args = plan.targets_oci_cmd();
    let language_name = plan
        .execution_language()
        .or_else(|| plan.execution_driver())
        .or_else(|| plan.execution_runtime());
    let is_python = language_name
        .as_deref()
        .map(|value| value.eq_ignore_ascii_case("python"))
        .unwrap_or(false);

    let (normalized_entrypoint, command) = if is_python {
        let mut tokens = vec!["run".to_string()];
        if has_local_uv_cache(plan) {
            tokens.push("--offline".to_string());
        }
        if let Some(requirements) = resolve_python_requirements_argument(plan) {
            tokens.push("--with-requirements".to_string());
            tokens.push(requirements);
        }
        tokens.push("python3".to_string());
        tokens.push(entrypoint.clone());
        tokens.extend(cmd_args.iter().cloned());
        (
            "uv".to_string(),
            Some(shell_words::join(tokens.iter().map(String::as_str))),
        )
    } else {
        let command = (!cmd_args.is_empty()).then(|| cmd_args.join(" "));
        (entrypoint.clone(), command)
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
    manifest.insert("execution".to_string(), toml::Value::Table(execution));

    if let Some(isolation) = plan.manifest.get("isolation").cloned() {
        manifest.insert("isolation".to_string(), isolation);
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

    let path = plan.manifest_dir.join(format!(
        ".ato-nacelle-{}.toml",
        rand::thread_rng().gen::<u64>()
    ));
    fs::write(&path, toml::to_string(&toml::Value::Table(manifest))?)
        .with_context(|| format!("Failed to write normalized manifest: {}", path.display()))?;
    Ok(path)
}

fn resolve_python_requirements_argument(plan: &ManifestData) -> Option<String> {
    let working_dir = plan.execution_working_directory();
    let candidates = [
        working_dir.join("requirements.txt"),
        plan.manifest_dir.join("requirements.txt"),
        plan.manifest_dir.join("source").join("requirements.txt"),
    ];
    candidates
        .into_iter()
        .find(|path| path.exists())
        .map(|path| {
            pathdiff::diff_paths(&path, &plan.manifest_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string()
        })
}

fn has_local_uv_cache(plan: &ManifestData) -> bool {
    let roots = [
        plan.execution_working_directory(),
        plan.manifest_dir.clone(),
        plan.manifest_dir.join("source"),
    ];
    roots.into_iter().any(|root| {
        let artifacts_dir = root.join("artifacts");
        fs::read_dir(&artifacts_dir)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.filter_map(Result::ok))
            .any(|entry| entry.path().join("uv-cache").is_dir())
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
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .context("Failed to read nacelle exec response")?;
    if read == 0 || line.trim().is_empty() {
        anyhow::bail!("nacelle exec returned an empty initial response");
    }

    let response: NacelleExecResponse = serde_json::from_str(line.trim())
        .with_context(|| format!("Failed to parse nacelle exec response: {}", line.trim()))?;
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
    thread::spawn(move || {
        for maybe_line in reader.lines() {
            let Ok(line) = maybe_line else {
                break;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<LifecycleEvent>(trimmed) {
                Ok(event) => {
                    let _ = event_tx.send(event);
                }
                Err(_) => {
                    debug!(event = trimmed, "nacelle exec event");
                }
            }
        }
    });

    Ok((child, event_rx, exec_meta))
}

fn spawn_host_lifecycle_events(pid: u32, readiness_port: Option<u16>) -> Receiver<LifecycleEvent> {
    let (event_tx, event_rx) = mpsc::channel();
    let ready_tx = event_tx.clone();
    thread::spawn(move || {
        if let Some(port) = readiness_port {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
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
    let _ = pid;

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
    use std::collections::HashMap;
    use tempfile::tempdir;

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
    fn test_apply_python_runtime_hardening_sets_env() {
        let mut cmd = Command::new("echo");
        apply_python_runtime_hardening(&mut cmd, true);

        let value = cmd
            .get_envs()
            .find_map(|(key, value)| {
                if key == "PYTHONDONTWRITEBYTECODE" {
                    value.map(|v| v.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .expect("PYTHONDONTWRITEBYTECODE must be set");

        assert_eq!(value, "1");
    }

    #[test]
    fn test_apply_python_runtime_hardening_noop_when_disabled() {
        let mut cmd = Command::new("echo");
        apply_python_runtime_hardening(&mut cmd, false);

        let has_var = cmd
            .get_envs()
            .any(|(key, _)| key == "PYTHONDONTWRITEBYTECODE");

        assert!(!has_var, "PYTHONDONTWRITEBYTECODE must not be set");
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

        let normalized_path = write_normalized_manifest(&plan).unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();

        assert!(normalized.contains("entrypoint = \"uv\""));
        assert!(normalized.contains("command = \"run python3 main.py --flag value\""));
        assert!(normalized.contains("language = \"python\""));
        assert!(normalized.contains("version = \"3.12\""));
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
}
