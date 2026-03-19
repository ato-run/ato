use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::router::ManifestData;
use capsule_core::types::ServiceSpec;

use crate::runtime_manager;
use crate::runtime_overrides;

use super::launch_context::RuntimeLaunchContext;

const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_INTERVAL: Duration = Duration::from_millis(250);
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
struct RuntimeBins {
    deno: Option<PathBuf>,
    node: Option<PathBuf>,
    python: Option<PathBuf>,
    uv: Option<PathBuf>,
}

struct RunningService {
    spec: ServiceSpec,
    env: HashMap<String, String>,
    child: Child,
    stdout_thread: Option<JoinHandle<std::io::Result<()>>>,
    stderr_thread: Option<JoinHandle<std::io::Result<()>>>,
}

pub fn execute(plan: &ManifestData, launch_ctx: &RuntimeLaunchContext) -> Result<i32> {
    if !plan.is_web_services_mode() {
        return Err(AtoExecutionError::policy_violation(
            "web services executor requires runtime=web driver=deno with top-level [services]",
        )
        .into());
    }

    let services = plan.services();
    if services.is_empty() {
        return Err(AtoExecutionError::policy_violation(
            "top-level [services] must define at least one service",
        )
        .into());
    }
    if !services.contains_key("main") {
        return Err(AtoExecutionError::policy_violation(
            "web/deno services mode requires top-level [services.main]",
        )
        .into());
    }

    let startup_order = service_startup_order(&services)?;
    let runtime_bins = resolve_runtime_bins(plan, &services)?;
    let runtime_dir = resolve_runtime_dir(&plan.manifest_dir);

    let mut running: HashMap<String, RunningService> = HashMap::new();
    let mut ready: HashSet<String> = HashSet::new();

    for service_name in &startup_order {
        let spec = services.get(service_name).ok_or_else(|| {
            AtoExecutionError::policy_violation(format!(
                "services.{} is missing from parsed manifest",
                service_name
            ))
        })?;

        if let Some(depends_on) = spec.depends_on.as_ref() {
            for dep in depends_on {
                wait_until_ready(dep, &mut running, &mut ready)?;
            }
        }

        let env = build_service_env(plan, service_name, spec, launch_ctx)?;
        let mut cmd = build_service_command(&runtime_dir, spec, &runtime_bins)?;
        cmd.current_dir(&runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(&env);

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn service '{}' with command '{}'",
                service_name, spec.entrypoint
            )
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_thread = spawn_prefixed_stream(stdout, service_name, false);
        let stderr_thread = spawn_prefixed_stream(stderr, service_name, true);

        running.insert(
            service_name.clone(),
            RunningService {
                spec: spec.clone(),
                env,
                child,
                stdout_thread: Some(stdout_thread),
                stderr_thread: Some(stderr_thread),
            },
        );
    }

    for service_name in &startup_order {
        wait_until_ready(service_name, &mut running, &mut ready)?;
    }

    monitor_and_shutdown(running)
}

fn resolve_runtime_dir(manifest_dir: &Path) -> PathBuf {
    let source_dir = manifest_dir.join("source");
    if source_dir.is_dir() {
        source_dir
    } else {
        manifest_dir.to_path_buf()
    }
}

fn resolve_runtime_bins(
    plan: &ManifestData,
    services: &HashMap<String, ServiceSpec>,
) -> Result<RuntimeBins> {
    let mut bins = RuntimeBins {
        deno: Some(runtime_manager::ensure_deno_binary(plan)?),
        ..RuntimeBins::default()
    };

    let mut required_tools: HashSet<String> = HashSet::new();
    for service in services.values() {
        if let Some(head) = command_head(&service.entrypoint)? {
            if matches!(head.as_str(), "node" | "python" | "uv" | "deno") {
                required_tools.insert(head);
            }
        }
    }

    if required_tools.contains("node") {
        ensure_runtime_tool_version(plan, "node")?;
        bins.node = Some(runtime_manager::ensure_node_binary(plan)?);
    }
    if required_tools.contains("python") {
        ensure_runtime_tool_version(plan, "python")?;
        bins.python = Some(runtime_manager::ensure_python_binary(plan)?);
    }
    if required_tools.contains("uv") {
        ensure_runtime_tool_version(plan, "uv")?;
        bins.uv = Some(runtime_manager::ensure_uv_binary(plan)?);
    }

    Ok(bins)
}

fn ensure_runtime_tool_version(plan: &ManifestData, tool: &str) -> Result<()> {
    let exists = plan
        .execution_runtime_tool_version(tool)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    Err(AtoExecutionError::policy_violation(format!(
        "targets.{}.runtime_tools.{} is required when services command references '{}'",
        plan.selected_target_label(),
        tool,
        tool
    ))
    .into())
}

fn command_head(command: &str) -> Result<Option<String>> {
    let tokens = shell_words::split(command).map_err(|err| {
        AtoExecutionError::policy_violation(format!(
            "failed to parse services entrypoint '{}': {}",
            command, err
        ))
    })?;
    Ok(tokens
        .first()
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty()))
}

fn build_service_env(
    plan: &ManifestData,
    service_name: &str,
    service: &ServiceSpec,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<HashMap<String, String>> {
    let mut env = runtime_overrides::merged_env(plan.execution_env());
    if let Some(extra) = service.env.as_ref() {
        env.extend(extra.clone());
    }
    if service_name == "main" {
        if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
            env.insert("PORT".to_string(), port.to_string());
        }
    }

    if let Some(ipc_env) = launch_ctx.ipc_env_vars() {
        for (key, value) in ipc_env {
            if key.starts_with("CAPSULE_IPC_") || key == "ATO_BRIDGE_TOKEN" {
                env.insert(key.clone(), value.clone());
                continue;
            }
            return Err(AtoExecutionError::policy_violation(format!(
                "session_token env '{}' is not allowlisted",
                key
            ))
            .into());
        }
    }

    env.extend(launch_ctx.injected_env().clone());

    Ok(env)
}

fn build_service_command(
    runtime_dir: &Path,
    service: &ServiceSpec,
    bins: &RuntimeBins,
) -> Result<Command> {
    let tokens = shell_words::split(service.entrypoint.as_str()).map_err(|err| {
        AtoExecutionError::policy_violation(format!(
            "failed to parse services entrypoint '{}': {}",
            service.entrypoint, err
        ))
    })?;
    if tokens.is_empty() {
        return Err(AtoExecutionError::policy_violation(
            "service entrypoint must include an executable",
        )
        .into());
    }

    let executable = resolve_executable(runtime_dir, &tokens[0], bins)?;
    let mut cmd = Command::new(executable);
    if tokens.len() > 1 {
        cmd.args(&tokens[1..]);
    }
    Ok(cmd)
}

fn resolve_executable(runtime_dir: &Path, token: &str, bins: &RuntimeBins) -> Result<PathBuf> {
    let normalized = token.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "deno" => bins
            .deno
            .clone()
            .ok_or_else(|| {
                AtoExecutionError::runtime_not_resolved(
                    "deno runtime is not resolved",
                    Some("deno"),
                )
            })
            .map_err(Into::into),
        "node" => bins
            .node
            .clone()
            .ok_or_else(|| {
                AtoExecutionError::runtime_not_resolved(
                    "node runtime is not resolved",
                    Some("node"),
                )
            })
            .map_err(Into::into),
        "python" | "python3" => bins
            .python
            .clone()
            .ok_or_else(|| {
                AtoExecutionError::runtime_not_resolved(
                    "python runtime is not resolved",
                    Some("python"),
                )
            })
            .map_err(Into::into),
        "uv" => bins
            .uv
            .clone()
            .ok_or_else(|| {
                AtoExecutionError::runtime_not_resolved("uv runtime is not resolved", Some("uv"))
            })
            .map_err(Into::into),
        _ => {
            let raw = PathBuf::from(token);
            if raw.is_absolute() {
                Ok(raw)
            } else if token.contains('/') || token.contains('\\') {
                Ok(runtime_dir.join(raw))
            } else {
                Ok(PathBuf::from(token))
            }
        }
    }
}

fn spawn_prefixed_stream(
    stream: Option<impl Read + Send + 'static>,
    service_name: &str,
    stderr: bool,
) -> JoinHandle<std::io::Result<()>> {
    let name = service_name.to_string();
    thread::spawn(move || -> std::io::Result<()> {
        let Some(stream) = stream else {
            return Ok(());
        };
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        let prefix = format!("[{}] ", name);
        loop {
            buf.clear();
            let read = reader.read_until(b'\n', &mut buf)?;
            if read == 0 {
                break;
            }
            if stderr {
                let mut writer = std::io::stderr();
                writer.write_all(prefix.as_bytes())?;
                writer.write_all(&buf)?;
                writer.flush()?;
            } else {
                let mut writer = std::io::stdout();
                writer.write_all(prefix.as_bytes())?;
                writer.write_all(&buf)?;
                writer.flush()?;
            }
        }
        Ok(())
    })
}

fn wait_until_ready(
    service_name: &str,
    running: &mut HashMap<String, RunningService>,
    ready: &mut HashSet<String>,
) -> Result<()> {
    if ready.contains(service_name) {
        return Ok(());
    }

    let service = running.get_mut(service_name).ok_or_else(|| {
        AtoExecutionError::execution_contract_invalid(
            format!(
                "service '{}' was not started before readiness check",
                service_name
            ),
            None,
            Some(service_name),
        )
    })?;

    let Some(probe) = service.spec.readiness_probe.as_ref() else {
        ready.insert(service_name.to_string());
        return Ok(());
    };

    let port = resolve_probe_port(&service.env, probe, service_name)?;
    let deadline = Instant::now() + READINESS_TIMEOUT;
    loop {
        if let Some(status) = service.child.try_wait()? {
            let code = status.code().unwrap_or(1);
            return Err(AtoExecutionError::execution_contract_invalid(
                format!(
                    "service '{}' exited before readiness check passed (exit code: {})",
                    service_name, code
                ),
                None,
                Some(service_name),
            )
            .into());
        }

        if readiness_probe_ok(probe, port)? {
            ready.insert(service_name.to_string());
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(AtoExecutionError::execution_contract_invalid(
                format!(
                    "service '{}' readiness check timed out after {}s",
                    service_name,
                    READINESS_TIMEOUT.as_secs()
                ),
                Some("readiness_probe"),
                Some(service_name),
            )
            .into());
        }
        thread::sleep(READINESS_INTERVAL);
    }
}

fn resolve_probe_port(
    env: &HashMap<String, String>,
    probe: &capsule_core::types::ReadinessProbe,
    service_name: &str,
) -> Result<u16> {
    let key = probe.port.trim();
    if key.is_empty() {
        return Err(AtoExecutionError::execution_contract_invalid(
            format!(
                "services.{}.readiness_probe.port must be a non-empty env placeholder",
                service_name
            ),
            Some("services.<name>.readiness_probe.port"),
            Some(service_name),
        )
        .into());
    }
    let value = env.get(key).ok_or_else(|| {
        AtoExecutionError::execution_contract_invalid(
            format!(
                "services.{}.readiness_probe.port '{}' is not defined in service env",
                service_name, key
            ),
            Some("services.<name>.readiness_probe.port"),
            Some(service_name),
        )
    })?;
    value.parse::<u16>().map_err(|_| {
        AtoExecutionError::execution_contract_invalid(
            format!(
                "services.{}.readiness_probe.port '{}' resolved to non-numeric value '{}'",
                service_name, key, value
            ),
            Some("services.<name>.readiness_probe.port"),
            Some(service_name),
        )
        .into()
    })
}

fn readiness_probe_ok(probe: &capsule_core::types::ReadinessProbe, port: u16) -> Result<bool> {
    if let Some(path) = probe
        .http_get
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        return Ok(http_probe(path, port));
    }
    if let Some(target) = probe
        .tcp_connect
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        return Ok(tcp_probe(target, port));
    }
    Err(AtoExecutionError::execution_contract_invalid(
        "readiness_probe must define http_get or tcp_connect",
        Some("readiness_probe"),
        None,
    )
    .into())
}

fn http_probe(path: &str, port: u16) -> bool {
    if path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }

    let normalized_path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };
    let address = format!("127.0.0.1:{}", port);
    let Ok(mut stream) = connect_with_timeout(&address) else {
        return false;
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        normalized_path
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = [0u8; 128];
    let Ok(read) = stream.read(&mut response) else {
        return false;
    };
    if read == 0 {
        return false;
    }
    let head = String::from_utf8_lossy(&response[..read]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    status
        .map(|code| (200..500).contains(&code))
        .unwrap_or(false)
}

fn tcp_probe(target: &str, port: u16) -> bool {
    let address = if target.contains(':') {
        target.to_string()
    } else {
        format!("{}:{}", target, port)
    };
    connect_with_timeout(&address).is_ok()
}

fn connect_with_timeout(address: &str) -> std::io::Result<TcpStream> {
    let mut addrs = address.to_socket_addrs()?;
    let Some(addr) = addrs.next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "no address resolved",
        ));
    };
    TcpStream::connect_timeout(&addr, Duration::from_secs(1))
}

fn monitor_and_shutdown(mut running: HashMap<String, RunningService>) -> Result<i32> {
    loop {
        let mut exited: Option<(String, i32)> = None;
        for (name, service) in &mut running {
            if let Some(status) = service.child.try_wait()? {
                exited = Some((name.clone(), status.code().unwrap_or(1)));
                break;
            }
        }

        if let Some((exited_name, exit_code)) = exited {
            shutdown_remaining(&mut running, &exited_name)?;
            drain_output_threads(&mut running);
            return Ok(exit_code);
        }

        thread::sleep(Duration::from_millis(200));
    }
}

fn shutdown_remaining(
    running: &mut HashMap<String, RunningService>,
    exited_service: &str,
) -> Result<()> {
    for (name, service) in running.iter_mut() {
        if name == exited_service {
            continue;
        }
        let _ = send_sigterm(&mut service.child);
    }

    let deadline = Instant::now() + GRACEFUL_STOP_TIMEOUT;
    loop {
        let mut all_stopped = true;
        for (name, service) in running.iter_mut() {
            if name == exited_service {
                continue;
            }
            if service.child.try_wait()?.is_none() {
                all_stopped = false;
            }
        }
        if all_stopped || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    for (name, service) in running.iter_mut() {
        if name == exited_service {
            continue;
        }
        if service.child.try_wait()?.is_none() {
            let _ = service.child.kill();
            let _ = service.child.wait();
        }
    }

    Ok(())
}

fn drain_output_threads(running: &mut HashMap<String, RunningService>) {
    for service in running.values_mut() {
        if let Some(handle) = service.stdout_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = service.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(unix)]
fn send_sigterm(child: &mut Child) -> Result<()> {
    let ret = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    if ret == 0 {
        return Ok(());
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err.into())
    }
}

#[cfg(not(unix))]
fn send_sigterm(child: &mut Child) -> Result<()> {
    child.kill().map_err(Into::into)
}

fn service_startup_order(services: &HashMap<String, ServiceSpec>) -> Result<Vec<String>> {
    fn visit(
        current: &str,
        services: &HashMap<String, ServiceSpec>,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
        stack: &mut Vec<String>,
        out: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(current) {
            return Ok(());
        }
        if visiting.contains(current) {
            stack.push(current.to_string());
            return Err(AtoExecutionError::policy_violation(format!(
                "services has circular dependency: {}",
                stack.join(" -> ")
            ))
            .into());
        }

        let spec = services.get(current).ok_or_else(|| {
            AtoExecutionError::policy_violation(format!(
                "unknown service '{}' in dependency graph",
                current
            ))
        })?;

        visiting.insert(current.to_string());
        stack.push(current.to_string());
        if let Some(deps) = spec.depends_on.as_ref() {
            for dep in deps {
                if !services.contains_key(dep) {
                    return Err(AtoExecutionError::policy_violation(format!(
                        "services.{}.depends_on references unknown service '{}'",
                        current, dep
                    ))
                    .into());
                }
                visit(dep, services, visited, visiting, stack, out)?;
            }
        }
        stack.pop();
        visiting.remove(current);
        visited.insert(current.to_string());
        out.push(current.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = services.keys().collect();
    names.sort();

    let mut out = Vec::new();
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    for name in names {
        let mut stack = Vec::new();
        visit(
            name,
            services,
            &mut visited,
            &mut visiting,
            &mut stack,
            &mut out,
        )?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::service_startup_order;
    use capsule_core::types::ServiceSpec;
    use std::collections::HashMap;

    #[test]
    fn startup_order_respects_dependencies() {
        let mut services = HashMap::new();
        services.insert(
            "main".to_string(),
            ServiceSpec {
                entrypoint: "node server.js".to_string(),
                target: None,
                depends_on: Some(vec!["api".to_string()]),
                expose: None,
                env: None,
                state_bindings: Vec::new(),
                readiness_probe: None,
                network: None,
            },
        );
        services.insert(
            "api".to_string(),
            ServiceSpec {
                entrypoint: "python api.py".to_string(),
                target: None,
                depends_on: None,
                expose: None,
                env: None,
                state_bindings: Vec::new(),
                readiness_probe: None,
                network: None,
            },
        );

        let order = service_startup_order(&services).unwrap();
        let main_idx = order.iter().position(|v| v == "main").unwrap();
        let api_idx = order.iter().position(|v| v == "api").unwrap();
        assert!(api_idx < main_idx);
    }

    #[test]
    fn startup_order_rejects_cycle() {
        let mut services = HashMap::new();
        services.insert(
            "main".to_string(),
            ServiceSpec {
                entrypoint: "node server.js".to_string(),
                target: None,
                depends_on: Some(vec!["api".to_string()]),
                expose: None,
                env: None,
                state_bindings: Vec::new(),
                readiness_probe: None,
                network: None,
            },
        );
        services.insert(
            "api".to_string(),
            ServiceSpec {
                entrypoint: "python api.py".to_string(),
                target: None,
                depends_on: Some(vec!["main".to_string()]),
                expose: None,
                env: None,
                state_bindings: Vec::new(),
                readiness_probe: None,
                network: None,
            },
        );

        let err = service_startup_order(&services).unwrap_err();
        assert!(err.to_string().contains("circular dependency"));
    }
}
