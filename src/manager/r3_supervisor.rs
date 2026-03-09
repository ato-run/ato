use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::config::{RuntimeConfig, ServiceConfig};
use crate::internal_api::NacelleEvent;
use crate::lockfile;
use crate::system::NetworkSandbox;

pub async fn run_services_from_config(
    config: &RuntimeConfig,
    bundle_root: &Path,
    sandbox: Option<&dyn NetworkSandbox>,
    strict_sandbox_required: bool,
) -> Result<(), String> {
    run_services_from_config_with_events(
        config,
        bundle_root,
        sandbox,
        strict_sandbox_required,
        None,
    )
    .await
}

pub async fn run_services_from_config_with_events(
    config: &RuntimeConfig,
    bundle_root: &Path,
    sandbox: Option<&dyn NetworkSandbox>,
    strict_sandbox_required: bool,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<NacelleEvent>>,
) -> Result<(), String> {
    if strict_sandbox_required && sandbox.is_none() {
        let msg = if cfg!(any(target_os = "macos", target_os = "windows")) {
            "Strict sandbox enforcement is enabled but sandbox backend is not available. Hint: If you trust this code, rerun via ato-cli with --unsafe-bypass-sandbox"
                .to_string()
        } else {
            "Strict sandbox enforcement is enabled but sandbox backend is not available"
                .to_string()
        };
        return Err(msg);
    }

    lockfile::enforce_lockfile_allowlist(bundle_root)
        .map_err(|e| format!("capsule.lock allowlist check failed: {}", e))?;
    lockfile::hydrate_bundle(bundle_root)
        .await
        .map_err(|e| format!("capsule.lock hydration failed: {}", e))?;
    if config.services.is_empty() {
        return Err("config.json has no services".to_string());
    }
    if !config.services.contains_key("main") {
        return Err("config.json requires services.main".to_string());
    }

    let order = resolve_dependencies(&config.services)?;
    let mut children: HashMap<String, Child> = HashMap::new();

    for name in &order {
        let svc = config
            .services
            .get(name)
            .ok_or_else(|| format!("Service '{}' missing from config", name))?;

        let mut cmd = build_command(bundle_root, svc)?;
        if let Some(sandbox) = sandbox {
            sandbox
                .apply_to_child(&mut cmd)
                .map_err(|e| format!("Failed to apply sandbox: {e}"))?;
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn service '{}': {}", name, e))?;

        if let Some(health_check) = &svc.health_check {
            wait_for_service_ready(&mut child, name, health_check, event_tx.as_ref()).await?;
        }

        children.insert(name.clone(), child);
    }

    supervise_children(children, event_tx.as_ref()).await
}

async fn supervise_children(
    mut children: HashMap<String, Child>,
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<NacelleEvent>>,
) -> Result<(), String> {
    loop {
        let mut exited: Option<(String, std::process::ExitStatus)> = None;
        let mut wait_error: Option<(String, String)> = None;

        for (name, child) in children.iter_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited = Some((name.clone(), status));
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    wait_error = Some((name.clone(), e.to_string()));
                    break;
                }
            }
        }

        if let Some((name, err)) = wait_error {
            terminate_all(&mut children, &name);
            return Err(format!("Service '{}' wait failed: {}", name, err));
        }

        if let Some((name, status)) = exited {
            let is_main = name == "main";
            emit_event(
                event_tx,
                NacelleEvent::ServiceExited {
                    service: name.clone(),
                    exit_code: status.code(),
                },
            );
            terminate_all(&mut children, &name);

            if is_main {
                if !status.success() {
                    return Err(format!("services.main exited with status {}", status));
                }
                return Ok(());
            }

            return Err(format!("Service '{}' exited (fail-fast): {}", name, status));
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_service_ready(
    child: &mut Child,
    service: &str,
    health_check: &crate::config::HealthCheck,
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<NacelleEvent>>,
) -> Result<(), String> {
    use tokio::time::{sleep, Instant};

    let timeout = Duration::from_secs(u64::from(health_check.timeout_secs.unwrap_or(30)));
    let interval = Duration::from_secs(u64::from(health_check.interval_secs.unwrap_or(1).max(1)));
    let deadline = Instant::now() + timeout;
    let port: u16 = health_check.port.trim().parse().map_err(|_| {
        format!(
            "Invalid health_check.port for '{service}': {}",
            health_check.port
        )
    })?;

    loop {
        if Instant::now() > deadline {
            return Err(format!("Health check timed out for '{service}'"));
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                emit_event(
                    event_tx,
                    NacelleEvent::ServiceExited {
                        service: service.to_string(),
                        exit_code: status.code(),
                    },
                );
                return Err(format!(
                    "Service '{service}' exited before readiness (code: {:?})",
                    status.code()
                ));
            }
            Ok(None) => {}
            Err(err) => {
                return Err(format!("Service '{service}' wait failed: {err}"));
            }
        }

        if health_check_ready(health_check, port).await {
            let (endpoint, port) = readiness_endpoint(health_check, port);
            emit_event(
                event_tx,
                NacelleEvent::IpcReady {
                    service: service.to_string(),
                    endpoint,
                    port,
                },
            );
            return Ok(());
        }

        sleep(interval).await;
    }
}

async fn health_check_ready(health_check: &crate::config::HealthCheck, port: u16) -> bool {
    if let Some(http_get) = &health_check.http_get {
        return readiness_http_ok(http_get, port).await;
    }

    let host = health_check
        .tcp_connect
        .as_deref()
        .unwrap_or("127.0.0.1")
        .trim();
    readiness_tcp_ok(host, port).await
}

fn readiness_endpoint(
    health_check: &crate::config::HealthCheck,
    port: u16,
) -> (String, Option<u16>) {
    let host = health_check
        .tcp_connect
        .as_deref()
        .unwrap_or("127.0.0.1")
        .trim();
    if host.contains(':') {
        (format!("tcp://{}", host), Some(port))
    } else {
        (format!("tcp://{}:{}", host, port), Some(port))
    }
}

fn emit_event(
    event_tx: Option<&tokio::sync::mpsc::UnboundedSender<NacelleEvent>>,
    event: NacelleEvent,
) {
    if let Some(tx) = event_tx {
        let _ = tx.send(event);
    }
}

async fn readiness_tcp_ok(host: &str, port: u16) -> bool {
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    let addr = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };

    timeout(Duration::from_secs(1), TcpStream::connect(addr))
        .await
        .ok()
        .and_then(|result| result.ok())
        .is_some()
}

async fn readiness_http_ok(http_get: &str, port: u16) -> bool {
    use tokio::time::timeout;

    let url = if http_get.starts_with("http://") || http_get.starts_with("https://") {
        http_get.to_string()
    } else if http_get.starts_with('/') {
        format!("http://127.0.0.1:{port}{http_get}")
    } else {
        format!("http://127.0.0.1:{port}/{http_get}")
    };

    let client = reqwest::Client::new();
    let fut = async {
        let resp = client.get(url).send().await.ok()?;
        Some(resp.status().is_success())
    };

    timeout(Duration::from_secs(2), fut)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

fn terminate_all(children: &mut HashMap<String, Child>, exclude: &str) {
    for (name, child) in children.iter_mut() {
        if name == exclude {
            continue;
        }
        let _ = child.kill();
    }
}

fn build_command(bundle_root: &Path, svc: &ServiceConfig) -> Result<Command, String> {
    let cwd = svc.cwd.as_deref().unwrap_or("source");
    let cwd_path = resolve_path(bundle_root, cwd);

    let executable = resolve_path_with_cwd(bundle_root, cwd_path.clone(), &svc.executable);
    let mut cmd = Command::new(&executable);

    let args: Vec<String> = svc
        .args
        .iter()
        .map(|a| resolve_arg(bundle_root, a))
        .collect();
    cmd.args(&args);

    cmd.current_dir(cwd_path);

    if let Some(envs) = &svc.env {
        for (key, value) in envs {
            cmd.env(key, resolve_env_value(bundle_root, value));
        }
    }
    if let Some(ports) = &svc.ports {
        for (k, v) in ports {
            cmd.env(k, v.to_string());
        }
    }

    Ok(cmd)
}

fn resolve_path_with_cwd(bundle_root: &Path, cwd: PathBuf, path: &str) -> PathBuf {
    let trimmed = path.trim();

    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }

    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return bundle_root.join(trimmed);
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

    bundle_root.join(trimmed)
}

fn resolve_path(bundle_root: &Path, path: &str) -> PathBuf {
    let trimmed = path.trim();

    // If the config provides an absolute path, trust it.
    if trimmed.starts_with('/') {
        return PathBuf::from(trimmed);
    }

    // If the config provides a bare executable name (e.g. "bash"), resolve it from PATH
    // so that host-provided runtimes work in bundle mode.
    if !trimmed.contains('/') {
        if let Ok(found) = which::which(trimmed) {
            return found;
        }
    }

    // Otherwise, treat it as a path relative to the extracted bundle root.
    bundle_root.join(trimmed)
}

fn resolve_arg(bundle_root: &Path, arg: &str) -> String {
    let trimmed = arg.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return resolve_path(bundle_root, trimmed)
            .to_string_lossy()
            .to_string();
    }
    if trimmed.starts_with('/') {
        return trimmed.to_string();
    }
    trimmed.to_string()
}

fn resolve_env_value(bundle_root: &Path, value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with("source/") || trimmed.starts_with("runtime/") {
        return resolve_path(bundle_root, trimmed)
            .to_string_lossy()
            .to_string();
    }
    trimmed.to_string()
}

fn resolve_dependencies(services: &HashMap<String, ServiceConfig>) -> Result<Vec<String>, String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut visiting: HashSet<String> = HashSet::new();
    let mut sorted: Vec<String> = Vec::new();

    fn visit(
        name: &str,
        services: &HashMap<String, ServiceConfig>,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        sorted: &mut Vec<String>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if visited.contains(name) {
            return Ok(());
        }
        if visiting.contains(name) {
            stack.push(name.to_string());
            return Err(format!(
                "Circular dependency detected: {}",
                stack.join(" -> ")
            ));
        }

        let spec = services
            .get(name)
            .ok_or_else(|| format!("Unknown service '{}' (referenced by depends_on)", name))?;

        visiting.insert(name.to_string());
        stack.push(name.to_string());

        if let Some(deps) = &spec.depends_on {
            for dep in deps {
                if !services.contains_key(dep) {
                    return Err(format!(
                        "Service '{}' depends on unknown service '{}'",
                        name, dep
                    ));
                }
                visit(dep, services, visited, visiting, sorted, stack)?;
            }
        }

        stack.pop();
        visiting.remove(name);
        visited.insert(name.to_string());
        sorted.push(name.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = services.keys().collect();
    names.sort();
    for name in names {
        let mut stack = Vec::new();
        visit(
            name,
            services,
            &mut visited,
            &mut visiting,
            &mut sorted,
            &mut stack,
        )?;
    }

    Ok(sorted)
}
