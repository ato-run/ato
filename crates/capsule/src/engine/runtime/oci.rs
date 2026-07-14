use async_trait::async_trait;
use bollard::Docker;
use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, NetworkingConfig, RemoveContainerOptions,
    StartContainerOptions, StatsOptions, StopContainerOptions, WaitContainerOptions,
};
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, StartExecOptions};
use bollard::image::CreateImageOptions;
use bollard::models::{EndpointSettings, HostConfig, PortBinding};
use bollard::network::{ConnectNetworkOptions, CreateNetworkOptions};
use bollard::volume::RemoveVolumeOptions;
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use crate::error::{CapsuleError, Result};
use crate::metrics::{MetricsSession, ResourceStats, RuntimeMetadata, UnifiedMetrics};
use crate::runtime::{Measurable, RuntimeHandle};
use crate::types::OciPlatform;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciPortSpec {
    pub container_port: u16,
    pub host_port: Option<u16>,
    pub protocol: String,
    pub host_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciMountSpec {
    /// Left-hand side of the engine `-v` flag. Interpreted according to
    /// `source_kind`: a host filesystem path for [`OciMountSourceKind::BindPath`],
    /// or an engine-managed volume name for [`OciMountSourceKind::EngineVolume`].
    pub source: String,
    pub target: String,
    pub readonly: bool,
    /// If set, the provider should use engine-delegated ownership init (e.g.
    /// Podman `:U`) so the container user can write to this mount.
    /// `None` means no ownership strategy is requested.
    pub ownership: Option<crate::types::MountOwnership>,
    /// How the provider should interpret `source`: a host bind path or an
    /// engine-managed named volume.
    ///
    /// On Windows + rootless Podman, Ato-managed writable state must use an
    /// engine-managed volume rather than a host bind mount: the Windows host
    /// filesystem has no POSIX ownership/permission semantics, so a non-root
    /// container user (or `postgres initdb`) cannot `chmod`/`chown` a bind-mounted
    /// host directory and stateful recipes fail to start. Named volumes live
    /// inside the engine's own Linux filesystem, where copy-up and `:U` give the
    /// container user a writable, correctly-owned directory. See #444.
    pub source_kind: OciMountSourceKind,
}

/// Whether an [`OciMountSpec`] source is a host bind path or an engine-managed
/// named volume. See [`OciMountSpec::source_kind`] and #444.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OciMountSourceKind {
    /// `source` is a host filesystem path; the provider creates a bind mount.
    #[default]
    BindPath,
    /// `source` is an engine-managed named volume living inside the engine's own
    /// Linux filesystem.
    ///
    /// `remove_on_stop` marks ephemeral volumes that cleanup should delete;
    /// persistent volumes are left intact across stops so durable state survives.
    EngineVolume { remove_on_stop: bool },
}

impl OciMountSpec {
    /// True when `source` names an engine-managed volume rather than a host path.
    pub fn is_engine_volume(&self) -> bool {
        matches!(self.source_kind, OciMountSourceKind::EngineVolume { .. })
    }
}

/// True when `source` is an Ato-managed state directory — either an ephemeral
/// state path or a path under the durable state root — rather than an explicit
/// user-supplied host path.
///
/// Only Ato-managed sources are eligible for the engine-managed volume strategy:
/// when a user pins an explicit host path we honor it as a bind mount. See #444.
pub fn is_ato_managed_state_source(source: &str) -> bool {
    is_ephemeral_state_source(source) || {
        let state_root = crate::common::paths::ato_state_dir();
        path_starts_with(source, &state_root)
    }
}

/// True when `source` lives under the ephemeral state base (state that is safe
/// to discard when the session stops). See #444.
pub fn is_ephemeral_state_source(source: &str) -> bool {
    let base = crate::types::default_ephemeral_state_base();
    path_starts_with(source, std::path::Path::new(&base))
}

fn path_starts_with(source: &str, prefix: &std::path::Path) -> bool {
    !prefix.as_os_str().is_empty() && std::path::Path::new(source).starts_with(prefix)
}

/// Select the source strategy (host bind path vs engine-managed volume) for one
/// state-binding mount and build the resulting [`OciMountSpec`].
///
/// On **Windows + Podman**, an Ato-managed *writable* state binding uses an
/// engine-managed named volume instead of a host bind mount: the Windows host
/// filesystem has no POSIX ownership/permission semantics, so a non-root
/// container user (or `postgres initdb`) cannot `chmod`/`chown` a bind-mounted
/// host directory and stateful recipes (node-red, blinko) fail to start. Named
/// volumes live in the engine's own Linux filesystem, where copy-up and `:U`
/// give the container user a writable, correctly-owned directory.
///
/// Everything else stays a bind mount, preserving existing behavior:
/// * explicit user-supplied host paths (not Ato-managed) — honor the path,
/// * read-only mounts — never re-homed to a volume,
/// * non-Windows hosts and non-Podman engines — unchanged.
///
/// `ownership` is carried through unchanged so the provider can still request
/// engine-delegated ownership init (`:U`) on the resulting mount.
///
/// The engine volume name is derived from the source-path identity (not the
/// session id) so persistent state maps to a stable volume across restarts;
/// ephemeral state is marked `remove_on_stop` so cleanup deletes it.
///
/// This is shared by both OCI execution paths — the multi-service executor
/// (`OciProvider`) and the Desktop session orchestrator (`OciRuntimeClient`) —
/// so the strategy is identical regardless of which path materializes the
/// container. See #444.
pub fn resolve_oci_mount(
    mount: &crate::types::Mount,
    is_podman: bool,
    is_windows_host: bool,
) -> OciMountSpec {
    let use_engine_volume = is_windows_host
        && is_podman
        && !mount.readonly
        && is_ato_managed_state_source(&mount.source);

    if use_engine_volume {
        OciMountSpec {
            source: engine_state_volume_name(&mount.source),
            target: mount.target.clone(),
            readonly: mount.readonly,
            ownership: mount.ownership.clone(),
            source_kind: OciMountSourceKind::EngineVolume {
                remove_on_stop: is_ephemeral_state_source(&mount.source),
            },
        }
    } else {
        OciMountSpec {
            source: mount.source.clone(),
            target: mount.target.clone(),
            readonly: mount.readonly,
            ownership: mount.ownership.clone(),
            source_kind: OciMountSourceKind::BindPath,
        }
    }
}

/// Build a stable, sanitized engine volume name for an Ato-managed state mount.
///
/// The name is derived from the *source-path identity*, not the session id, so
/// the same persistent state binding maps to the same volume across restarts
/// (and across worktrees only insofar as their state paths differ). The result
/// is restricted to characters valid in a Podman/Docker volume name
/// (`[a-zA-Z0-9][a-zA-Z0-9_.-]*`). See #444.
pub fn engine_state_volume_name(source: &str) -> String {
    let hash = fnv1a_hex(source);
    let leaf = source
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("state");
    let sanitized: String = leaf
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let leaf = sanitized.trim_matches('-');
    let leaf = if leaf.is_empty() { "state" } else { leaf };
    format!("ato-state-{}-{}", &hash[..12], leaf)
}

/// Deterministic FNV-1a (64-bit) hash rendered as lowercase hex. Used for stable
/// volume names; intentionally not a cryptographic hash.
fn fnv1a_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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
    /// Optional platform override for emulated execution (e.g., linux/amd64 on arm64 host).
    /// When set and different from host platform, the provider must pass `--platform` to create.
    pub platform: Option<OciPlatform>,
    /// Additional `/etc/hosts` entries injected via `--add-host`.
    ///
    /// Each entry is in `name:address` form as accepted by `podman create --add-host`.
    /// Use `host.containers.internal:host-gateway` to let containers reach the host.
    pub extra_hosts: Vec<String>,
    /// Optional container user (`--user`): `"uid"`, `"uid:gid"`, or a name the
    /// image resolves. `None` keeps the image's baked-in `USER`. See #428.
    pub user: Option<String>,
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
    /// Create a network that preserves service-to-service connectivity while
    /// denying external routing.
    async fn create_internal_network(&self, _request: &OciNetworkRequest) -> Result<String> {
        Err(CapsuleError::Runtime(
            "internal OCI networks are unsupported by this runtime client".to_string(),
        ))
    }
    async fn remove_network(&self, network_name: &str) -> Result<()>;
    async fn create_container(&self, request: &OciContainerRequest) -> Result<String>;
    async fn start_container(&self, container_id: &str) -> Result<()>;
    async fn inspect_container(&self, container_id: &str) -> Result<OciContainerInspect>;
    async fn logs(
        &self,
        container_id: &str,
        follow: bool,
    ) -> Result<mpsc::Receiver<Result<OciLogChunk>>>;
    async fn exec_container(&self, container_id: &str, cmd: &[String]) -> Result<i64>;
    async fn wait_container(&self, container_id: &str) -> Result<i64>;
    async fn stop_container(&self, container_id: &str, timeout_secs: i64) -> Result<()>;
    async fn remove_container(&self, container_id: &str, force: bool) -> Result<()>;

    /// Remove an engine-managed named volume. Default is a no-op so clients that
    /// never create volumes need not implement it. Used by cleanup to delete
    /// ephemeral state volumes. See #444.
    async fn remove_volume(&self, _volume_name: &str) -> Result<()> {
        Ok(())
    }

    /// Whether the engine behind this client is Podman (vs Docker). Used to
    /// select the Windows engine-managed-volume mount strategy. Default `false`.
    /// See #444.
    async fn is_podman(&self) -> bool {
        false
    }
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

    async fn is_podman_engine(&self) -> bool {
        let Ok(version) = self.docker.version().await else {
            return false;
        };
        let platform = version
            .platform
            .map(|platform| platform.name)
            .unwrap_or_default();
        let version = version.version.unwrap_or_default();
        let marker = format!("{platform} {version}").to_ascii_lowercase();
        marker.contains("podman")
    }

    async fn create_network_with_internal(
        &self,
        request: &OciNetworkRequest,
        internal: bool,
    ) -> Result<String> {
        let response = match self
            .docker
            .create_network(CreateNetworkOptions {
                name: request.name.clone(),
                check_duplicate: true,
                driver: "bridge".to_string(),
                internal,
                attachable: true,
                ingress: false,
                ipam: Default::default(),
                enable_ipv6: false,
                options: HashMap::<String, String>::new(),
                labels: request.labels.clone(),
            })
            .await
        {
            Ok(response) => response,
            Err(err) if is_bollard_eof(&err) && self.is_podman_engine().await => {
                return create_podman_network_cli(request, internal).await;
            }
            Err(err) => return Err(map_bollard_error(err)),
        };

        response
            .id
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                CapsuleError::Runtime("OCI network create returned empty id".to_string())
            })
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
        self.create_network_with_internal(request, false).await
    }

    async fn create_internal_network(&self, request: &OciNetworkRequest) -> Result<String> {
        self.create_network_with_internal(request, true).await
    }

    async fn remove_network(&self, network_name: &str) -> Result<()> {
        match self.docker.remove_network(network_name).await {
            Ok(()) => Ok(()),
            Err(err) if is_bollard_eof(&err) && self.is_podman_engine().await => {
                remove_podman_network_cli(network_name).await
            }
            Err(err) => Err(map_bollard_error(err)),
        }
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
                    host_port: Some(
                        port.host_port
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    ),
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
            extra_hosts: (!request.extra_hosts.is_empty()).then(|| request.extra_hosts.clone()),
            network_mode: request.network.clone(),
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
                    user: request.user.clone(),
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
        if let Some(network_settings) = inspect.network_settings
            && let Some(ports) = network_settings.ports
        {
            for (container_port, bindings) in ports {
                let Some((port_raw, _)) = container_port.split_once('/') else {
                    continue;
                };
                let Ok(container_port) = port_raw.parse::<u16>() else {
                    continue;
                };
                let Some(binding) = bindings.and_then(|values| values.into_iter().next()) else {
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

    async fn exec_container(&self, container_id: &str, cmd: &[String]) -> Result<i64> {
        let exec = self
            .docker
            .create_exec(
                container_id,
                CreateExecOptions {
                    attach_stdin: Some(false),
                    attach_stdout: Some(false),
                    attach_stderr: Some(false),
                    tty: Some(false),
                    cmd: Some(cmd.to_vec()),
                    ..Default::default()
                },
            )
            .await
            .map_err(map_bollard_error)?;

        self.docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: true,
                    ..Default::default()
                }),
            )
            .await
            .map_err(map_bollard_error)?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let inspect = self
                .docker
                .inspect_exec(&exec.id)
                .await
                .map_err(map_bollard_error)?;
            if !inspect.running.unwrap_or(false) {
                return Ok(inspect.exit_code.unwrap_or(1));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(1);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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

    async fn remove_volume(&self, volume_name: &str) -> Result<()> {
        // `force: true` so removing an already-gone volume is a no-op rather
        // than an error (idempotent cleanup). See #444.
        match self
            .docker
            .remove_volume(volume_name, Some(RemoveVolumeOptions { force: true }))
            .await
        {
            Ok(()) => Ok(()),
            Err(err) if is_bollard_eof(&err) && self.is_podman_engine().await => {
                remove_podman_volume_cli(volume_name).await
            }
            Err(err) => Err(map_bollard_error(err)),
        }
    }

    async fn is_podman(&self) -> bool {
        self.is_podman_engine().await
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

fn is_bollard_eof(err: &BollardError) -> bool {
    err.to_string()
        .to_ascii_lowercase()
        .contains("eof while parsing a value")
}

/// Build a tokio `Command` for podman, resolved to an absolute binary with a
/// `PATH` override so GUI-launched (minimal-PATH) processes find Homebrew/
/// known-location Podman. Falls back to the bare `"podman"` name when
/// resolution fails.
fn podman_cli_command() -> tokio::process::Command {
    let invocation = crate::foundation::podman::podman_invocation();
    let mut command = tokio::process::Command::new(&invocation.program);
    if let Some(path_env) = &invocation.path_env {
        command.env("PATH", path_env);
    }
    // An Ato-managed Podman carries its own containers.conf pointing at the
    // bundled machine helpers (gvproxy/vfkit); honour it so the machine the CLI
    // runs against matches what runtime-setup configured.
    if let Some(containers_conf) = &invocation.containers_conf {
        command.env("CONTAINERS_CONF", containers_conf);
    }
    command
}

async fn create_podman_network_cli(request: &OciNetworkRequest, internal: bool) -> Result<String> {
    let mut args = vec!["network".to_string(), "create".to_string()];
    if internal {
        args.push("--opt".to_string());
        args.push("no_default_route=1".to_string());
    }
    for (key, value) in &request.labels {
        args.push("--label".to_string());
        args.push(format!("{key}={value}"));
    }
    args.push(request.name.clone());

    let output = podman_cli_command()
        .args(&args)
        .output()
        .await
        .map_err(|err| {
            CapsuleError::ContainerEngine(format!("failed to run podman network create: {err}"))
        })?;
    if !output.status.success() {
        return Err(CapsuleError::Runtime(format!(
            "podman network create failed for '{}': {}",
            request.name,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if id.is_empty() {
        Ok(request.name.clone())
    } else {
        Ok(id)
    }
}

async fn remove_podman_network_cli(network_name: &str) -> Result<()> {
    let output = podman_cli_command()
        .args(["network", "rm", network_name])
        .output()
        .await
        .map_err(|err| {
            CapsuleError::ContainerEngine(format!("failed to run podman network rm: {err}"))
        })?;
    if output.status.success() {
        return Ok(());
    }

    Err(CapsuleError::Runtime(format!(
        "podman network rm failed for '{}': {}",
        network_name,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

async fn remove_podman_volume_cli(volume_name: &str) -> Result<()> {
    let output = podman_cli_command()
        .args(["volume", "rm", "--force", volume_name])
        .output()
        .await
        .map_err(|err| {
            CapsuleError::ContainerEngine(format!("failed to run podman volume rm: {err}"))
        })?;
    if output.status.success() {
        return Ok(());
    }

    Err(CapsuleError::Runtime(format!(
        "podman volume rm failed for '{}': {}",
        volume_name,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
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

#[cfg(test)]
mod mount_source_tests {
    use super::*;

    #[test]
    fn engine_volume_name_is_stable_for_same_source() {
        let a = engine_state_volume_name("/var/lib/ato/state/blinko/pgdata");
        let b = engine_state_volume_name("/var/lib/ato/state/blinko/pgdata");
        assert_eq!(a, b, "name must be deterministic across calls");
    }

    #[test]
    fn engine_volume_name_differs_for_different_sources() {
        let a = engine_state_volume_name("/var/lib/ato/state/blinko/pgdata");
        let b = engine_state_volume_name("/var/lib/ato/state/blinko/uploads");
        assert_ne!(a, b);
    }

    #[test]
    fn engine_volume_name_is_a_valid_volume_identifier() {
        // Podman/Docker volume names: [a-zA-Z0-9][a-zA-Z0-9_.-]*
        let name = engine_state_volume_name("/var/lib/ato/state/My App/data dir!");
        assert!(name.starts_with("ato-state-"));
        let mut chars = name.chars();
        let first = chars.next().unwrap();
        assert!(first.is_ascii_alphanumeric(), "first char must be alnum");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'),
            "name has invalid chars: {name}"
        );
        // The sanitized leaf preserves the human-readable tail.
        assert!(name.ends_with("-data-dir") || name.ends_with("-data-dir-"));
    }

    #[test]
    fn ephemeral_source_is_detected_under_ephemeral_base() {
        let base = crate::types::default_ephemeral_state_base();
        let source = format!("{}/node-red/data", base.trim_end_matches(['/', '\\']));
        assert!(is_ephemeral_state_source(&source));
        assert!(is_ato_managed_state_source(&source));
    }

    #[test]
    fn explicit_host_path_is_not_managed() {
        assert!(!is_ato_managed_state_source("/explicit/user/data"));
        assert!(!is_ephemeral_state_source("/explicit/user/data"));
    }

    #[test]
    fn durable_state_root_path_is_managed_but_not_ephemeral() {
        let source = crate::common::paths::ato_state_dir()
            .join("blinko")
            .join("pgdata")
            .to_string_lossy()
            .to_string();
        assert!(is_ato_managed_state_source(&source));
        // The durable root is not under the ephemeral base.
        assert!(!is_ephemeral_state_source(&source));
    }

    // ── resolve_oci_mount: Windows/Podman engine-volume strategy (#444) ───────

    fn managed_persistent_mount(target: &str) -> crate::types::Mount {
        // A path under the durable state root → Ato-managed, persistent.
        let source = crate::common::paths::ato_state_dir()
            .join("blinko")
            .join("pgdata")
            .to_string_lossy()
            .to_string();
        crate::types::Mount {
            source,
            target: target.to_string(),
            readonly: false,
            ownership: None,
        }
    }

    fn managed_ephemeral_mount(target: &str) -> crate::types::Mount {
        // A path under the ephemeral state base → Ato-managed, ephemeral.
        let base = crate::types::default_ephemeral_state_base();
        let source = format!("{}/node-red/data", base.trim_end_matches(['/', '\\']));
        crate::types::Mount {
            source,
            target: target.to_string(),
            readonly: false,
            ownership: None,
        }
    }

    fn explicit_host_mount(target: &str) -> crate::types::Mount {
        crate::types::Mount {
            source: "/explicit/user/data".to_string(),
            target: target.to_string(),
            readonly: false,
            ownership: None,
        }
    }

    #[test]
    fn windows_podman_managed_persistent_is_persistent_engine_volume() {
        let m = managed_persistent_mount("/var/lib/postgresql/data");
        let spec = resolve_oci_mount(&m, true, true);
        assert_eq!(
            spec.source_kind,
            OciMountSourceKind::EngineVolume {
                remove_on_stop: false
            }
        );
        assert!(spec.source.starts_with("ato-state-"));
        assert_ne!(spec.source, m.source);
    }

    #[test]
    fn windows_podman_managed_ephemeral_removes_on_stop() {
        let m = managed_ephemeral_mount("/data");
        let spec = resolve_oci_mount(&m, true, true);
        assert_eq!(
            spec.source_kind,
            OciMountSourceKind::EngineVolume {
                remove_on_stop: true
            }
        );
    }

    #[test]
    fn windows_podman_explicit_host_path_stays_bind() {
        let m = explicit_host_mount("/data");
        let spec = resolve_oci_mount(&m, true, true);
        assert_eq!(spec.source_kind, OciMountSourceKind::BindPath);
        assert_eq!(spec.source, m.source);
    }

    #[test]
    fn non_windows_podman_managed_stays_bind() {
        let m = managed_persistent_mount("/data");
        let spec = resolve_oci_mount(&m, true, false);
        assert_eq!(spec.source_kind, OciMountSourceKind::BindPath);
        assert_eq!(spec.source, m.source);
    }

    #[test]
    fn windows_non_podman_managed_stays_bind() {
        let m = managed_persistent_mount("/data");
        let spec = resolve_oci_mount(&m, false, true);
        assert_eq!(spec.source_kind, OciMountSourceKind::BindPath);
    }

    #[test]
    fn windows_podman_readonly_managed_stays_bind() {
        let mut m = managed_persistent_mount("/data");
        m.readonly = true;
        let spec = resolve_oci_mount(&m, true, true);
        assert_eq!(spec.source_kind, OciMountSourceKind::BindPath);
    }

    #[test]
    fn node_red_data_mount_is_engine_volume_on_windows() {
        // node-red binds /data; ephemeral managed state → engine volume.
        let m = managed_ephemeral_mount("/data");
        let spec = resolve_oci_mount(&m, true, true);
        assert!(spec.is_engine_volume());
    }

    #[test]
    fn blinko_pgdata_mount_is_engine_volume_on_windows() {
        // blinko's postgres binds /var/lib/postgresql/data; persistent managed
        // state → engine volume, ownership carried through.
        let mut m = managed_persistent_mount("/var/lib/postgresql/data");
        m.ownership = Some(crate::types::MountOwnership {
            uid: Some(999),
            gid: Some(999),
            recursive: false,
            mode: Some(0o700),
        });
        let spec = resolve_oci_mount(&m, true, true);
        assert!(spec.is_engine_volume());
        assert_eq!(spec.ownership.as_ref().unwrap().uid, Some(999));
    }

    #[test]
    fn engine_volume_spec_reports_is_engine_volume() {
        let spec = OciMountSpec {
            source: "ato-state-x-data".to_string(),
            target: "/data".to_string(),
            readonly: false,
            ownership: None,
            source_kind: OciMountSourceKind::EngineVolume {
                remove_on_stop: true,
            },
        };
        assert!(spec.is_engine_volume());

        let bind = OciMountSpec {
            source_kind: OciMountSourceKind::BindPath,
            ..spec
        };
        assert!(!bind.is_engine_volume());
    }
}
