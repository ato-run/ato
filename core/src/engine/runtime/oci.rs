use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, NetworkingConfig, RemoveContainerOptions,
    StartContainerOptions, StatsOptions, StopContainerOptions, WaitContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::image::CreateImageOptions;
use bollard::models::{EndpointSettings, HostConfig, PortBinding};
use bollard::network::{ConnectNetworkOptions, CreateNetworkOptions};
use bollard::Docker;
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::error::{CapsuleError, Result};
use crate::metrics::{MetricsSession, ResourceStats, RuntimeMetadata, UnifiedMetrics};
use crate::runtime::{Measurable, RuntimeHandle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciPortSpec {
    pub container_port: u16,
    pub host_port: Option<u16>,
    pub protocol: String,
    pub host_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciMountSpec {
    pub source: String,
    pub target: String,
    pub readonly: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OciNetworkRequest {
    pub name: String,
    pub labels: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciContainerRequest {
    pub name: String,
    pub image: String,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    pub working_dir: Option<String>,
    pub labels: HashMap<String, String>,
    pub mounts: Vec<OciMountSpec>,
    pub ports: Vec<OciPortSpec>,
    pub network: Option<String>,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OciContainerInspect {
    pub running: bool,
    pub exit_code: Option<i64>,
    pub host_ports: HashMap<u16, u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciLogChunk {
    pub stderr: bool,
    pub message: Vec<u8>,
}

#[async_trait]
pub trait OciRuntimeClient: Send + Sync {
    async fn pull_image(&self, image: &str) -> Result<()>;
    async fn create_network(&self, request: &OciNetworkRequest) -> Result<String>;
    async fn remove_network(&self, network_name: &str) -> Result<()>;
    async fn create_container(&self, request: &OciContainerRequest) -> Result<String>;
    async fn start_container(&self, container_id: &str) -> Result<()>;
    async fn inspect_container(&self, container_id: &str) -> Result<OciContainerInspect>;
    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> Result<mpsc::Receiver<Result<OciLogChunk>>>;
    async fn wait_container(&self, container_id: &str) -> Result<i64>;
    async fn stop_container(&self, container_id: &str, timeout_secs: i64) -> Result<()>;
    async fn remove_container(&self, container_id: &str, force: bool) -> Result<()>;
}

#[derive(Clone)]
pub struct BollardOciRuntimeClient {
    docker: Docker,
}

impl BollardOciRuntimeClient {
    pub fn connect_default() -> Result<Self> {
        Ok(Self {
            docker: connect_docker_default()?,
        })
    }

    pub fn docker(&self) -> &Docker {
        &self.docker
    }
}

#[async_trait]
impl OciRuntimeClient for BollardOciRuntimeClient {
    async fn pull_image(&self, image: &str) -> Result<()> {
        let mut stream = self.docker.create_image(
            Some(CreateImageOptions::<String> {
                from_image: image.to_string(),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(next) = stream.next().await {
            next.map_err(map_bollard_error)?;
        }
        Ok(())
    }

    async fn create_network(&self, request: &OciNetworkRequest) -> Result<String> {
        let response = self
            .docker
            .create_network(CreateNetworkOptions {
                name: request.name.clone(),
                check_duplicate: true,
                driver: "bridge".to_string(),
                internal: false,
                attachable: true,
                ingress: false,
                ipam: Default::default(),
                enable_ipv6: false,
                options: HashMap::<String, String>::new(),
                labels: request.labels.clone(),
            })
            .await
            .map_err(map_bollard_error)?;

        response
            .id
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                CapsuleError::Runtime("OCI network create returned empty id".to_string())
            })
    }

    async fn remove_network(&self, network_name: &str) -> Result<()> {
        self.docker
            .remove_network(network_name)
            .await
            .map_err(map_bollard_error)
    }

    async fn create_container(&self, request: &OciContainerRequest) -> Result<String> {
        let mut exposed_ports = HashMap::new();
        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        for port in &request.ports {
            let key = format!("{}/{}", port.container_port, port.protocol);
            exposed_ports.insert(key.clone(), HashMap::new());
            port_bindings.insert(
                key,
                Some(vec![PortBinding {
                    host_ip: port.host_ip.clone(),
                    host_port: port.host_port.map(|value| value.to_string()),
                }]),
            );
        }

        let host_config = HostConfig {
            binds: (!request.mounts.is_empty()).then(|| {
                request
                    .mounts
                    .iter()
                    .map(|mount| {
                        let mode = if mount.readonly { "ro" } else { "rw" };
                        format!("{}:{}:{}", mount.source, mount.target, mode)
                    })
                    .collect()
            }),
            port_bindings: (!port_bindings.is_empty()).then_some(port_bindings),
            ..Default::default()
        };

        let networking_config = request.network.as_ref().map(|network| NetworkingConfig {
            endpoints_config: HashMap::from([(
                network.clone(),
                EndpointSettings {
                    aliases: (!request.aliases.is_empty()).then(|| request.aliases.clone()),
                    ..Default::default()
                },
            )]),
        });

        let response = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: request.name.clone(),
                    platform: None,
                }),
                Config {
                    image: Some(request.image.clone()),
                    env: (!request.env.is_empty()).then(|| {
                        request
                            .env
                            .iter()
                            .map(|(key, value)| format!("{key}={value}"))
                            .collect()
                    }),
                    cmd: (!request.cmd.is_empty()).then(|| request.cmd.clone()),
                    working_dir: request.working_dir.clone(),
                    exposed_ports: (!exposed_ports.is_empty()).then_some(exposed_ports),
                    host_config: Some(host_config),
                    labels: (!request.labels.is_empty()).then(|| request.labels.clone()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(map_bollard_error)?;

        if let (Some(network), Some(config)) = (request.network.as_ref(), networking_config) {
            let endpoint_config = config
                .endpoints_config
                .get(network)
                .cloned()
                .unwrap_or_default();
            self.docker
                .connect_network(
                    network,
                    ConnectNetworkOptions {
                        container: response.id.clone(),
                        endpoint_config,
                    },
                )
                .await
                .map_err(map_bollard_error)?;
        }

        Ok(response.id)
    }

    async fn start_container(&self, container_id: &str) -> Result<()> {
        self.docker
            .start_container(container_id, None::<StartContainerOptions<String>>)
            .await
            .map_err(map_bollard_error)
    }

    async fn inspect_container(&self, container_id: &str) -> Result<OciContainerInspect> {
        let inspect = self
            .docker
            .inspect_container(
                container_id,
                None::<bollard::container::InspectContainerOptions>,
            )
            .await
            .map_err(map_bollard_error)?;

        let running = inspect
            .state
            .as_ref()
            .and_then(|state| state.running)
            .unwrap_or(false);
        let exit_code = inspect.state.as_ref().and_then(|state| state.exit_code);

        let mut host_ports = HashMap::new();
        if let Some(network_settings) = inspect.network_settings {
            if let Some(ports) = network_settings.ports {
                for (container_port, bindings) in ports {
                    let Some((port_raw, _)) = container_port.split_once('/') else {
                        continue;
                    };
                    let Ok(container_port) = port_raw.parse::<u16>() else {
                        continue;
                    };
                    let Some(binding) = bindings.and_then(|values| values.into_iter().next())
                    else {
                        continue;
                    };
                    let Some(host_port) = binding
                        .host_port
                        .and_then(|value| value.parse::<u16>().ok())
                    else {
                        continue;
                    };
                    host_ports.insert(container_port, host_port);
                }
            }
        }

        Ok(OciContainerInspect {
            running,
            exit_code,
            host_ports,
        })
    }

    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> Result<mpsc::Receiver<Result<OciLogChunk>>> {
        let (tx, rx) = mpsc::channel(128);
        let docker = self.docker.clone();
        let container_id = container_id.to_string();
        std::mem::drop(tokio::spawn(async move {
            let mut stream = docker.logs(
                &container_id,
                Some(LogsOptions::<String> {
                    follow,
                    stdout: true,
                    stderr: true,
                    since: 0,
                    until: 0,
                    timestamps: false,
                    tail: "all".to_string(),
                }),
            );
            while let Some(next) = stream.next().await {
                match next {
                    Ok(bollard::container::LogOutput::StdErr { message }) => {
                        if tx
                            .send(Ok(OciLogChunk {
                                stderr: true,
                                message: message.to_vec(),
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(bollard::container::LogOutput::StdOut { message })
                    | Ok(bollard::container::LogOutput::Console { message })
                    | Ok(bollard::container::LogOutput::StdIn { message }) => {
                        if tx
                            .send(Ok(OciLogChunk {
                                stderr: false,
                                message: message.to_vec(),
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(map_bollard_error(err))).await;
                        break;
                    }
                }
            }
        }));
        Ok(rx)
    }

    async fn wait_container(&self, container_id: &str) -> Result<i64> {
        let mut wait_stream = self
            .docker
            .wait_container(container_id, None::<WaitContainerOptions<String>>);
        match wait_stream.next().await {
            Some(Ok(response)) => Ok(response.status_code),
            Some(Err(BollardError::DockerContainerWaitError { code, .. })) => Ok(code),
            Some(Err(err)) => Err(map_bollard_error(err)),
            None => Ok(1),
        }
    }

    async fn stop_container(&self, container_id: &str, timeout_secs: i64) -> Result<()> {
        self.docker
            .stop_container(container_id, Some(StopContainerOptions { t: timeout_secs }))
            .await
            .map_err(map_bollard_error)
    }

    async fn remove_container(&self, container_id: &str, force: bool) -> Result<()> {
        self.docker
            .remove_container(
                container_id,
                Some(RemoveContainerOptions {
                    force,
                    ..Default::default()
                }),
            )
            .await
            .map_err(map_bollard_error)
    }
}

/// OCI(Docker/Podman) 実行のメトリクスハンドル。
pub struct OciHandle {
    session: MetricsSession,
    container_id: String,
    image_hash: String,
    docker: Docker,
    last_resources: Arc<Mutex<ResourceStats>>,
}

impl OciHandle {
    pub fn new(
        session_id: impl Into<String>,
        container_id: impl Into<String>,
        image_hash: impl Into<String>,
        docker: Docker,
    ) -> Self {
        let session = MetricsSession::new(session_id);
        let container_id = container_id.into();
        let image_hash = image_hash.into();
        let last_resources = Arc::new(Mutex::new(ResourceStats::default()));

        Self::spawn_stats_worker(
            docker.clone(),
            session.clone(),
            container_id.clone(),
            Arc::clone(&last_resources),
        );

        Self {
            session,
            container_id,
            image_hash,
            docker,
            last_resources,
        }
    }

    fn metadata(&self, exit_code: Option<i32>) -> RuntimeMetadata {
        RuntimeMetadata::Oci {
            container_id: self.container_id.clone(),
            image_hash: self.image_hash.clone(),
            exit_code,
        }
    }

    pub async fn finalize_from_cache(&self, exit_code: Option<i32>) -> UnifiedMetrics {
        let mut resources = self.last_resources.lock().await.clone();
        resources.duration_ms = self.session.elapsed_ms();
        self.session.finalize(resources, self.metadata(exit_code))
    }

    fn spawn_stats_worker(
        docker: Docker,
        session: MetricsSession,
        container_id: String,
        last_resources: Arc<Mutex<ResourceStats>>,
    ) {
        std::mem::drop(tokio::spawn(async move {
            let mut attempts = 0usize;
            loop {
                let mut got_sample = false;
                let mut stats_stream = docker.stats(
                    &container_id,
                    Some(StatsOptions {
                        stream: true,
                        one_shot: false,
                    }),
                );

                while let Some(next) = stats_stream.next().await {
                    let stats = match next {
                        Ok(value) => value,
                        Err(_) => break,
                    };

                    got_sample = true;

                    let mut resources = last_resources.lock().await;
                    resources.duration_ms = session.elapsed_ms();

                    if let Some(cpu_seconds) = extract_cpu_seconds(&stats) {
                        resources.cpu_seconds = cpu_seconds;
                    }

                    if let Some(mem_bytes) = extract_memory_bytes(&stats) {
                        resources.peak_memory_bytes = mem_bytes;
                    }
                }

                if got_sample {
                    break;
                }

                attempts += 1;
                if attempts >= 20 {
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }));
    }
}

impl RuntimeHandle for OciHandle {
    fn id(&self) -> &str {
        &self.container_id
    }

    fn kill(&mut self) -> Result<()> {
        let docker = self.docker.clone();
        let container_id = self.container_id.clone();
        let runtime =
            tokio::runtime::Runtime::new().map_err(|err| CapsuleError::Runtime(err.to_string()))?;

        runtime.block_on(async move {
            docker
                .stop_container(&container_id, Some(StopContainerOptions { t: 0 }))
                .await
                .map_err(map_bollard_error)
        })
    }
}

#[async_trait]
impl Measurable for OciHandle {
    async fn capture_metrics(&self) -> Result<UnifiedMetrics> {
        let mut resources = self.last_resources.lock().await.clone();
        if resources.duration_ms == 0 {
            resources.duration_ms = self.session.elapsed_ms();
        }
        Ok(self.session.snapshot(resources, self.metadata(None)))
    }

    async fn wait_and_finalize(&self) -> Result<UnifiedMetrics> {
        let mut wait_stream = self
            .docker
            .wait_container(&self.container_id, None::<WaitContainerOptions<String>>);
        let exit_code = match wait_stream.next().await {
            Some(Ok(response)) => Some(response.status_code as i32),
            Some(Err(BollardError::DockerContainerWaitError { code, .. })) => Some(code as i32),
            Some(Err(err)) => return Err(map_bollard_error(err)),
            None => None,
        };

        let mut resources = self.last_resources.lock().await.clone();
        resources.duration_ms = self.session.elapsed_ms();
        Ok(self.session.finalize(resources, self.metadata(exit_code)))
    }
}

fn map_bollard_error(err: BollardError) -> CapsuleError {
    let message = err.to_string();
    if is_engine_unavailable(&message) {
        return CapsuleError::ContainerEngine(message);
    }
    CapsuleError::Runtime(message)
}

pub fn connect_docker_default() -> Result<Docker> {
    if let Some(host) = resolve_docker_host() {
        if let Some(path) = host.strip_prefix("unix://") {
            return Docker::connect_with_local(path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|err| CapsuleError::ContainerEngine(err.to_string()));
        }

        if let Some(path) = host.strip_prefix("npipe://") {
            return Docker::connect_with_local(path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|err| CapsuleError::ContainerEngine(err.to_string()));
        }

        if let Some(addr) = host.strip_prefix("tcp://") {
            let http = format!("http://{}", addr);
            return Docker::connect_with_http(&http, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|err| CapsuleError::ContainerEngine(err.to_string()));
        }

        if host.starts_with("http://") {
            return Docker::connect_with_http(&host, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|err| CapsuleError::ContainerEngine(err.to_string()));
        }
    }

    Docker::connect_with_local_defaults()
        .map_err(|err| CapsuleError::ContainerEngine(err.to_string()))
}

fn resolve_docker_host() -> Option<String> {
    if let Ok(host) = std::env::var("DOCKER_HOST") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let output = Command::new("docker")
        .args([
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let host = stdout.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn is_engine_unavailable(message: &str) -> bool {
    let msg = message.to_ascii_lowercase();
    msg.contains("cannot connect")
        || msg.contains("connection refused")
        || msg.contains("is the docker daemon running")
        || msg.contains("no such file or directory")
        || msg.contains("connection error")
        || msg.contains("timed out")
}

fn extract_cpu_seconds(stats: &bollard::container::Stats) -> Option<f64> {
    let total_usage = stats.cpu_stats.cpu_usage.total_usage;
    Some(total_usage as f64 / 1_000_000_000.0)
}

fn extract_memory_bytes(stats: &bollard::container::Stats) -> Option<u64> {
    let mem = &stats.memory_stats;
    if let Some(max_usage) = mem.max_usage {
        return Some(max_usage);
    }
    if let Some(usage) = mem.usage {
        return Some(usage);
    }
    None
}
