use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::CapsuleError;

const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 2000;
const PORT_RETRY_TIMEOUT: Duration = Duration::from_secs(30);
const PORT_RETRY_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeOptions {
    pub startup_timeout_ms: u64,
    pub check_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeSummary {
    pub startup_timeout_ms: u64,
    pub required_port: Option<u16>,
    pub checked_commands: usize,
}

#[derive(Debug, Clone)]
struct MainService {
    executable: String,
    args: Vec<String>,
    cwd: String,
    env: HashMap<String, String>,
    ports: HashMap<String, u16>,
    health_port: Option<String>,
}

pub fn run_capsule_smoke(
    capsule_path: &Path,
    target_label: &str,
) -> Result<SmokeSummary, CapsuleError> {
    let extract_dir = tempfile::tempdir().map_err(CapsuleError::Io)?;
    extract_capsule(capsule_path, extract_dir.path())?;

    let manifest_path = extract_dir.path().join("capsule.toml");
    let manifest = std::fs::read_to_string(&manifest_path).map_err(CapsuleError::Io)?;
    let manifest_raw: toml::Value = toml::from_str(&manifest)
        .map_err(|e| CapsuleError::Pack(format!("manifest parse failed: {e}")))?;

    let options = parse_smoke_options(&manifest_raw, target_label)?;
    let service = load_main_service(extract_dir.path())?;
    let required_port = resolve_required_port(&manifest_raw, target_label, &service)?;
    ensure_required_port_is_free_before_start(required_port)?;

    let mut child = spawn_main_service(extract_dir.path(), &service)?;
    let startup_timeout = Duration::from_millis(options.startup_timeout_ms);
    let deadline = Instant::now() + startup_timeout;

    loop {
        if let Some(status) = child.try_wait().map_err(CapsuleError::Io)? {
            if status.success() && required_port.is_none() && options.check_commands.is_empty() {
                return Ok(SmokeSummary {
                    startup_timeout_ms: options.startup_timeout_ms,
                    required_port,
                    checked_commands: options.check_commands.len(),
                });
            }
            return Err(CapsuleError::Pack(format!(
                "Smoke failed: process exited before startup timeout (status: {status})"
            )));
        }

        if Instant::now() >= deadline {
            break;
        }

        std::thread::sleep(Duration::from_millis(100));
    }

    if let Some(port) = required_port {
        wait_for_required_port_with_retry(&mut child, port, PORT_RETRY_TIMEOUT)?;
    }

    run_check_commands(extract_dir.path(), &service, &options)?;
    kill_child(&mut child)?;

    Ok(SmokeSummary {
        startup_timeout_ms: options.startup_timeout_ms,
        required_port,
        checked_commands: options.check_commands.len(),
    })
}

fn extract_capsule(capsule_path: &Path, out_dir: &Path) -> Result<(), CapsuleError> {
    let mut archive = std::fs::File::open(capsule_path).map_err(CapsuleError::Io)?;
    let mut outer = tar::Archive::new(&mut archive);
    outer.unpack(out_dir).map_err(CapsuleError::Io)?;

    crate::capsule_v3::unpack_payload_from_capsule_root(out_dir, out_dir)?;

    Ok(())
}

fn parse_smoke_options(
    manifest: &toml::Value,
    target_label: &str,
) -> Result<SmokeOptions, CapsuleError> {
    let target = manifest
        .get("targets")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(target_label))
        .and_then(|v| v.as_table())
        .ok_or_else(|| {
            CapsuleError::Pack(format!("targets.{target_label} is missing in manifest"))
        })?;

    let mut startup_timeout_ms = DEFAULT_STARTUP_TIMEOUT_MS;
    let mut check_commands = Vec::new();

    if let Some(smoke) = target.get("smoke") {
        let smoke = smoke.as_table().ok_or_else(|| {
            CapsuleError::Pack(format!("targets.{target_label}.smoke must be a table"))
        })?;

        if let Some(timeout) = smoke.get("startup_timeout_ms") {
            let timeout = timeout.as_integer().ok_or_else(|| {
                CapsuleError::Pack(format!(
                    "targets.{target_label}.smoke.startup_timeout_ms must be an integer"
                ))
            })?;
            if timeout <= 0 {
                return Err(CapsuleError::Pack(format!(
                    "targets.{target_label}.smoke.startup_timeout_ms must be greater than 0"
                )));
            }
            startup_timeout_ms = timeout as u64;
        }

        if let Some(commands) = smoke.get("check_commands") {
            let commands = commands.as_array().ok_or_else(|| {
                CapsuleError::Pack(format!(
                    "targets.{target_label}.smoke.check_commands must be an array"
                ))
            })?;
            for (idx, cmd) in commands.iter().enumerate() {
                let cmd = cmd.as_str().ok_or_else(|| {
                    CapsuleError::Pack(format!(
                        "targets.{target_label}.smoke.check_commands[{idx}] must be a string"
                    ))
                })?;
                if cmd.trim().is_empty() {
                    return Err(CapsuleError::Pack(format!(
                        "targets.{target_label}.smoke.check_commands[{idx}] must not be empty"
                    )));
                }
                check_commands.push(cmd.to_string());
            }
        }
    }

    Ok(SmokeOptions {
        startup_timeout_ms,
        check_commands,
    })
}

fn load_main_service(extract_dir: &Path) -> Result<MainService, CapsuleError> {
    let config_path = extract_dir.join("config.json");
    let raw = std::fs::read_to_string(&config_path).map_err(CapsuleError::Io)?;
    let json: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| CapsuleError::Pack(format!("config.json parse failed: {e}")))?;

    let main = json
        .get("services")
        .and_then(|v| v.get("main"))
        .and_then(|v| v.as_object())
        .ok_or_else(|| CapsuleError::Pack("config.json requires services.main".to_string()))?;

    let executable = main
        .get("executable")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .ok_or_else(|| CapsuleError::Pack("services.main.executable is required".to_string()))?;

    let args = main
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let cwd = main
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("source")
        .to_string();

    let env = main
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let ports = main
        .get("ports")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_u64().map(|p| (k.clone(), p as u16)))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let health_port = main
        .get("health_check")
        .and_then(|v| v.get("port"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(MainService {
        executable,
        args,
        cwd,
        env,
        ports,
        health_port,
    })
}

fn resolve_required_port(
    manifest: &toml::Value,
    target_label: &str,
    service: &MainService,
) -> Result<Option<u16>, CapsuleError> {
    let target_port = manifest
        .get("targets")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(target_label))
        .and_then(|v| v.as_table())
        .and_then(|target| target.get("port"))
        .map(|v| {
            v.as_integer().ok_or_else(|| {
                CapsuleError::Pack(format!("targets.{target_label}.port must be an integer"))
            })
        })
        .transpose()?;

    if let Some(port) = target_port {
        if !(1..=65535).contains(&port) {
            return Err(CapsuleError::Pack(format!(
                "targets.{target_label}.port must be between 1 and 65535"
            )));
        }
        return Ok(Some(port as u16));
    }

    if let Some(port) = service.health_port.as_deref() {
        if let Ok(num) = port.parse::<u16>() {
            if num == 0 {
                return Err(CapsuleError::Pack(
                    "services.main.health_check.port must be > 0".to_string(),
                ));
            }
            return Ok(Some(num));
        }
        if let Some(num) = service.ports.get(port) {
            return Ok(Some(*num));
        }
        return Err(CapsuleError::Pack(format!(
            "services.main.health_check.port '{port}' is not numeric and not found in services.main.ports"
        )));
    }

    Ok(None)
}

fn spawn_main_service(root: &Path, service: &MainService) -> Result<Child, CapsuleError> {
    let cwd_path = resolve_path(root, &service.cwd);
    let executable = resolve_path_with_cwd(root, &cwd_path, &service.executable);
    let mut cmd = Command::new(&executable);
    let args = service
        .args
        .iter()
        .map(|a| resolve_arg(root, a))
        .collect::<Vec<_>>();
    cmd.args(args);
    cmd.current_dir(&cwd_path);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    for (k, v) in &service.env {
        cmd.env(k, resolve_env_value(root, v));
    }
    for (k, v) in &service.ports {
        cmd.env(k, v.to_string());
    }

    cmd.spawn().map_err(|e| {
        CapsuleError::Pack(format!(
            "failed to start process '{}' for smoke: {e}",
            executable.display()
        ))
    })
}

fn run_check_commands(
    root: &Path,
    service: &MainService,
    options: &SmokeOptions,
) -> Result<(), CapsuleError> {
    if options.check_commands.is_empty() {
        return Ok(());
    }

    let cwd_path = resolve_path(root, &service.cwd);
    for command in &options.check_commands {
        let parts = shell_words::split(command).map_err(|e| {
            CapsuleError::Pack(format!(
                "invalid smoke.check_commands entry '{command}': {e}"
            ))
        })?;
        if parts.is_empty() {
            return Err(CapsuleError::Pack(
                "smoke command must not be empty".to_string(),
            ));
        }

        let mut cmd = Command::new(&parts[0]);
        cmd.args(parts.iter().skip(1));
        cmd.current_dir(&cwd_path);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        for (k, v) in &service.env {
            cmd.env(k, resolve_env_value(root, v));
        }
        for (k, v) in &service.ports {
            cmd.env(k, v.to_string());
        }

        let status = cmd.status().map_err(|e| {
            CapsuleError::Pack(format!("failed to execute smoke command '{command}': {e}"))
        })?;
        if !status.success() {
            return Err(CapsuleError::Pack(format!(
                "smoke command failed (status {status}): {command}"
            )));
        }
    }

    Ok(())
}

fn kill_child(child: &mut Child) -> Result<(), CapsuleError> {
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn can_connect_localhost(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok()
}

fn ensure_required_port_is_free_before_start(
    required_port: Option<u16>,
) -> Result<(), CapsuleError> {
    let Some(port) = required_port else {
        return Ok(());
    };
    if can_connect_localhost(port) {
        return Err(CapsuleError::Pack(format!(
            "Smoke failed: required port {port} is already in use before launch; stop the existing process and retry"
        )));
    }
    Ok(())
}

fn wait_for_required_port_with_retry(
    child: &mut Child,
    port: u16,
    timeout: Duration,
) -> Result<(), CapsuleError> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(CapsuleError::Io)? {
            return Err(CapsuleError::Pack(format!(
                "Smoke failed: process exited while waiting for port {port} (status: {status})"
            )));
        }

        if can_connect_localhost(port) {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(CapsuleError::Pack(format!(
                "Port {port} did not open within {} seconds. Check logs.",
                timeout.as_secs()
            )));
        }

        std::thread::sleep(PORT_RETRY_INTERVAL);
    }
}

fn resolve_path(root: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }
    if !trimmed.contains('/') {
        if let Ok(found) = which::which(trimmed) {
            return found;
        }
    }
    root.join(trimmed)
}

fn resolve_path_with_cwd(root: &Path, cwd: &Path, raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return root.join(trimmed);
    }
    if trimmed.starts_with("./") {
        return cwd.join(trimmed.trim_start_matches("./"));
    }
    if !trimmed.contains('/') {
        let with_cwd = cwd.join(trimmed);
        if with_cwd.exists() {
            return with_cwd;
        }
        if let Ok(found) = which::which(trimmed) {
            return found;
        }
    }
    root.join(trimmed)
}

fn resolve_arg(root: &Path, arg: &str) -> String {
    let trimmed = arg.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return root.join(trimmed).to_string_lossy().to_string();
    }
    trimmed.to_string()
}

fn resolve_env_value(root: &Path, raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return root.join(trimmed).to_string_lossy().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_smoke_defaults() {
        let manifest: toml::Value = toml::from_str(
            r#"
[targets.cli]
runtime = "source"
entrypoint = "main.py"
"#,
        )
        .unwrap();
        let opts = parse_smoke_options(&manifest, "cli").unwrap();
        assert_eq!(opts.startup_timeout_ms, DEFAULT_STARTUP_TIMEOUT_MS);
        assert!(opts.check_commands.is_empty());
    }

    #[test]
    fn parse_smoke_invalid_timeout() {
        let manifest: toml::Value = toml::from_str(
            r#"
[targets.cli]
runtime = "source"
entrypoint = "main.py"

[targets.cli.smoke]
startup_timeout_ms = 0
"#,
        )
        .unwrap();
        let err = parse_smoke_options(&manifest, "cli").unwrap_err();
        assert!(err.to_string().contains("startup_timeout_ms"));
    }

    #[test]
    fn reject_required_port_already_in_use_before_start() {
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
            return;
        };
        let port = listener.local_addr().unwrap().port();

        let err = ensure_required_port_is_free_before_start(Some(port)).unwrap_err();
        assert!(err.to_string().contains("already in use"));
    }
}
