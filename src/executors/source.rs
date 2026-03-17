use anyhow::{Context, Result};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use tracing::debug;

use capsule_core::runtime::native::NativeHandle;
use capsule_core::{RuntimeMetadata, SessionRunner, SessionRunnerConfig};

use super::launch_context::RuntimeLaunchContext;
use crate::common::proxy;
use crate::reporters::CliReporter;
use crate::runtime_manager;
use crate::runtime_overrides;

use capsule_core::engine;
use capsule_core::r3_config;
use capsule_core::router::ManifestData;

pub struct CapsuleProcess {
    pub child: Child,
    pub cleanup_paths: Vec<PathBuf>,
    pub event_rx: Option<Receiver<NacelleExecEvent>>,
    pub workload_pid: Option<u32>,
    pub log_path: Option<PathBuf>,
}

#[derive(Clone, Copy)]
pub enum ExecuteMode {
    Foreground,
    Background,
    Piped,
}

pub fn execute(
    plan: &ManifestData,
    nacelle_override: Option<PathBuf>,
    _reporter: std::sync::Arc<CliReporter>,
    enforcement: &str,
    mode: ExecuteMode,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<CapsuleProcess> {
    let nacelle = engine::discover_nacelle(engine::EngineRequest {
        explicit_path: nacelle_override,
        manifest_path: Some(plan.manifest_path.clone()),
    })?;

    r3_config::generate_and_write_config(
        &plan.manifest_path,
        Some(enforcement.to_string()),
        false,
    )?;

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
    _reporter: std::sync::Arc<CliReporter>,
    mode: ExecuteMode,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<CapsuleProcess> {
    let entrypoint = plan
        .execution_entrypoint()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            capsule_core::execution_plan::error::AtoExecutionError::policy_violation(
                "source/native target requires entrypoint",
            )
        })?;

    let runtime_dir = resolve_runtime_dir(&plan.manifest_dir, &entrypoint);
    let entrypoint_path = if Path::new(&entrypoint).is_absolute() {
        PathBuf::from(&entrypoint)
    } else {
        runtime_dir.join(&entrypoint)
    };
    let force_python_no_bytecode = is_python_entrypoint(plan, &entrypoint);
    let injected_port =
        runtime_overrides::override_port(plan.execution_port()).map(|port| port.to_string());

    let mut cmd = if force_python_no_bytecode {
        let python_bin = runtime_manager::ensure_python_binary(plan)?;
        let mut python = Command::new(python_bin);
        python.arg(&entrypoint);
        python
    } else {
        Command::new(entrypoint_path)
    };

    cmd.current_dir(&runtime_dir);
    if let Some(proxy_env) = proxy::proxy_env_from_env(&[])? {
        proxy::apply_proxy_env(&mut cmd, &proxy_env);
    }
    apply_python_runtime_hardening(&mut cmd, force_python_no_bytecode);

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    if let Some(port) = injected_port {
        cmd.env("PORT", port);
    }
    launch_ctx.apply_allowlisted_env(&mut cmd)?;

    let args = plan.targets_oci_cmd();
    if !args.is_empty() {
        cmd.args(args);
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

    Ok(CapsuleProcess {
        child,
        cleanup_paths: Vec::new(),
        event_rx: None,
        workload_pid: None,
        log_path: None,
    })
}

fn resolve_runtime_dir(manifest_dir: &Path, entrypoint: &str) -> PathBuf {
    let source_dir = manifest_dir.join("source");
    if source_dir.is_dir() && source_dir.join(entrypoint).exists() {
        return source_dir;
    }
    manifest_dir.to_path_buf()
}

fn is_python_entrypoint(plan: &ManifestData, entrypoint: &str) -> bool {
    if plan
        .execution_driver()
        .map(|driver| driver.trim().eq_ignore_ascii_case("python"))
        .unwrap_or(false)
    {
        return true;
    }

    entrypoint.trim().to_ascii_lowercase().ends_with(".py")
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

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NacelleExecEvent {
    IpcReady {
        service: String,
        endpoint: String,
        #[serde(default)]
        port: Option<u16>,
    },
    ServiceExited {
        service: String,
        #[serde(default)]
        exit_code: Option<i32>,
    },
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
    let command = (!cmd_args.is_empty()).then(|| cmd_args.join(" "));

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
    execution.insert("entrypoint".to_string(), toml::Value::String(entrypoint));
    if let Some(command) = command {
        execution.insert("command".to_string(), toml::Value::String(command));
    }
    manifest.insert("execution".to_string(), toml::Value::Table(execution));

    if let Some(isolation) = plan.manifest.get("isolation").cloned() {
        manifest.insert("isolation".to_string(), isolation);
    }

    let language_name = plan
        .execution_language()
        .or_else(|| plan.execution_driver())
        .or_else(|| plan.execution_runtime());
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

fn spawn_internal_exec(
    nacelle: &Path,
    manifest_dir: &Path,
    payload: &serde_json::Value,
    mode: ExecuteMode,
) -> Result<(Child, Receiver<NacelleExecEvent>, NacelleExecMeta)> {
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
            match serde_json::from_str::<NacelleExecEvent>(trimmed) {
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
        let manifest_path = dir.path().join("capsule.toml");
        let plan = ManifestData {
            manifest: toml::from_str(
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
            )
            .unwrap(),
            manifest_path,
            manifest_dir: dir.path().to_path_buf(),
            profile: capsule_core::router::ExecutionProfile::Dev,
            selected_target: "dev".to_string(),
            state_source_overrides: HashMap::new(),
        };

        let normalized_path = write_normalized_manifest(&plan).unwrap();
        let normalized = fs::read_to_string(&normalized_path).unwrap();

        assert!(normalized.contains("entrypoint = \"main.py\""));
        assert!(normalized.contains("command = \"--flag value\""));
        assert!(normalized.contains("language = \"python\""));
        assert!(normalized.contains("version = \"3.12\""));
    }

    #[test]
    fn test_adapter_includes_ipc_socket_paths() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        let plan = ManifestData {
            manifest: toml::from_str(
                r#"
                name = "demo"
                version = "1.2.3"

                [targets.dev]
                runtime = "source"
                entrypoint = "main.py"
                "#,
            )
            .unwrap(),
            manifest_path,
            manifest_dir: dir.path().to_path_buf(),
            profile: capsule_core::router::ExecutionProfile::Dev,
            selected_target: "dev".to_string(),
            state_source_overrides: HashMap::new(),
        };
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
        let event: NacelleExecEvent = serde_json::from_str(
            r#"{"event":"ipc_ready","service":"main","endpoint":"unix:///tmp/main.sock"}"#,
        )
        .unwrap();

        assert_eq!(
            event,
            NacelleExecEvent::IpcReady {
                service: "main".to_string(),
                endpoint: "unix:///tmp/main.sock".to_string(),
                port: None,
            }
        );
    }
}
