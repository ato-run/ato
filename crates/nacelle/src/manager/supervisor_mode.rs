use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::capsule_types::capsule_v1::ServiceSpec;

pub type PortRegistry = HashMap<String, HashMap<String, u16>>;

#[derive(Debug, Clone)]
pub struct SupervisorModePlan {
    pub startup_order: Vec<String>,
    pub ports: PortRegistry,
    pub services: HashMap<String, ServiceSpec>,
}

#[derive(Debug, Clone)]
pub enum LogStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub service: String,
    pub stream: LogStream,
    pub line: String,
}

#[derive(Debug, Clone)]
pub struct SupervisorModeRunOptions {
    pub readiness_timeout: Duration,
    pub readiness_interval: Duration,
    pub shutdown_grace: Duration,
    pub kill_wait: Duration,
}

impl Default for SupervisorModeRunOptions {
    fn default() -> Self {
        Self {
            readiness_timeout: Duration::from_secs(30),
            readiness_interval: Duration::from_millis(200),
            shutdown_grace: Duration::from_secs(2),
            kill_wait: Duration::from_millis(250),
        }
    }
}

pub fn build_supervisor_mode_plan(
    services: &HashMap<String, ServiceSpec>,
) -> Result<SupervisorModePlan, String> {
    let startup_order = resolve_dependencies(services)?;

    // Step 2: Port Registry & Injection
    let ports = allocate_ports(services)?;
    let services = inject_all_services(services, &ports)?;

    Ok(SupervisorModePlan {
        startup_order,
        ports,
        services,
    })
}

/// Step 3: Spawn + readiness + supervision (fail-fast).
///
/// - Spawns services in `startup_order`
/// - Attaches stdout/stderr readers and prefixes lines with `[service]`
/// - Waits for readiness probes before continuing
/// - Exits when any service exits (fail-fast) or on Ctrl-C/SIGTERM, and tears down all children
pub async fn run_supervisor_mode(
    plan: &SupervisorModePlan,
    working_dir: &Path,
    options: SupervisorModeRunOptions,
    log_tx: Option<tokio::sync::mpsc::Sender<LogLine>>,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::sync::mpsc;

    #[cfg(unix)]
    let (mut sig_term, mut sig_int) = {
        use tokio::signal::unix::{signal, SignalKind};
        let sig_term = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
        let sig_int = signal(SignalKind::interrupt()).map_err(|e| e.to_string())?;
        (sig_term, sig_int)
    };

    let (exit_tx, mut exit_rx) = mpsc::unbounded_channel::<(String, std::process::ExitStatus)>();
    let mut running: Vec<(String, u32)> = Vec::new(); // (service, pid)
    let mut wait_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    for svc_name in &plan.startup_order {
        let spec = plan
            .services
            .get(svc_name)
            .ok_or_else(|| format!("Service '{}' missing from plan.services", svc_name))?;

        let mut cmd = build_shell_command(&spec.entrypoint);
        cmd.current_dir(working_dir);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        // Propagate env from manifest.
        if let Some(env) = &spec.env {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        // Also inject exposed ports as env vars.
        if let Some(port_map) = plan.ports.get(svc_name) {
            for (k, port) in port_map {
                cmd.env(k, port.to_string());
            }
        }

        // Put each service in its own process group (so we can kill the whole subtree).
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn service '{svc_name}': {e}"))?;

        let pid = child
            .id()
            .ok_or_else(|| format!("Failed to get PID for service '{svc_name}'"))?;
        running.push((svc_name.clone(), pid));

        if let Some(stdout) = child.stdout.take() {
            let svc = svc_name.clone();
            let tx = log_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    emit_log(tx.as_ref(), &svc, LogStream::Stdout, line).await;
                }
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let svc = svc_name.clone();
            let tx = log_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    emit_log(tx.as_ref(), &svc, LogStream::Stderr, line).await;
                }
            });
        }

        // Spawn wait task for exit detection (fail-fast).
        {
            let svc = svc_name.clone();
            let tx = exit_tx.clone();
            let handle = tokio::spawn(async move {
                if let Ok(status) = child.wait().await {
                    let _ = tx.send((svc, status));
                }
            });
            wait_tasks.push(handle);
        }

        // Readiness check (if configured)
        if let Some(probe) = &spec.readiness_probe {
            wait_for_readiness(
                probe,
                svc_name,
                options.readiness_timeout,
                options.readiness_interval,
            )
            .await?;
        }
    }

    // Supervision: wait for any service exit or shutdown signal.
    #[cfg(unix)]
    let reason: ShutdownReason = tokio::select! {
        msg = exit_rx.recv() => {
            if let Some((svc, status)) = msg {
                ShutdownReason::ServiceExited { service: svc, status }
            } else {
                ShutdownReason::Signal
            }
        },
        _ = tokio::signal::ctrl_c() => ShutdownReason::Signal,
        _ = sig_term.recv() => ShutdownReason::Signal,
        _ = sig_int.recv() => ShutdownReason::Signal,
    };

    #[cfg(not(unix))]
    let reason: ShutdownReason = tokio::select! {
        msg = exit_rx.recv() => {
            if let Some((svc, status)) = msg {
                ShutdownReason::ServiceExited { service: svc, status }
            } else {
                ShutdownReason::Signal
            }
        },
        _ = tokio::signal::ctrl_c() => ShutdownReason::Signal,
    };

    // Teardown all services.
    shutdown_all(&running, options.shutdown_grace, options.kill_wait).await;

    // Best-effort: wait for children to be reaped.
    for handle in wait_tasks {
        let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
    }

    match reason {
        ShutdownReason::Signal => Ok(()),
        ShutdownReason::ServiceExited { service, status } => Err(format!(
            "Service '{service}' exited (code: {:?})",
            status.code()
        )),
    }
}

#[derive(Debug, Clone)]
enum ShutdownReason {
    Signal,
    ServiceExited {
        service: String,
        status: std::process::ExitStatus,
    },
}

fn build_shell_command(command: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("cmd.exe");
        cmd.arg("/C");
        cmd.raw_arg(command);
        return cmd;
    }

    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c");
        cmd.arg(command);
        cmd
    }
}

async fn emit_log(
    tx: Option<&tokio::sync::mpsc::Sender<LogLine>>,
    service: &str,
    stream: LogStream,
    line: String,
) {
    if let Some(tx) = tx {
        let _ = tx
            .send(LogLine {
                service: service.to_string(),
                stream: stream.clone(),
                line,
            })
            .await;
        return;
    }

    // Default interactive behavior: prefix lines for human consumption.
    match stream {
        LogStream::Stdout => println!("[{service}] {line}"),
        LogStream::Stderr => eprintln!("[{service}] {line}"),
    }
}

async fn wait_for_readiness(
    probe: &crate::capsule_types::capsule_v1::ReadinessProbe,
    service: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<(), String> {
    use tokio::time::{sleep, Instant};

    let deadline = Instant::now() + timeout;
    let port: u16 = probe.port.trim().parse().map_err(|_| {
        format!(
            "Invalid readiness_probe.port for '{service}': {}",
            probe.port
        )
    })?;

    loop {
        if Instant::now() > deadline {
            return Err(format!("Readiness probe timed out for '{service}'"));
        }

        // Prefer http_get if configured.
        if let Some(http_get) = &probe.http_get {
            if readiness_http_ok(http_get, port).await {
                return Ok(());
            }
        } else {
            // Default: TCP connect
            let host = probe.tcp_connect.as_deref().unwrap_or("127.0.0.1").trim();

            if readiness_tcp_ok(host, port).await {
                return Ok(());
            }
        }

        sleep(interval).await;
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
        .and_then(|r| r.ok())
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

async fn shutdown_all(running: &[(String, u32)], grace: Duration, kill_wait: Duration) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{self as nix_signal, Signal};
        use nix::unistd::Pid;
        use tokio::time::sleep;

        // SIGTERM all process groups
        for (_svc, pid) in running {
            let pgid = Pid::from_raw(*pid as i32);
            let _ = nix_signal::killpg(pgid, Signal::SIGTERM);
        }

        sleep(grace).await;

        // SIGKILL all process groups
        for (_svc, pid) in running {
            let pgid = Pid::from_raw(*pid as i32);
            let _ = nix_signal::killpg(pgid, Signal::SIGKILL);
        }

        sleep(kill_wait).await;
    }

    #[cfg(not(unix))]
    {
        let _ = (running, grace, kill_wait);
    }
}

/// Resolve `depends_on` into a deterministic startup order.
///
/// Semantics (Step 1):
/// - Unknown dependency => error
/// - Cycle => error
/// - Order is topologically sorted (dependency comes before dependent)
pub fn resolve_dependencies(
    services: &HashMap<String, ServiceSpec>,
) -> Result<Vec<String>, String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut visiting: HashSet<String> = HashSet::new();
    let mut sorted: Vec<String> = Vec::new();

    fn visit(
        name: &str,
        services: &HashMap<String, ServiceSpec>,
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

    // Deterministic iteration over root services.
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

/// Allocate all exposed ports for all services (Bind-Get-Release).
pub fn allocate_ports(services: &HashMap<String, ServiceSpec>) -> Result<PortRegistry, String> {
    let mut registry: PortRegistry = HashMap::new();

    // Deterministic iteration makes behavior stable in tests/logs.
    let mut service_names: Vec<&String> = services.keys().collect();
    service_names.sort();

    for svc_name in service_names {
        let spec = services
            .get(svc_name)
            .ok_or_else(|| format!("Unknown service '{}'", svc_name))?;

        let mut svc_ports: HashMap<String, u16> = HashMap::new();
        if let Some(expose_list) = &spec.expose {
            for port_name in expose_list {
                let port_num = get_free_port().map_err(|e| e.to_string())?;
                svc_ports.insert(port_name.clone(), port_num);
            }
        }
        registry.insert(svc_name.clone(), svc_ports);
    }

    Ok(registry)
}

fn get_free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
    // listener dropped here
}

pub fn inject_all_services(
    services: &HashMap<String, ServiceSpec>,
    ports: &PortRegistry,
) -> Result<HashMap<String, ServiceSpec>, String> {
    let mut out = HashMap::new();

    // Deterministic order for stable outputs.
    let mut names: Vec<&String> = services.keys().collect();
    names.sort();

    for name in names {
        let spec = services
            .get(name)
            .ok_or_else(|| format!("Unknown service '{}'", name))?;
        let injected = inject_service_spec(spec, name, ports)?;
        out.insert(name.clone(), injected);
    }

    Ok(out)
}

pub fn inject_service_spec(
    spec: &ServiceSpec,
    my_name: &str,
    ports: &PortRegistry,
) -> Result<ServiceSpec, String> {
    let mut new_spec = spec.clone();

    new_spec.entrypoint = replace_placeholders(&new_spec.entrypoint, my_name, ports)?;

    if let Some(env) = &new_spec.env {
        let mut new_env = HashMap::new();
        for (k, v) in env {
            new_env.insert(k.clone(), replace_placeholders(v, my_name, ports)?);
        }
        new_spec.env = Some(new_env);
    }

    if let Some(probe) = &new_spec.readiness_probe {
        let mut probe = probe.clone();
        if let Some(http_get) = &probe.http_get {
            probe.http_get = Some(replace_placeholders(http_get, my_name, ports)?);
        }
        if let Some(tcp_connect) = &probe.tcp_connect {
            probe.tcp_connect = Some(replace_placeholders(tcp_connect, my_name, ports)?);
        }
        probe.port = resolve_port_field(&probe.port, my_name, ports)?;
        new_spec.readiness_probe = Some(probe);
    }

    Ok(new_spec)
}

fn resolve_port_field(port: &str, my_name: &str, ports: &PortRegistry) -> Result<String, String> {
    let trimmed = port.trim();

    // If it's a templated string, resolve templates first.
    if trimmed.contains("{{") {
        return replace_placeholders(trimmed, my_name, ports);
    }

    // If it's already a number, keep it.
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Ok(trimmed.to_string());
    }

    // Otherwise interpret as a local placeholder name (e.g. "PORT").
    let port_num = lookup_local_port(my_name, trimmed, ports)?;
    Ok(port_num.to_string())
}

fn replace_placeholders(
    input: &str,
    my_name: &str,
    ports: &PortRegistry,
) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("{{") {
        let (before, after_start) = rest.split_at(start);
        out.push_str(before);

        let after_start = &after_start[2..]; // skip "{{"
        let end = after_start
            .find("}}")
            .ok_or_else(|| format!("Unclosed placeholder in: {input}"))?;

        let (raw_key, after_end) = after_start.split_at(end);
        let key = raw_key.trim();

        let replacement = resolve_placeholder(key, my_name, ports)?;
        out.push_str(&replacement);

        rest = &after_end[2..]; // skip "}}"
    }

    out.push_str(rest);
    Ok(out)
}

fn resolve_placeholder(key: &str, my_name: &str, ports: &PortRegistry) -> Result<String, String> {
    // Cross-service form: services.<name>.ports.<KEY>
    if let Some(stripped) = key.strip_prefix("services.") {
        let mut parts = stripped.split('.');
        let svc = parts
            .next()
            .ok_or_else(|| format!("Invalid placeholder '{{{{{key}}}}}'"))?;
        let kind = parts
            .next()
            .ok_or_else(|| format!("Invalid placeholder '{{{{{key}}}}}'"))?;
        if kind != "ports" {
            return Err(format!(
                "Invalid placeholder '{{{{{key}}}}}': expected 'ports'"
            ));
        }
        let port_name = parts
            .next()
            .ok_or_else(|| format!("Invalid placeholder '{{{{{key}}}}}'"))?;
        if parts.next().is_some() {
            return Err(format!("Invalid placeholder '{{{{{key}}}}}'"));
        }

        let svc_ports = ports
            .get(svc)
            .ok_or_else(|| format!("Unknown service '{svc}' in placeholder '{{{{{key}}}}}'"))?;
        let port_num = svc_ports.get(port_name).ok_or_else(|| {
            format!(
                "Unknown port name '{port_name}' for service '{svc}' in placeholder '{{{{{key}}}}}'"
            )
        })?;
        return Ok(port_num.to_string());
    }

    // Local form: {{PORT}} / {{ADMIN_PORT}} (defaults to own exposed ports)
    let port_num = lookup_local_port(my_name, key, ports)?;
    Ok(port_num.to_string())
}

fn lookup_local_port(my_name: &str, port_name: &str, ports: &PortRegistry) -> Result<u16, String> {
    let svc_ports = ports.get(my_name).ok_or_else(|| {
        format!("No ports allocated for service '{my_name}' (needed for placeholder '{port_name}')")
    })?;
    let port_num = svc_ports.get(port_name).ok_or_else(|| {
        format!("Service '{my_name}' has no exposed port named '{port_name}' (add to expose=...)")
    })?;
    Ok(*port_num)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(entrypoint: &str, depends_on: &[&str]) -> ServiceSpec {
        ServiceSpec {
            entrypoint: entrypoint.to_string(),
            depends_on: if depends_on.is_empty() {
                None
            } else {
                Some(depends_on.iter().map(|s| s.to_string()).collect())
            },
            expose: None,
            env: None,
            readiness_probe: None,
        }
    }

    fn svc_with_expose(entrypoint: &str, expose: &[&str]) -> ServiceSpec {
        ServiceSpec {
            entrypoint: entrypoint.to_string(),
            depends_on: None,
            expose: if expose.is_empty() {
                None
            } else {
                Some(expose.iter().map(|s| s.to_string()).collect())
            },
            env: None,
            readiness_probe: None,
        }
    }

    #[test]
    fn topo_sort_orders_dependencies_first() {
        let mut services = HashMap::new();
        services.insert("b".to_string(), svc("/bin/echo b", &[]));
        services.insert("a".to_string(), svc("/bin/echo a", &["b"]));

        let order = resolve_dependencies(&services).unwrap();
        assert_eq!(order, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn topo_sort_is_deterministic_for_independent_services() {
        let mut services = HashMap::new();
        services.insert("web".to_string(), svc("node ui.js", &[]));
        services.insert("llm".to_string(), svc("python server.py", &[]));

        let order = resolve_dependencies(&services).unwrap();
        assert_eq!(order, vec!["llm".to_string(), "web".to_string()]);
    }

    #[test]
    fn unknown_dependency_is_error() {
        let mut services = HashMap::new();
        services.insert("a".to_string(), svc("/bin/echo a", &["missing"]));

        let err = resolve_dependencies(&services).unwrap_err();
        assert!(err.contains("depends on unknown service"));
    }

    #[test]
    fn cycle_is_error() {
        let mut services = HashMap::new();
        services.insert("a".to_string(), svc("/bin/echo a", &["b"]));
        services.insert("b".to_string(), svc("/bin/echo b", &["a"]));

        let err = resolve_dependencies(&services).unwrap_err();
        assert!(err.contains("Circular dependency detected"));
    }

    #[test]
    fn allocate_ports_assigns_ports_for_exposed_names() {
        let mut services = HashMap::new();
        services.insert(
            "llm".to_string(),
            svc_with_expose("python server.py --port {{PORT}}", &["PORT"]),
        );
        services.insert(
            "web".to_string(),
            svc_with_expose("node ui.js --port {{WEB_PORT}}", &["WEB_PORT"]),
        );

        let ports = allocate_ports(&services).unwrap();
        assert!(ports.get("llm").unwrap().get("PORT").unwrap() > &0);
        assert!(ports.get("web").unwrap().get("WEB_PORT").unwrap() > &0);
    }

    #[test]
    fn inject_replaces_local_and_cross_service_placeholders() {
        let mut services = HashMap::new();

        let llm = svc_with_expose("python server.py --port {{PORT}}", &["PORT"]);
        let mut web = svc_with_expose("node ui.js", &["WEB_PORT"]);
        web.depends_on = Some(vec!["llm".to_string()]);
        web.env = Some(HashMap::from([(
            "API_URL".to_string(),
            "http://localhost:{{services.llm.ports.PORT}}".to_string(),
        )]));

        services.insert("llm".to_string(), llm);
        services.insert("web".to_string(), web);

        let ports: PortRegistry = HashMap::from([
            (
                "llm".to_string(),
                HashMap::from([("PORT".to_string(), 54321)]),
            ),
            (
                "web".to_string(),
                HashMap::from([("WEB_PORT".to_string(), 40000)]),
            ),
        ]);

        let injected = inject_all_services(&services, &ports).unwrap();

        assert_eq!(
            injected.get("llm").unwrap().entrypoint,
            "python server.py --port 54321"
        );
        assert_eq!(
            injected
                .get("web")
                .unwrap()
                .env
                .as_ref()
                .unwrap()
                .get("API_URL")
                .unwrap(),
            "http://localhost:54321"
        );
    }

    #[test]
    fn inject_errors_on_unknown_placeholder() {
        let services = HashMap::from([(
            "a".to_string(),
            svc_with_expose("echo {{MISSING}}", &["PORT"]),
        )]);
        let ports: PortRegistry = HashMap::from([(
            "a".to_string(),
            HashMap::from([("PORT".to_string(), 12345)]),
        )]);

        let err = inject_all_services(&services, &ports).unwrap_err();
        assert!(err.contains("has no exposed port named"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_supervisor_mode_fails_fast_when_any_service_exits() {
        let mut services = HashMap::new();

        // Long-running service (will be torn down when the other exits).
        let a = svc("echo a-start; sleep 5", &[]);
        // Short-lived service.
        let mut b = svc("exit 0", &["a"]);
        // ensure it depends on a via depends_on
        b.depends_on = Some(vec!["a".to_string()]);

        services.insert("a".to_string(), a);
        services.insert("b".to_string(), b);

        let plan = build_supervisor_mode_plan(&services).unwrap();

        let options = SupervisorModeRunOptions {
            shutdown_grace: Duration::from_millis(100),
            kill_wait: Duration::from_millis(50),
            readiness_timeout: Duration::from_secs(1),
            readiness_interval: Duration::from_millis(50),
        };

        let res = tokio::time::timeout(
            Duration::from_secs(2),
            run_supervisor_mode(&plan, Path::new("/"), options, None),
        )
        .await;

        assert!(res.is_ok(), "runner should not hang");
        let err = res.unwrap().unwrap_err();
        assert!(err.contains("exited"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn readiness_tcp_probe_succeeds_when_port_is_open() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Minimal plan just to exercise readiness waiting.
        let mut services = HashMap::new();
        let mut s = svc("sleep 1", &[]);
        s.readiness_probe = Some(crate::capsule_types::capsule_v1::ReadinessProbe {
            http_get: None,
            tcp_connect: Some("127.0.0.1".to_string()),
            port: port.to_string(),
        });
        services.insert("svc".to_string(), s);

        let plan = build_supervisor_mode_plan(&services).unwrap();
        let options = SupervisorModeRunOptions {
            readiness_timeout: Duration::from_millis(500),
            readiness_interval: Duration::from_millis(20),
            shutdown_grace: Duration::from_millis(50),
            kill_wait: Duration::from_millis(20),
        };

        // Keep listener alive while readiness check happens.
        let _guard = listener;

        // Expect fail-fast because service 'svc' exits after sleep, but readiness should succeed.
        let err = run_supervisor_mode(&plan, Path::new("/"), options, None)
            .await
            .unwrap_err();
        assert!(err.contains("svc"));
    }
}
