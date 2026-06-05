use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;

use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::installed_state::{InstalledStateDb, os_port_is_free};
use capsule_core::router::ManifestData;
use capsule_core::types::ServiceSpec;

use crate::adapters::runtime::port_admission::{
    self, DEFAULT_PORT_CONFLICT_POLICY, PortAdmissionPlan,
};
use crate::application::pipeline::cleanup::{CleanupScope, PipelineAttemptContext};
use crate::application::services::{
    ServiceGraphPlan, ServicePhaseCoordinator, ServicePhaseRuntime,
};
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;

use super::launch_context::RuntimeLaunchContext;

const READINESS_INTERVAL: Duration = Duration::from_millis(250);
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Protocol for web-service port claims. Web services bind TCP listeners; the
/// claim ledger scopes conflicts per-protocol (#515).
const PORT_ADMISSION_PROTOCOL: &str = "tcp";

#[derive(Debug, Default, Clone)]
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

#[derive(Default)]
struct ServiceStartupState {
    running: HashMap<String, RunningService>,
    ready: HashSet<String>,
}

#[derive(Clone)]
struct ServiceStartupRuntime {
    plan: ManifestData,
    launch_ctx: RuntimeLaunchContext,
    services: HashMap<String, ServiceSpec>,
    runtime_dir: PathBuf,
    runtime_bins: RuntimeBins,
    state: Arc<Mutex<ServiceStartupState>>,
    startup_cleanup: Arc<Mutex<Option<CleanupScope>>>,
}

impl ServiceStartupRuntime {
    fn new(
        plan: ManifestData,
        launch_ctx: RuntimeLaunchContext,
        services: &HashMap<String, ServiceSpec>,
        runtime_dir: PathBuf,
        runtime_bins: RuntimeBins,
        startup_cleanup: Option<CleanupScope>,
    ) -> Self {
        Self {
            plan,
            launch_ctx,
            services: services.clone(),
            runtime_dir,
            runtime_bins,
            state: Arc::new(Mutex::new(ServiceStartupState::default())),
            startup_cleanup: Arc::new(Mutex::new(startup_cleanup)),
        }
    }

    fn commit_startup_cleanup(&self) {
        let scope = self
            .startup_cleanup
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(scope) = scope {
            scope.commit_all();
        }
    }

    fn into_running(self) -> HashMap<String, RunningService> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut state.running)
    }

    /// Compute the installed-app port admission plan for `service_name` from the
    /// already-resolved service env (its `PORT`), returning the open DB handle
    /// alongside the plan so the claim can be recorded after a successful spawn.
    ///
    /// `Ok(None)` when admission does not apply: this is not an installed-app
    /// launch, the service is not `main`, there is no preferred port, or the
    /// installed-state DB is unavailable (admission is best-effort and never
    /// blocks a launch on infrastructure failure). A genuine, unresolvable port
    /// conflict still surfaces as `Err` with the typed `ATO_ERR_PORT_CONFLICT`.
    fn plan_port_admission(
        &self,
        service_name: &str,
        env: &HashMap<String, String>,
    ) -> Result<Option<(InstalledStateDb, PortAdmissionPlan)>> {
        if self.launch_ctx.install_profile_key().is_none() {
            return Ok(None);
        }
        let db = match InstalledStateDb::open_default() {
            Ok(db) => db,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "installed-state DB unavailable; skipping port admission"
                );
                return Ok(None);
            }
        };
        let preferred = env.get("PORT").and_then(|port| port.parse::<u16>().ok());
        let plan = plan_service_port_admission(
            &db,
            &self.launch_ctx,
            service_name,
            preferred,
            os_port_is_free,
        )?;
        Ok(plan.map(|plan| (db, plan)))
    }
}

/// Compute a port admission plan for a web service, gated to the `main`
/// service (the only one whose `PORT` ato injects). Delegates the conflict
/// decision to [`port_admission::plan_port_admission_with`] with the default
/// (Remap) policy. The `os_available` probe is injectable for tests.
fn plan_service_port_admission(
    db: &InstalledStateDb,
    launch_ctx: &RuntimeLaunchContext,
    service_name: &str,
    preferred: Option<u16>,
    os_available: impl Fn(u16) -> bool,
) -> Result<Option<PortAdmissionPlan>> {
    if service_name != "main" {
        return Ok(None);
    }
    port_admission::plan_port_admission_with(
        db,
        launch_ctx.install_profile_key(),
        service_name,
        PORT_ADMISSION_PROTOCOL,
        preferred,
        DEFAULT_PORT_CONFLICT_POLICY,
        os_available,
    )
}

/// Override the service env's `PORT` with the admission-resolved port. Mirrors
/// exactly what `start_service` applies before spawning.
fn apply_port_admission(env: &mut HashMap<String, String>, plan: &PortAdmissionPlan) {
    env.insert("PORT".to_string(), plan.resolved_port.to_string());
}

#[async_trait]
impl ServicePhaseRuntime for ServiceStartupRuntime {
    async fn start_service(&self, service_name: &str) -> Result<()> {
        let spec = self.services.get(service_name).ok_or_else(|| {
            AtoExecutionError::policy_violation(format!(
                "services.{} is missing from parsed manifest",
                service_name
            ))
        })?;

        let mut env = build_service_env(&self.plan, service_name, spec, &self.launch_ctx)?;

        // Installed-app port admission (#508): for the `main` service of an
        // installed app, reconcile the resolved `PORT` against the per-install
        // port-claim ledger before binding. A port held by a *different*
        // installed endpoint is remapped (default policy) so two installed apps
        // that both prefer the same port don't collide. The claim is recorded
        // only after a successful spawn (below) — never here, since the launch
        // may still fail. `ato run` / non-installed launches are untouched.
        let port_admission = self.plan_port_admission(service_name, &env)?;
        if let Some((_, plan)) = &port_admission {
            apply_port_admission(&mut env, plan);
        }

        let mut cmd = build_service_command(&self.runtime_dir, spec, &self.runtime_bins)?;
        cmd.current_dir(&self.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(&env);

        // Spawn the consumer/service into its own process group on
        // Unix so a parent SIGKILL doesn't strand it as a PID-1
        // orphan still bound to the consumer port. The session-start
        // sweep can then `kill(-pid, ...)` the whole tree (uvicorn +
        // any forked workers) when it sees a stale sentinel pointing
        // at the consumer's recorded pid. See ato-run/ato#121.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn service '{}' with command '{}'",
                service_name, spec.entrypoint
            )
        })?;

        // The service spawned successfully; persist the port claim so future
        // relaunches and other installed apps observe the reservation. Recording
        // is best-effort and must not fail an already-running service.
        if let Some((db, plan)) = &port_admission {
            port_admission::record_port_admission_plan(db, plan);
        }

        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_thread = spawn_prefixed_stream(stdout, service_name, false);
        let stderr_thread = spawn_prefixed_stream(stderr, service_name, true);

        if let Some(scope) = self
            .startup_cleanup
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_mut()
        {
            scope.register_kill_child_process(pid, service_name.to_string());
        }

        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .running
            .insert(
                service_name.to_string(),
                RunningService {
                    spec: spec.clone(),
                    env,
                    child,
                    stdout_thread: Some(stdout_thread),
                    stderr_thread: Some(stderr_thread),
                },
            );

        Ok(())
    }

    async fn await_readiness(&self, service_name: String) -> Result<()> {
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || wait_until_ready_in_state(&service_name, &state))
            .await
            .map_err(anyhow::Error::new)?
    }
}

pub fn execute(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<i32> {
    if !plan.is_web_services_mode() {
        return Err(AtoExecutionError::policy_violation(
            "web services executor requires runtime=web driver=deno with top-level [services]",
        )
        .into());
    }

    let graph = ServiceGraphPlan::from_manifest(plan)?;
    let runtime_bins = resolve_runtime_bins(plan, graph.services())?;
    let runtime_dir = resolve_runtime_dir(&plan.manifest_dir);
    let runtime = ServiceStartupRuntime::new(
        plan.clone(),
        launch_ctx.clone(),
        graph.services(),
        runtime_dir,
        runtime_bins,
        attempt.map(|attempt| attempt.cleanup_scope()),
    );

    run_startup_coordinator(&graph, runtime.clone())?;
    runtime.commit_startup_cleanup();

    monitor_and_shutdown(runtime.into_running())
}

fn run_startup_coordinator(graph: &ServiceGraphPlan, runtime: ServiceStartupRuntime) -> Result<()> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| {
            handle.block_on(ServicePhaseCoordinator::new(graph).run(runtime))
        })
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(ServicePhaseCoordinator::new(graph).run(runtime))
    }
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
        if let Some(head) = command_head(&service.entrypoint)?
            && matches!(head.as_str(), "node" | "python" | "uv" | "deno")
        {
            required_tools.insert(head);
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
    if service_name == "main"
        && let Some(port) = runtime_overrides::override_port(plan.execution_port())
    {
        env.insert("PORT".to_string(), port.to_string());
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

    // SecretStore-backed launch-condition grants (#508). Applied at the service
    // env-build boundary only and kept off the receipt-observed `merged_env`; last
    // so a secret wins for its exact env key.
    for secret in launch_ctx.secret_env() {
        env.insert(secret.name.clone(), secret.value.expose().to_string());
    }

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

fn wait_until_ready_in_state(
    service_name: &str,
    state: &Arc<Mutex<ServiceStartupState>>,
) -> Result<()> {
    let mut delay_applied = false;
    let mut deadline: Option<Instant> = None;
    loop {
        let readiness = {
            let mut state_guard = state.lock().unwrap_or_else(|poison| poison.into_inner());
            if state_guard.ready.contains(service_name) {
                return Ok(());
            }

            let service = state_guard.running.get_mut(service_name).ok_or_else(|| {
                AtoExecutionError::execution_contract_invalid(
                    format!(
                        "service '{}' was not started before readiness check",
                        service_name
                    ),
                    None,
                    Some(service_name),
                )
            })?;

            let Some(probe) = service.spec.readiness_probe.clone() else {
                state_guard.ready.insert(service_name.to_string());
                return Ok(());
            };

            let port = resolve_probe_port(&service.env, &probe, service_name)?;
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

            (probe, port)
        };
        if !delay_applied {
            let initial_delay = readiness_initial_delay(&readiness.0);
            if !initial_delay.is_zero() {
                thread::sleep(initial_delay);
            }
            delay_applied = true;
        }

        let timeout = readiness_timeout(&readiness.0);
        let interval = readiness_interval(&readiness.0);
        let deadline = *deadline.get_or_insert_with(|| Instant::now() + timeout);

        if readiness_probe_ok(&readiness.0, readiness.1)? {
            state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .ready
                .insert(service_name.to_string());
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(AtoExecutionError::execution_contract_invalid(
                format!(
                    "service '{}' readiness check timed out after {}s",
                    service_name,
                    timeout.as_secs()
                ),
                Some("readiness_probe"),
                Some(service_name),
            )
            .into());
        }

        thread::sleep(interval);
    }
}

fn readiness_initial_delay(probe: &capsule_core::types::ReadinessProbe) -> Duration {
    Duration::from_secs(probe.initial_delay_seconds as u64)
}

fn readiness_timeout(probe: &capsule_core::types::ReadinessProbe) -> Duration {
    Duration::from_secs(probe.timeout_seconds.max(1) as u64)
}

fn readiness_interval(probe: &capsule_core::types::ReadinessProbe) -> Duration {
    if probe.interval_seconds == 0 {
        return READINESS_INTERVAL;
    }
    Duration::from_secs(probe.interval_seconds as u64)
}

fn resolve_probe_port(
    env: &HashMap<String, String>,
    probe: &capsule_core::types::ReadinessProbe,
    service_name: &str,
) -> Result<Option<u16>> {
    // Exec probes do not use a port; only HTTP/TCP probes require one.
    let has_exec = probe.exec.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
    if has_exec {
        return Ok(None);
    }
    let key = probe.port.as_deref().unwrap_or("").trim();
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
    let port = match env.get(key) {
        Some(value) => value.parse::<u16>().map_err(|_| {
            AtoExecutionError::execution_contract_invalid(
                format!(
                    "services.{}.readiness_probe.port '{}' resolved to non-numeric value '{}'",
                    service_name, key, value
                ),
                Some("services.<name>.readiness_probe.port"),
                Some(service_name),
            )
        })?,
        None => key.parse::<u16>().map_err(|_| {
            AtoExecutionError::execution_contract_invalid(
                format!(
                    "services.{}.readiness_probe.port '{}' is neither defined in service env nor a numeric port literal",
                    service_name, key
                ),
                Some("services.<name>.readiness_probe.port"),
                Some(service_name),
            )
        })?,
    };
    Ok(Some(port))
}

fn readiness_probe_ok(
    probe: &capsule_core::types::ReadinessProbe,
    port: Option<u16>,
) -> Result<bool> {
    if let Some(path) = probe
        .http_get
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        let p = port.ok_or_else(|| -> anyhow::Error {
            AtoExecutionError::execution_contract_invalid(
                "readiness_probe.http_get requires a port",
                Some("readiness_probe.port"),
                None,
            )
            .into()
        })?;
        return Ok(http_probe(path, p));
    }
    if let Some(target) = probe
        .tcp_connect
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        let p = port.ok_or_else(|| -> anyhow::Error {
            AtoExecutionError::execution_contract_invalid(
                "readiness_probe.tcp_connect requires a port",
                Some("readiness_probe.port"),
                None,
            )
            .into()
        })?;
        return Ok(tcp_probe(target, p));
    }
    Err(AtoExecutionError::execution_contract_invalid(
        "readiness_probe must define http_get, tcp_connect, or exec",
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

#[cfg_attr(not(test), allow(dead_code))]
fn service_startup_order(services: &HashMap<String, ServiceSpec>) -> Result<Vec<String>> {
    Ok(ServiceGraphPlan::from_services(services)?
        .startup_order()
        .to_vec())
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeLaunchContext, apply_port_admission, plan_service_port_admission,
        readiness_initial_delay, readiness_interval, readiness_timeout, resolve_probe_port,
        service_startup_order,
    };
    use crate::adapters::runtime::port_admission::{logical_endpoint, record_port_admission_plan};
    use capsule_core::installed_state::{ConflictPolicy, InstalledStateDb, PortClaim};
    use capsule_core::types::ReadinessProbe;
    use capsule_core::types::ServiceSpec;
    use std::collections::HashMap;
    use std::time::Duration;

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = InstalledStateDb::open(dir.path().join("state")).expect("open db");
        (dir, db)
    }

    fn installed_ctx(ipk: &str) -> RuntimeLaunchContext {
        RuntimeLaunchContext::empty().with_install_profile_key(Some(ipk.to_string()))
    }

    fn other_app_main_claim(ipk: &str, port: u16) -> PortClaim {
        PortClaim {
            install_profile_key: ipk.to_string(),
            logical_endpoint: logical_endpoint(ipk, "main"),
            preferred_port: port,
            last_actual_port: Some(port),
            protocol: super::PORT_ADMISSION_PROTOCOL.to_string(),
            conflict_policy: ConflictPolicy::Remap,
        }
    }

    /// Reproduce exactly what `start_service` does to the env: resolve the
    /// admission plan, apply it, and return the resulting `PORT`.
    fn resolved_port_env(
        db: &InstalledStateDb,
        launch_ctx: &RuntimeLaunchContext,
        service_name: &str,
        preferred: u16,
    ) -> String {
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), preferred.to_string());
        let plan =
            plan_service_port_admission(db, launch_ctx, service_name, Some(preferred), |_| true)
                .expect("admission must not error under Remap");
        if let Some(plan) = &plan {
            apply_port_admission(&mut env, plan);
        }
        env.get("PORT").cloned().expect("PORT must be present")
    }

    #[test]
    fn installed_conflicting_claim_remaps_port_env() {
        let (_d, db) = temp_db();
        // A different installed app already holds 3000.
        db.record_port_claim(&other_app_main_claim("ipk_other", 3000))
            .unwrap();
        let ctx = installed_ctx("ipk_app");
        let port = resolved_port_env(&db, &ctx, "main", 3000);
        assert_ne!(port, "3000", "conflicting installed claim must remap PORT");
        assert!(
            port.parse::<u16>().unwrap() >= 49152,
            "remap must land in the ephemeral range"
        );
    }

    #[test]
    fn non_installed_launch_leaves_port_unchanged() {
        let (_d, db) = temp_db();
        // Even with a conflicting claim on record, a non-installed launch
        // (no install_profile_key) must keep its preferred PORT.
        db.record_port_claim(&other_app_main_claim("ipk_other", 3000))
            .unwrap();
        let ctx = RuntimeLaunchContext::empty();
        let port = resolved_port_env(&db, &ctx, "main", 3000);
        assert_eq!(port, "3000", "non-installed launch must be untouched");
    }

    #[test]
    fn successful_launch_records_last_actual_port() {
        let (_d, db) = temp_db();
        let ctx = installed_ctx("ipk_app");
        let plan = plan_service_port_admission(&db, &ctx, "main", Some(3000), |_| true)
            .unwrap()
            .expect("installed main launch must produce a plan");
        // start_service records only after a successful spawn.
        record_port_admission_plan(&db, &plan);

        let claims = db.port_claims().unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].install_profile_key, "ipk_app");
        assert_eq!(claims[0].preferred_port, 3000);
        assert_eq!(claims[0].last_actual_port, Some(3000));
    }

    #[test]
    fn same_app_same_endpoint_keeps_preferred_port() {
        let (_d, db) = temp_db();
        // The app already holds its own main endpoint at 3000.
        db.record_port_claim(&other_app_main_claim("ipk_app", 3000))
            .unwrap();
        let ctx = installed_ctx("ipk_app");
        // Relaunching the same app/endpoint sees its own claim as self → keeps 3000.
        let port = resolved_port_env(&db, &ctx, "main", 3000);
        assert_eq!(port, "3000", "own endpoint claim must not self-conflict");
    }

    #[test]
    fn non_main_service_is_not_admitted() {
        let (_d, db) = temp_db();
        db.record_port_claim(&other_app_main_claim("ipk_other", 3000))
            .unwrap();
        let ctx = installed_ctx("ipk_app");
        // Only the `main` service's PORT is ato-injected / admitted.
        let plan = plan_service_port_admission(&db, &ctx, "worker", Some(3000), |_| true).unwrap();
        assert!(plan.is_none(), "non-main services skip port admission");
    }

    fn http_probe(port: &str) -> ReadinessProbe {
        ReadinessProbe {
            http_get: Some("/".to_string()),
            tcp_connect: None,
            exec: None,
            port: Some(port.to_string()),
            initial_delay_seconds: 0,
            timeout_seconds: 1,
            interval_seconds: 1,
        }
    }

    #[test]
    fn readiness_timing_uses_manifest_probe_fields() {
        let probe = ReadinessProbe {
            initial_delay_seconds: 3,
            timeout_seconds: 60,
            interval_seconds: 2,
            ..http_probe("3000")
        };

        assert_eq!(readiness_initial_delay(&probe), Duration::from_secs(3));
        assert_eq!(readiness_timeout(&probe), Duration::from_secs(60));
        assert_eq!(readiness_interval(&probe), Duration::from_secs(2));
    }

    #[test]
    fn resolve_probe_port_accepts_numeric_literal() {
        let env = HashMap::new();
        let port = resolve_probe_port(&env, &http_probe("3000"), "main")
            .expect("literal port should resolve");
        assert_eq!(port, Some(3000));
    }

    #[test]
    fn resolve_probe_port_keeps_env_placeholder_precedence() {
        let env = HashMap::from([("PORT".to_string(), "4173".to_string())]);
        let port = resolve_probe_port(&env, &http_probe("PORT"), "main")
            .expect("env placeholder should resolve");
        assert_eq!(port, Some(4173));
    }

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
