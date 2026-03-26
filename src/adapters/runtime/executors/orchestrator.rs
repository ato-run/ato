use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Child;
use std::sync::{
    mpsc::{Receiver, TryRecvError},
    Arc, Mutex as StdMutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;

use capsule_core::execution_plan::guard::ExecutorKind;
use capsule_core::lifecycle::LifecycleEvent;
use capsule_core::router::ManifestData;
use capsule_core::runtime::oci::{
    BollardOciRuntimeClient, OciContainerRequest, OciLogChunk, OciMountSpec, OciNetworkRequest,
    OciPortSpec, OciRuntimeClient,
};
use capsule_core::types::{
    OrchestrationPlan, ReadinessProbe, ResolvedService, ResolvedServiceRuntime,
};
use capsule_core::CapsuleReporter;

use super::launch_context::RuntimeLaunchContext;
use super::source::ExecuteMode;
use super::target_runner::{self, TargetLaunchOptions};
use crate::application::pipeline::cleanup::{CleanupScope, PipelineAttemptContext};
use crate::application::pipeline::phases::run::PreparedRunContext;
use crate::application::services::{
    ServiceGraphPlan, ServicePhaseCoordinator, ServicePhaseRuntime,
};
use crate::reporters::CliReporter;
use crate::runtime::overrides as runtime_overrides;

const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const READINESS_INTERVAL: Duration = Duration::from_millis(250);
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(200);
const OCI_STOP_TIMEOUT_SECS: i64 = 5;

#[derive(Debug, Clone)]
pub struct OrchestratorOptions {
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub assume_yes: bool,
    pub nacelle: Option<PathBuf>,
}

impl OrchestratorOptions {
    fn target_launch_options(&self) -> TargetLaunchOptions {
        TargetLaunchOptions {
            enforcement: self.enforcement.clone(),
            sandbox_mode: self.sandbox_mode,
            dangerously_skip_permissions: self.dangerously_skip_permissions,
            assume_yes: self.assume_yes,
            preview_mode: false,
            defer_consent: false,
        }
    }
}

pub async fn execute(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    options: OrchestratorOptions,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<i32> {
    let client = BollardOciRuntimeClient::connect_default()
        .context("Failed to connect to OCI engine via Docker-compatible API")?;
    execute_with_client(
        plan, prepared, reporter, launch_ctx, &options, attempt, client,
    )
    .await
}

pub async fn execute_with_client<C>(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    options: &OrchestratorOptions,
    attempt: Option<&mut PipelineAttemptContext>,
    client: C,
) -> Result<i32>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    let orchestration = plan.resolve_services()?;
    let graph = ServiceGraphPlan::from_services(&plan.services())?;
    let session_id = session_id(plan);
    let client = Arc::new(client);
    let network_name = if orchestration
        .services
        .iter()
        .any(|service| service.runtime.is_oci())
    {
        Some(network_name(plan))
    } else {
        None
    };

    if let Some(network_name) = network_name.as_ref() {
        client
            .create_network(&OciNetworkRequest {
                name: network_name.clone(),
                labels: session_labels(plan, &session_id),
            })
            .await?;
    }

    let runtime = OrchestratorStartupRuntime::new(
        plan.clone(),
        prepared.clone(),
        orchestration.clone(),
        reporter.clone(),
        launch_ctx.clone(),
        options.clone(),
        Arc::clone(&client),
        session_id,
        network_name.clone(),
        attempt.map(|attempt| attempt.cleanup_scope()),
    );

    if let Err(err) = ServicePhaseCoordinator::new(&graph)
        .run(runtime.clone())
        .await
    {
        let mut running = runtime.into_running().await;
        shutdown_all(
            &orchestration,
            &mut running,
            client.as_ref(),
            network_name.as_deref(),
        )
        .await;
        return Err(err);
    }

    runtime.commit_startup_cleanup();
    let mut running = runtime.into_running().await;

    notify_main_endpoint(&orchestration, &running, &reporter).await?;

    let exit_code = monitor_until_exit(
        &orchestration,
        &mut running,
        client.as_ref(),
        network_name.as_deref(),
    )
    .await?;
    Ok(exit_code)
}

struct RunningService {
    service: ResolvedService,
    env: HashMap<String, String>,
    handle: RunningHandle,
}

impl RunningService {
    fn local_pid(&self) -> Option<u32> {
        match &self.handle {
            RunningHandle::Local(local) => Some(local.child.id()),
            RunningHandle::Oci(_) => None,
        }
    }
}

enum RunningHandle {
    Local(RunningLocalService),
    Oci(RunningOciService),
}

struct RunningLocalService {
    child: Child,
    stdout_thread: Option<JoinHandle<std::io::Result<()>>>,
    stderr_thread: Option<JoinHandle<std::io::Result<()>>>,
    cleanup_paths: Vec<PathBuf>,
    exit_task: Option<tokio::task::JoinHandle<Result<i32>>>,
    event_rx: Option<Receiver<LifecycleEvent>>,
    readiness_state: LocalReadinessState,
}

struct RunningOciService {
    container_id: String,
    log_task: Option<tokio::task::JoinHandle<()>>,
    host_ports: HashMap<u16, u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalReadinessState {
    Pending,
    Ready,
    Exited(i32),
}

#[derive(Default)]
struct OrchestratorStartupState {
    running: HashMap<String, RunningService>,
    ready: HashSet<String>,
}

#[derive(Clone)]
struct OrchestratorStartupRuntime<C>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    plan: ManifestData,
    prepared: PreparedRunContext,
    orchestration: OrchestrationPlan,
    reporter: Arc<CliReporter>,
    launch_ctx: RuntimeLaunchContext,
    options: OrchestratorOptions,
    client: Arc<C>,
    session_id: String,
    network_name: Option<String>,
    state: Arc<Mutex<OrchestratorStartupState>>,
    startup_cleanup: Arc<StdMutex<Option<CleanupScope>>>,
}

impl<C> OrchestratorStartupRuntime<C>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        plan: ManifestData,
        prepared: PreparedRunContext,
        orchestration: OrchestrationPlan,
        reporter: Arc<CliReporter>,
        launch_ctx: RuntimeLaunchContext,
        options: OrchestratorOptions,
        client: Arc<C>,
        session_id: String,
        network_name: Option<String>,
        startup_cleanup: Option<CleanupScope>,
    ) -> Self {
        Self {
            plan,
            prepared,
            orchestration,
            reporter,
            launch_ctx,
            options,
            client,
            session_id,
            network_name,
            state: Arc::new(Mutex::new(OrchestratorStartupState::default())),
            startup_cleanup: Arc::new(StdMutex::new(startup_cleanup)),
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

    async fn into_running(self) -> HashMap<String, RunningService> {
        let mut state = self.state.lock().await;
        std::mem::take(&mut state.running)
    }
}

#[async_trait]
impl<C> ServicePhaseRuntime for OrchestratorStartupRuntime<C>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    async fn start_service(&self, service_name: &str) -> Result<()> {
        let service = self
            .orchestration
            .service(service_name)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "service '{}' is missing from orchestration plan",
                    service_name
                )
            })?;

        let env = {
            let state = self.state.lock().await;
            build_service_env(&self.plan, &service, &state.running, &self.launch_ctx)?
        };
        preflight_required_envs(&service, &env)?;

        let running_service = launch_service(
            &self.plan,
            &self.prepared,
            &self.orchestration,
            &service,
            env,
            &self.reporter,
            &self.launch_ctx,
            self.client.as_ref(),
            &self.session_id,
            self.network_name.as_deref(),
            &self.options,
        )
        .await?;

        if let Some(pid) = running_service.local_pid() {
            if let Some(scope) = self
                .startup_cleanup
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_mut()
            {
                scope.register_kill_child_process(pid, service.name.clone());
            }
        }

        self.state
            .lock()
            .await
            .running
            .insert(service.name.clone(), running_service);
        Ok(())
    }

    async fn await_readiness(&self, service_name: String) -> Result<()> {
        wait_until_ready_in_state(&service_name, &self.state, self.client.as_ref()).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn launch_service<C: OciRuntimeClient>(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    orchestration: &OrchestrationPlan,
    service: &ResolvedService,
    env: HashMap<String, String>,
    reporter: &std::sync::Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    client: &C,
    session_id: &str,
    network_name: Option<&str>,
    options: &OrchestratorOptions,
) -> Result<RunningService> {
    let handle = match &service.runtime {
        ResolvedServiceRuntime::Oci(runtime) => {
            let image = runtime
                .image
                .clone()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("service '{}' is missing OCI image", service.name)
                })?;

            client.pull_image(&image).await?;

            let publish_mode = determine_publish_mode(orchestration, service);
            let host_port = if matches!(publish_mode, PublishMode::Fixed) {
                runtime.port
            } else {
                None
            };
            let ports = runtime
                .port
                .filter(|_| !matches!(publish_mode, PublishMode::None))
                .map(|port| {
                    vec![OciPortSpec {
                        container_port: port,
                        host_port,
                        protocol: "tcp".to_string(),
                        host_ip: Some("127.0.0.1".to_string()),
                    }]
                })
                .unwrap_or_default();

            let container_id = client
                .create_container(&OciContainerRequest {
                    name: container_name(plan, &service.name, session_id),
                    image,
                    cmd: runtime.cmd.clone(),
                    env: env.clone(),
                    working_dir: runtime.working_dir.clone(),
                    labels: container_labels(plan, &service.name, session_id, &runtime.target),
                    mounts: runtime
                        .mounts
                        .iter()
                        .map(|mount| OciMountSpec {
                            source: mount.source.clone(),
                            target: mount.target.clone(),
                            readonly: mount.readonly,
                        })
                        .collect(),
                    ports,
                    network: network_name.map(str::to_string),
                    aliases: service.network.aliases.clone(),
                })
                .await?;
            client.start_container(&container_id).await?;

            let inspect = client
                .inspect_container(&container_id)
                .await
                .unwrap_or_default();
            let mut logs = client.logs(&container_id, true).await?;
            let service_name = service.name.clone();
            let log_task = tokio::spawn(async move {
                while let Some(chunk) = logs.recv().await {
                    match chunk {
                        Ok(chunk) => {
                            let _ = print_prefixed_chunk(&service_name, &chunk);
                        }
                        Err(err) => {
                            let _ = writeln!(
                                std::io::stderr(),
                                "[{}] log error: {}",
                                service_name,
                                err
                            );
                            break;
                        }
                    }
                }
            });

            RunningHandle::Oci(RunningOciService {
                container_id,
                log_task: Some(log_task),
                host_ports: inspect.host_ports,
            })
        }
        ResolvedServiceRuntime::Managed(runtime) => {
            let service_plan = ManifestData {
                selected_target: runtime.target.clone(),
                ..plan.clone()
            };
            let service_launch_ctx = launch_ctx.clone().with_injected_env(env.clone());
            let service_prepared = prepared.with_raw_manifest(
                service_plan.manifest.clone(),
                if options.target_launch_options().preview_mode {
                    capsule_core::types::ValidationMode::Preview
                } else {
                    capsule_core::types::ValidationMode::Strict
                },
                service_plan.manifest.get("engine").is_some(),
            );
            let prepared = target_runner::prepare_target_execution(
                &service_plan,
                &service_prepared,
                service_launch_ctx,
                &options.target_launch_options(),
            )?;
            let managed_plan = &prepared.runtime_decision.plan;

            let (mut child, cleanup_paths, exit_task, event_rx) = match prepared
                .guard_result
                .executor_kind
            {
                ExecutorKind::Native => {
                    let process = if options.dangerously_skip_permissions {
                        crate::executors::source::execute_host(
                            managed_plan,
                            reporter.clone(),
                            ExecuteMode::Piped,
                            &prepared.launch_ctx,
                        )?
                    } else {
                        let nacelle = crate::commands::run::preflight_native_sandbox(
                            options.nacelle.clone(),
                            managed_plan,
                            &service_prepared,
                            reporter,
                        )?;
                        crate::executors::source::execute(
                            managed_plan,
                            service_prepared.authoritative_lock.as_ref(),
                            service_prepared.effective_state.as_ref(),
                            Some(nacelle),
                            reporter.clone(),
                            &options.enforcement,
                            ExecuteMode::Piped,
                            &prepared.launch_ctx,
                        )?
                    };
                    let pid = process.child.id();
                    let exit_task = if options.dangerously_skip_permissions {
                        None
                    } else {
                        Some(tokio::spawn(crate::executors::source::wait_for_pid_exit(
                            pid,
                        )))
                    };
                    (
                        process.child,
                        process.cleanup_paths,
                        exit_task,
                        process.event_rx,
                    )
                }
                ExecutorKind::Deno => (
                    crate::executors::deno::spawn(
                        managed_plan,
                        &prepared.execution_plan,
                        &prepared.launch_ctx,
                        options.dangerously_skip_permissions,
                    )?,
                    Vec::new(),
                    None,
                    None,
                ),
                ExecutorKind::NodeCompat => (
                    crate::executors::node_compat::spawn(
                        managed_plan,
                        &prepared.execution_plan,
                        &prepared.launch_ctx,
                        options.dangerously_skip_permissions,
                    )?,
                    Vec::new(),
                    None,
                    None,
                ),
                ExecutorKind::WebStatic => {
                    anyhow::bail!(
                        "service '{}' uses runtime=web driver=static, which is unsupported in orchestration mode",
                        service.name
                    );
                }
                ExecutorKind::Wasm => {
                    anyhow::bail!(
                        "service '{}' uses runtime=wasm, which is unsupported in orchestration mode",
                        service.name
                    );
                }
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            RunningHandle::Local(RunningLocalService {
                child,
                stdout_thread: Some(spawn_prefixed_stream(stdout, &service.name, false)),
                stderr_thread: Some(spawn_prefixed_stream(stderr, &service.name, true)),
                cleanup_paths,
                exit_task,
                event_rx,
                readiness_state: LocalReadinessState::Pending,
            })
        }
    };

    Ok(RunningService {
        service: service.clone(),
        env,
        handle,
    })
}

fn build_service_env(
    plan: &ManifestData,
    service: &ResolvedService,
    running: &HashMap<String, RunningService>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<HashMap<String, String>> {
    let mut env = launch_ctx.merged_env();
    env.extend(runtime_overrides::merged_env(
        service.runtime.runtime().env.clone(),
    ));

    if let Some(port) = service.runtime.runtime().port {
        let port = if service.name == "main" {
            runtime_overrides::override_port(Some(port)).unwrap_or(port)
        } else {
            port
        };
        env.insert("PORT".to_string(), port.to_string());
        if service
            .runtime
            .runtime()
            .runtime
            .eq_ignore_ascii_case("web")
        {
            env.entry("HOST".to_string())
                .or_insert_with(|| "127.0.0.1".to_string());
            env.entry("ATO_WEB_HOST".to_string())
                .or_insert_with(|| "127.0.0.1".to_string());
        }
    }

    for connection in &service.connections {
        let dependency = running.get(&connection.dependency).ok_or_else(|| {
            anyhow::anyhow!(
                "dependency '{}' for service '{}' has not been started",
                connection.dependency,
                service.name
            )
        })?;

        let dependency_port = connection.container_port.ok_or_else(|| {
            anyhow::anyhow!(
                "dependency '{}' for service '{}' does not declare a port",
                connection.dependency,
                service.name
            )
        })?;

        let (host, port) = if service.runtime.is_oci() {
            if !dependency.service.runtime.is_oci() {
                anyhow::bail!(
                    "OCI service '{}' cannot depend on non-OCI service '{}'",
                    service.name,
                    connection.dependency
                );
            }
            (
                dependency.service.primary_alias().to_string(),
                dependency_port,
            )
        } else if dependency.service.runtime.is_oci() {
            (
                "127.0.0.1".to_string(),
                resolve_host_port(dependency, dependency_port)?,
            )
        } else {
            ("127.0.0.1".to_string(), dependency_port)
        };

        env.insert(connection.host_env.clone(), host);
        env.insert(connection.port_env.clone(), port.to_string());
    }

    if service.name == "main" {
        if let Some(scoped_id) = runtime_overrides::scoped_id_override() {
            env.insert("ATO_SCOPED_ID".to_string(), scoped_id);
        }
    }

    if let Some(path) = plan.manifest_path.to_str() {
        env.entry("ATO_MANIFEST_PATH".to_string())
            .or_insert_with(|| path.to_string());
    }

    Ok(env)
}

fn preflight_required_envs(service: &ResolvedService, env: &HashMap<String, String>) -> Result<()> {
    let override_env = runtime_overrides::override_env();
    let missing: Vec<String> = service
        .runtime
        .runtime()
        .required_env
        .iter()
        .filter(|key| {
            env.get(*key)
                .map(|value| value.trim().is_empty())
                .unwrap_or_else(|| {
                    if override_env
                        .get(key.as_str())
                        .map(|value| !value.trim().is_empty())
                        .unwrap_or(false)
                    {
                        return false;
                    }
                    std::env::var(key.as_str())
                        .map(|value| value.trim().is_empty())
                        .unwrap_or(true)
                })
        })
        .cloned()
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "missing required environment variables for service '{}': {}",
        service.name,
        missing.join(", ")
    );
}

async fn wait_until_ready_in_state<C: OciRuntimeClient>(
    service_name: &str,
    state: &Arc<Mutex<OrchestratorStartupState>>,
    client: &C,
) -> Result<()> {
    {
        let state = state.lock().await;
        if state.ready.contains(service_name) {
            return Ok(());
        }
    }

    let mut service = {
        let mut state = state.lock().await;
        state.running.remove(service_name).ok_or_else(|| {
            anyhow::anyhow!(
                "service '{}' was not started before readiness check",
                service_name
            )
        })?
    };

    let result = async {
        let Some(probe) = service.service.readiness_probe.clone() else {
            return Ok(());
        };

        let deadline = Instant::now() + READINESS_TIMEOUT;
        loop {
            if let RunningHandle::Local(local) = &mut service.handle {
                match poll_local_readiness_events(local)? {
                    LocalReadinessState::Ready => return Ok(()),
                    LocalReadinessState::Exited(exit_code) => {
                        anyhow::bail!(
                            "service '{}' exited before readiness event was observed (exit code: {})",
                            service_name,
                            exit_code
                        );
                    }
                    LocalReadinessState::Pending => {}
                }
            }

            if let Some(exit_code) = try_wait(&mut service, client).await? {
                anyhow::bail!(
                    "service '{}' exited before readiness check passed (exit code: {})",
                    service_name,
                    exit_code
                );
            }

            if !uses_event_driven_readiness(&service) {
                let port = resolve_probe_port(&service, &probe)?;
                if readiness_probe_ok(&probe, port)? {
                    return Ok(());
                }
            }

            if Instant::now() >= deadline {
                anyhow::bail!(
                    "service '{}' readiness check timed out after {}s",
                    service_name,
                    READINESS_TIMEOUT.as_secs()
                );
            }

            tokio::time::sleep(READINESS_INTERVAL).await;
        }
    }
    .await;

    let mut state = state.lock().await;
    state.running.insert(service_name.to_string(), service);
    if result.is_ok() {
        state.ready.insert(service_name.to_string());
    }
    result
}

async fn monitor_until_exit<C: OciRuntimeClient>(
    orchestration: &OrchestrationPlan,
    running: &mut HashMap<String, RunningService>,
    client: &C,
    network_name: Option<&str>,
) -> Result<i32> {
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);

    loop {
        tokio::select! {
            signal_code = &mut shutdown_signal => {
                shutdown_all(orchestration, running, client, network_name).await;
                return signal_code;
            }
            _ = tokio::time::sleep(SHUTDOWN_POLL_INTERVAL) => {
                let mut exited = None;
                for service_name in &orchestration.startup_order {
                    let Some(service) = running.get_mut(service_name) else {
                        continue;
                    };
                    if let Some(exit_code) = try_wait(service, client).await? {
                        exited = Some((service_name.clone(), exit_code));
                        break;
                    }
                }

                if let Some((_exited_name, exit_code)) = exited {
                    shutdown_all(orchestration, running, client, network_name).await;
                    return Ok(exit_code);
                }
            }
        }
    }
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> Result<i32> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("Failed to install SIGTERM handler for orchestrator")?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => Ok(130),
        _ = sigterm.recv() => Ok(143),
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> Result<i32> {
    tokio::signal::ctrl_c()
        .await
        .context("Failed to install Ctrl+C handler for orchestrator")?;
    Ok(130)
}

fn uses_event_driven_readiness(service: &RunningService) -> bool {
    matches!(
        &service.handle,
        RunningHandle::Local(local) if local.event_rx.is_some()
    )
}

fn poll_local_readiness_events(local: &mut RunningLocalService) -> Result<LocalReadinessState> {
    if matches!(
        local.readiness_state,
        LocalReadinessState::Ready | LocalReadinessState::Exited(_)
    ) {
        return Ok(local.readiness_state);
    }

    let Some(event_rx) = local.event_rx.as_ref() else {
        return Ok(LocalReadinessState::Pending);
    };

    match event_rx.try_recv() {
        Ok(LifecycleEvent::Ready { .. }) => {
            local.readiness_state = LocalReadinessState::Ready;
            Ok(local.readiness_state)
        }
        Ok(LifecycleEvent::Exited { exit_code, .. }) => {
            local.readiness_state = LocalReadinessState::Exited(exit_code.unwrap_or(1));
            Ok(local.readiness_state)
        }
        Err(TryRecvError::Empty) => Ok(local.readiness_state),
        Err(TryRecvError::Disconnected) => {
            local.event_rx = None;
            Ok(local.readiness_state)
        }
    }
}

async fn shutdown_all<C: OciRuntimeClient>(
    orchestration: &OrchestrationPlan,
    running: &mut HashMap<String, RunningService>,
    client: &C,
    network_name: Option<&str>,
) {
    for service_name in orchestration.startup_order.iter().rev() {
        let Some(mut service) = running.remove(service_name) else {
            continue;
        };
        let _ = stop_service(&mut service, client).await;
        drain_service(&mut service);
    }

    if let Some(network_name) = network_name {
        let _ = client.remove_network(network_name).await;
    }
}

async fn stop_service<C: OciRuntimeClient>(service: &mut RunningService, client: &C) -> Result<()> {
    match &mut service.handle {
        RunningHandle::Local(local) => {
            let _ = send_sigterm(&mut local.child);
            let deadline = Instant::now() + Duration::from_secs(OCI_STOP_TIMEOUT_SECS as u64);
            while Instant::now() < deadline {
                if let Some(task) = local.exit_task.as_ref() {
                    if task.is_finished() {
                        if let Some(task) = local.exit_task.take() {
                            let _ = task.await;
                        }
                        return Ok(());
                    }
                } else if local.child.try_wait()?.is_some() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }
            if local.exit_task.is_some() || local.child.try_wait()?.is_none() {
                let _ = local.child.kill();
                let _ = local.child.wait();
            }
            if let Some(task) = local.exit_task.take() {
                task.abort();
                let _ = task.await;
            }
        }
        RunningHandle::Oci(oci) => {
            let _ = client
                .stop_container(&oci.container_id, OCI_STOP_TIMEOUT_SECS)
                .await;
            let _ = client.remove_container(&oci.container_id, true).await;
        }
    }
    Ok(())
}

fn drain_service(service: &mut RunningService) {
    match &mut service.handle {
        RunningHandle::Local(local) => {
            if let Some(handle) = local.stdout_thread.take() {
                let _ = handle.join();
            }
            if let Some(handle) = local.stderr_thread.take() {
                let _ = handle.join();
            }
            for path in local.cleanup_paths.drain(..) {
                if path.exists() {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        RunningHandle::Oci(oci) => {
            if let Some(task) = oci.log_task.take() {
                task.abort();
            }
        }
    }
}

async fn try_wait<C: OciRuntimeClient>(
    service: &mut RunningService,
    client: &C,
) -> Result<Option<i32>> {
    match &mut service.handle {
        RunningHandle::Local(local) => {
            if let Some(task) = local.exit_task.as_ref() {
                if !task.is_finished() {
                    return Ok(None);
                }
                let task = local
                    .exit_task
                    .take()
                    .expect("finished exit task must still be present");
                return Ok(Some(
                    task.await.context("native service exit watcher failed")??,
                ));
            }

            Ok(local
                .child
                .try_wait()?
                .map(|status| status.code().unwrap_or(1)))
        }
        RunningHandle::Oci(oci) => {
            let inspect = client.inspect_container(&oci.container_id).await?;
            oci.host_ports = inspect.host_ports.clone();
            if inspect.running {
                Ok(None)
            } else {
                Ok(Some(inspect.exit_code.unwrap_or(1) as i32))
            }
        }
    }
}

fn resolve_probe_port(service: &RunningService, probe: &ReadinessProbe) -> Result<u16> {
    let key = probe.port.trim();
    if key.is_empty() {
        anyhow::bail!(
            "services.{}.readiness_probe.port must be a non-empty env placeholder",
            service.service.name
        );
    }

    let value = service.env.get(key).ok_or_else(|| {
        anyhow::anyhow!(
            "services.{}.readiness_probe.port '{}' is not defined in service env",
            service.service.name,
            key
        )
    })?;
    let container_port = value.parse::<u16>().map_err(|_| {
        anyhow::anyhow!(
            "services.{}.readiness_probe.port '{}' resolved to non-numeric value '{}'",
            service.service.name,
            key,
            value
        )
    })?;

    match &service.handle {
        RunningHandle::Local(_) => Ok(container_port),
        RunningHandle::Oci(oci) => Ok(oci
            .host_ports
            .get(&container_port)
            .copied()
            .unwrap_or(container_port)),
    }
}

fn resolve_host_port(service: &RunningService, container_port: u16) -> Result<u16> {
    match &service.handle {
        RunningHandle::Local(_) => Ok(container_port),
        RunningHandle::Oci(oci) => oci.host_ports.get(&container_port).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "service '{}' has no published host port for {}",
                service.service.name,
                container_port
            )
        }),
    }
}

fn determine_publish_mode(
    orchestration: &OrchestrationPlan,
    service: &ResolvedService,
) -> PublishMode {
    if service.name == "main" || service.network.publish {
        return PublishMode::Fixed;
    }

    if service.readiness_probe.is_some() {
        return PublishMode::Ephemeral;
    }

    if orchestration.services.iter().any(|candidate| {
        candidate
            .depends_on
            .iter()
            .any(|dependency| dependency == &service.name)
            && !candidate.runtime.is_oci()
    }) {
        return PublishMode::Ephemeral;
    }

    PublishMode::None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublishMode {
    None,
    Fixed,
    Ephemeral,
}

async fn notify_main_endpoint(
    orchestration: &OrchestrationPlan,
    running: &HashMap<String, RunningService>,
    reporter: &std::sync::Arc<CliReporter>,
) -> Result<()> {
    let Some(main) = orchestration.service("main") else {
        return Ok(());
    };
    let Some(port) = main.runtime.runtime().port else {
        return Ok(());
    };
    let Some(running_main) = running.get("main") else {
        return Ok(());
    };

    let host_port = if main.runtime.is_oci() {
        resolve_host_port(running_main, port)?
    } else {
        port
    };

    reporter
        .notify(format!(
            "🌐 Orchestrated service 'main' is available at http://127.0.0.1:{}/",
            host_port
        ))
        .await?;
    Ok(())
}

fn readiness_probe_ok(probe: &ReadinessProbe, port: u16) -> Result<bool> {
    if let Some(path) = probe
        .http_get
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(http_probe(path, port));
    }
    if let Some(target) = probe
        .tcp_connect
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(tcp_probe(target, port));
    }
    anyhow::bail!("readiness_probe must define http_get or tcp_connect");
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

fn print_prefixed_chunk(service_name: &str, chunk: &OciLogChunk) -> Result<()> {
    let prefix = format!("[{}] ", service_name);
    if chunk.stderr {
        let mut writer = std::io::stderr();
        writer.write_all(prefix.as_bytes())?;
        writer.write_all(&chunk.message)?;
        writer.flush()?;
    } else {
        let mut writer = std::io::stdout();
        writer.write_all(prefix.as_bytes())?;
        writer.write_all(&chunk.message)?;
        writer.flush()?;
    }
    Ok(())
}

fn session_id(plan: &ManifestData) -> String {
    format!(
        "{}-{}-{}",
        sanitize_name(
            &plan
                .manifest_name()
                .unwrap_or_else(|| "capsule".to_string())
        ),
        short_hash(plan.manifest_name().as_deref().unwrap_or("capsule")),
        std::process::id()
    )
}

fn network_name(plan: &ManifestData) -> String {
    let manifest_name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    format!(
        "ato-{}-{}-{}",
        sanitize_name(&manifest_name),
        short_hash(&manifest_name),
        std::process::id()
    )
}

fn session_labels(plan: &ManifestData, session_id: &str) -> HashMap<String, String> {
    HashMap::from([
        ("io.ato.session".to_string(), session_id.to_string()),
        (
            "io.ato.manifest".to_string(),
            plan.manifest_name()
                .unwrap_or_else(|| "capsule".to_string()),
        ),
    ])
}

fn container_labels(
    plan: &ManifestData,
    service_name: &str,
    session_id: &str,
    target_label: &str,
) -> HashMap<String, String> {
    let mut labels = session_labels(plan, session_id);
    labels.insert("io.ato.service".to_string(), service_name.to_string());
    labels.insert("io.ato.target".to_string(), target_label.to_string());
    labels
}

fn container_name(plan: &ManifestData, service_name: &str, session_id: &str) -> String {
    let manifest_name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    format!(
        "ato-{}-{}-{}",
        sanitize_name(&manifest_name),
        short_hash(session_id),
        sanitize_name(service_name)
    )
}

fn short_hash(value: &str) -> String {
    blake3::hash(value.as_bytes())
        .to_hex()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
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

#[cfg(test)]
mod tests {
    use super::execute_with_client;
    use super::*;
    use capsule_core::runtime::oci::OciContainerInspect;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeClient {
        events: Arc<Mutex<Vec<String>>>,
        states: Arc<Mutex<HashMap<String, FakeState>>>,
    }

    #[derive(Clone, Default)]
    struct FakeState {
        service: String,
        running: bool,
        exit_code: i64,
        inspect_calls: usize,
        host_ports: HashMap<u16, u16>,
        mounts: Vec<(String, String, bool)>,
    }

    #[async_trait::async_trait]
    impl OciRuntimeClient for FakeClient {
        async fn pull_image(&self, image: &str) -> capsule_core::Result<()> {
            self.events.lock().unwrap().push(format!("pull:{image}"));
            Ok(())
        }

        async fn create_network(
            &self,
            request: &OciNetworkRequest,
        ) -> capsule_core::Result<String> {
            self.events
                .lock()
                .unwrap()
                .push(format!("network:create:{}", request.name));
            Ok(request.name.clone())
        }

        async fn remove_network(&self, network_name: &str) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("network:remove:{network_name}"));
            Ok(())
        }

        async fn create_container(
            &self,
            request: &OciContainerRequest,
        ) -> capsule_core::Result<String> {
            let service = request
                .labels
                .get("io.ato.service")
                .cloned()
                .unwrap_or_else(|| request.name.clone());
            self.events
                .lock()
                .unwrap()
                .push(format!("container:create:{service}"));
            self.states.lock().unwrap().insert(
                request.name.clone(),
                FakeState {
                    service: service.clone(),
                    running: false,
                    exit_code: if service == "main" { 0 } else { 1 },
                    inspect_calls: 0,
                    host_ports: request
                        .ports
                        .iter()
                        .map(|port| {
                            (
                                port.container_port,
                                port.host_port.unwrap_or(port.container_port),
                            )
                        })
                        .collect(),
                    mounts: request
                        .mounts
                        .iter()
                        .map(|mount| (mount.source.clone(), mount.target.clone(), mount.readonly))
                        .collect(),
                },
            );
            Ok(request.name.clone())
        }

        async fn start_container(&self, container_id: &str) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("container:start:{container_id}"));
            if let Some(state) = self.states.lock().unwrap().get_mut(container_id) {
                state.running = true;
            }
            Ok(())
        }

        async fn inspect_container(
            &self,
            container_id: &str,
        ) -> capsule_core::Result<OciContainerInspect> {
            let mut states = self.states.lock().unwrap();
            let state = states.get_mut(container_id).expect("state");
            state.inspect_calls += 1;
            if state.service == "main" && state.inspect_calls > 1 {
                state.running = false;
            }
            Ok(OciContainerInspect {
                running: state.running,
                exit_code: (!state.running).then_some(state.exit_code),
                host_ports: state.host_ports.clone(),
            })
        }

        async fn logs(
            &self,
            _container_id: &str,
            _follow: bool,
        ) -> capsule_core::Result<tokio::sync::mpsc::Receiver<capsule_core::Result<OciLogChunk>>>
        {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn wait_container(&self, _container_id: &str) -> capsule_core::Result<i64> {
            Ok(0)
        }

        async fn stop_container(
            &self,
            container_id: &str,
            _timeout_secs: i64,
        ) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("container:stop:{container_id}"));
            if let Some(state) = self.states.lock().unwrap().get_mut(container_id) {
                state.running = false;
            }
            Ok(())
        }

        async fn remove_container(
            &self,
            container_id: &str,
            _force: bool,
        ) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("container:remove:{container_id}"));
            Ok(())
        }
    }

    fn manifest_data(manifest_toml: &str) -> ManifestData {
        ManifestData {
            manifest: toml::from_str(manifest_toml).expect("manifest toml"),
            manifest_path: PathBuf::from("/tmp/capsule.toml"),
            manifest_dir: PathBuf::from("/tmp"),
            profile: capsule_core::router::ExecutionProfile::Dev,
            selected_target: "app".to_string(),
            state_source_overrides: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn orchestrator_cleans_up_oci_services_and_network() {
        let plan = manifest_data(
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 8080

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306

[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "app"
depends_on = ["db"]

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"

[services.db]
target = "db"
"#,
        );
        let client = FakeClient::default();
        let reporter = Arc::new(CliReporter::new(false));
        let launch_ctx = RuntimeLaunchContext::empty();
        let options = OrchestratorOptions {
            enforcement: "strict".to_string(),
            sandbox_mode: true,
            dangerously_skip_permissions: false,
            assume_yes: true,
            nacelle: None,
        };

        let exit = execute_with_client(
            &plan,
            &PreparedRunContext {
                authoritative_lock: None,
                effective_state: None,
                raw_manifest: plan.manifest.clone(),
                validation_mode: capsule_core::types::ValidationMode::Strict,
                engine_override_declared: false,
                compatibility_legacy_lock: None,
            },
            reporter,
            &launch_ctx,
            &options,
            None,
            client.clone(),
        )
        .await
        .expect("orchestrator exit");
        assert_eq!(exit, 0);

        let events = client.events.lock().unwrap().clone();
        assert!(events
            .iter()
            .any(|event| event.starts_with("network:create:")));
        assert!(events
            .iter()
            .any(|event| event.contains("container:create:db")));
        assert!(events
            .iter()
            .any(|event| event.contains("container:create:main")));
        assert!(events
            .iter()
            .any(|event| event.starts_with("network:remove:")));
        let stop_db = events
            .iter()
            .position(|event| event.contains("container:stop:") && event.contains("db"))
            .expect("db stop");
        let remove_network = events
            .iter()
            .position(|event| event.starts_with("network:remove:"))
            .expect("network remove");
        assert!(stop_db < remove_network);

        let states = client.states.lock().unwrap();
        let app_state = states
            .values()
            .find(|state| state.service == "main")
            .expect("main state");
        assert_eq!(
            app_state.mounts,
            vec![(
                "/var/lib/ato/state/demo-app/data".to_string(),
                "/var/lib/app".to_string(),
                false,
            )]
        );
    }
}
