//! Trusted display materialization. No helper image, cookie or host endpoint
//! belongs to K/D. Lease dispatch and typed capability advertisement require
//! explicit trusted Runner configuration and verified local physical admission.
use super::*;
use ato_adapter_oci::{OciMount, OciResourceLimits, OciServiceGroup, OciSpec, OciUser};
use ato_objects::PortableOciArchive;
use ato_portable_application::oci_archive::verify_oci_archive;
use netd::pixel_gateway::{PixelGatewayConfig, PixelGatewayControls};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tokio::{
    net::{TcpListener, UnixStream},
    task::JoinSet,
};

const DISPLAY: &str = ":64";
const X11: &str = "/tmp/.X11-unix";
const AUTH: &str = "/run/ato-native-auth";
const RFB: &str = "/run/ato-native-rfb";
const HELPER: &str = "/usr/local/lib/ato/native-display";

/// Physical identity comes from the existing trusted HostBoundary, never D.
/// Ordinary local Runners retain the owner of their private runtime directory.
fn helper_user(directory: &Path) -> Result<OciUser> {
    let user = if let Some(boundary) = super::super::host_boundary::active() {
        OciUser {
            uid: boundary.uid,
            gid: boundary.gid,
        }
    } else {
        let owner = std::fs::metadata(directory)?;
        OciUser {
            uid: owner.uid(),
            gid: owner.gid(),
        }
    };
    user.validate()
}

fn validate_application_user(user: OciUser) -> Result<()> {
    user.validate()?;
    if let Some(boundary) = super::super::host_boundary::active() {
        ensure!(
            user.uid == boundary.uid && user.gid == boundary.gid,
            "oci_user_state_owner_mismatch: Native GUI requires the configured workload identity"
        );
    }
    Ok(())
}

/// Explicit trusted Runner configuration, default absent. Only a verified
/// immutable OCI archive is admitted; no registry fallback, mutable tag, shell
/// supplied by the application, or image package installation occurs at Run.
/// Deliberately neither Debug nor Serialize (its archive is not a receipt).
#[derive(Clone)]
pub struct PixelHelper {
    image: String,
    platform: String,
    archive: Arc<Vec<u8>>,
    config_reference: String,
    limits: OciResourceLimits,
    owner: Option<super::pixel_capacity::PixelOwner>,
    prepared_user: Option<OciUser>,
    controls: PixelGatewayControls,
}
impl PixelHelper {
    pub fn from_archive(archive: PortableOciArchive, limits: OciResourceLimits) -> Result<Self> {
        Self::from_archive_with_controls(archive, limits, PixelGatewayControls::default())
    }

    pub fn from_archive_with_controls(
        archive: PortableOciArchive,
        limits: OciResourceLimits,
        controls: PixelGatewayControls,
    ) -> Result<Self> {
        ensure!(
            (64 * 1024 * 1024..=1024 * 1024 * 1024).contains(&limits.memory_bytes)
                && (100..=2000).contains(&limits.cpu_limit_millis)
                && (8..=128).contains(&limits.pids_limit),
            "invalid bounded display helper resources"
        );
        let verified = verify_oci_archive(&archive)?;
        if controls.text_paste || controls.framebuffer_resize {
            ensure!(
                super::pixel_controls::verified_controls_abi(&verified)?,
                "native_surface_controls_unsupported: trusted helper controls ABI is unavailable"
            );
        }
        Ok(Self {
            image: archive.image,
            platform: archive.platform,
            archive: Arc::new(verified.bytes),
            config_reference: verified.config_reference,
            limits,
            owner: None,
            prepared_user: None,
            controls,
        })
    }

    pub(crate) fn controls(&self) -> PixelGatewayControls {
        self.controls
    }

    fn launch_argv(&self, width: u32, height: u32) -> Vec<String> {
        let mut argv = vec![width.to_string(), height.to_string()];
        if self.controls.text_paste || self.controls.framebuffer_resize {
            argv.extend([
                "--controls-v1".into(),
                u8::from(self.controls.text_paste).to_string(),
                u8::from(self.controls.framebuffer_resize).to_string(),
            ]);
        }
        argv
    }

    pub(crate) fn with_runtime_user(mut self, work_root: &Path) -> Result<Self> {
        self.prepared_user = Some(helper_user(work_root)?);
        Ok(self)
    }

    pub(crate) fn with_local_owner(mut self, socket: PathBuf, parent: String) -> Result<Self> {
        self.owner = Some(super::pixel_capacity::PixelOwner::new(socket, parent)?);
        Ok(self)
    }

    /// No image load or workload creation here: rollout requires explicit
    /// prematerialization and exact verified config/platform already present.
    pub(crate) fn check_ready(&self, work_root: &Path) -> Result<()> {
        let owner = self
            .owner
            .as_ref()
            .context("Pixel omitted local owner binding")?;
        owner.admit(
            &OciResourceLimits {
                memory_bytes: 0,
                cpu_limit_millis: 0,
                pids_limit: 0,
            },
            &self.limits,
        )?;
        let spec = OciSpec {
            id: "native-display-admission".into(),
            image: self.image.clone(),
            platform: self.platform.clone(),
            entrypoint: Some(HELPER.into()),
            user: Some(
                self.prepared_user
                    .context("Pixel omitted prepared workload identity")?,
            ),
            argv: self.launch_argv(320, 200),
            working_dir: "/run".into(),
            workspace_mount_path: "/run/ato-unused-workspace".into(),
            environment: BTreeMap::new(),
            endpoints: vec![],
            mounts: vec![],
            limits: self.limits.clone(),
            stop_timeout_seconds: 1,
            labels: BTreeMap::new(),
        };
        let config = tempfile::Builder::new()
            .prefix(".native-display-admission-")
            .tempdir_in(work_root)?;
        DockerOciAdapter::new_offline(spec, vec![], self.config_reference.clone())?
            .with_cgroup_parent(owner.docker_parent.clone())?
            .with_isolated_owner(&owner.socket, &config.path().join("docker"))?
            .with_control(ato_adapter_oci::command_control::CommandControl::cleanup(
                Duration::from_millis(500),
            ))
            .admit()?;
        Ok(())
    }

    pub(crate) fn admit_spec(&self, spec: &RuntimeLaunchSpecV1) -> Result<()> {
        let LaunchRealizationV1::Oci(oci) = &spec.realization else {
            anyhow::bail!("Pixel requires OCI");
        };
        let user = oci
            .user
            .context("native GUI requires explicit nonroot user")?;
        validate_application_user(OciUser {
            uid: user.uid,
            gid: user.gid,
        })?;
        let limits = oci
            .resource_limits
            .as_ref()
            .context("Pixel requires explicit app limits")?;
        self.owner
            .as_ref()
            .context("Pixel omitted local owner binding")?
            .admit(
                &OciResourceLimits {
                    memory_bytes: limits.memory_bytes,
                    cpu_limit_millis: limits.cpu_limit_millis,
                    pids_limit: limits.pids_limit,
                },
                &self.limits,
            )?;
        Ok(())
    }
}

/// Only these child directories are bind mounted; the owning parent remains
/// host-private. The wildcard Xauthority record is necessary across the two
/// isolated container hostnames. Read access is delegated only into this Run.
pub(super) struct DisplayDirectories {
    root: PathBuf,
    retain: bool,
}
impl DisplayDirectories {
    fn create(runtime: &Path) -> Result<Self> {
        std::fs::create_dir_all(runtime)?;
        std::fs::set_permissions(runtime, std::fs::Permissions::from_mode(0o700))?;
        let user = helper_user(runtime)?;
        let root = runtime.join("display");
        std::fs::create_dir(&root).context("create fresh owned display directory")?;
        let dirs = Self {
            root,
            retain: false,
        };
        std::fs::set_permissions(&dirs.root, std::fs::Permissions::from_mode(0o700))?;
        for (name, mode) in [
            ("x11", 0o755),
            ("auth", 0o755),
            ("rfb", 0o700),
            ("empty", 0o755),
        ] {
            let path = dirs.root.join(name);
            std::fs::create_dir(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
            if super::super::host_boundary::active().is_some() && matches!(name, "x11" | "rfb") {
                // Fresh owned children only. The credential/runtime parent,
                // workspace and Runner configuration remain Runner-owned.
                std::os::unix::fs::chown(&path, Some(user.uid), Some(user.gid))?;
            }
        }
        // Xauthority binary format: FamilyWild, address, display, protocol,
        // and a cryptographically random cookie. No command/log contains it.
        let cookie: [u8; 16] = rand::random();
        let mut bytes = 0xffff_u16.to_be_bytes().to_vec();
        for field in [b"".as_slice(), b"64", b"MIT-MAGIC-COOKIE-1", &cookie] {
            bytes.extend_from_slice(&(field.len() as u16).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o444)
            .open(dirs.root.join("auth/Xauthority"))?;
        file.write_all(&bytes)?;
        std::fs::set_permissions(
            dirs.root.join("auth/Xauthority"),
            std::fs::Permissions::from_mode(0o444),
        )?;
        Ok(dirs)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub(super) fn retain_for_recovery(mut self) {
        self.retain = true;
    }
}
impl Drop for DisplayDirectories {
    fn drop(&mut self) {
        // Even an unconfirmed stop must never preserve the credential in a
        // future snapshot. Recovery retains socket directories, not the cookie.
        let _ = std::fs::remove_file(self.root.join("auth/Xauthority"));
        if !self.retain {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// Private bounded TCP -> owned Unix socket bridge for the existing RFB
/// probe/gateway. Its JoinSet owns every accepted stream; cancellation closes
/// both halves without detached forwarding threads or live guest sockets.
pub(super) struct RfbRelay {
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl RfbRelay {
    async fn start(path: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    Some(_) = clients.join_next(), if !clients.is_empty() => {},
                    accepted = listener.accept(), if clients.len() < 8 => {
                        let Ok((mut incoming,_)) = accepted else { break };
                        if incoming.set_nodelay(true).is_err() {continue;}
                        let path = path.clone();
                        clients.spawn(async move {
                            if let Ok(Ok(mut upstream)) = tokio::time::timeout(Duration::from_secs(1),UnixStream::connect(path)).await {
                                // The enclosing owner/idle/lifetime actor owns
                                // this JoinSet, including incomplete handshakes.
                                let _ = tokio::io::copy_bidirectional(&mut incoming,&mut upstream).await;
                            }
                        });
                    }
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Ok(Self {
            addr,
            stop: Some(stop),
            task: Some(task),
        })
    }
    pub(super) fn failed(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }
    pub(super) async fn stop(&mut self) -> Result<()> {
        self.stop.take();
        if let Some(mut task) = self.task.take() {
            match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
                Ok(result) => result?,
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    anyhow::bail!("RFB relay cleanup timeout")
                }
            }
        }
        Ok(())
    }
}
impl Drop for RfbRelay {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn reserved_mount(path: &str) -> bool {
    [X11, AUTH, RFB].iter().any(|reserved| {
        path == *reserved
            || path.starts_with(&format!("{reserved}/"))
            || reserved.starts_with(&format!("{}/", path.trim_end_matches('/')))
    })
}
fn validate_app(app: &OciSpec) -> Result<()> {
    validate_application_user(
        app.user
            .context("native GUI requires explicit nonroot user")?,
    )?;
    ensure!(
        app.endpoints.is_empty(),
        "host GUI cannot erase declared OCI endpoints"
    );
    ensure!(
        !app.environment.contains_key("DISPLAY") && !app.environment.contains_key("XAUTHORITY"),
        "native display owns DISPLAY and XAUTHORITY"
    );
    ensure!(
        !reserved_mount(&app.workspace_mount_path)
            && app.mounts.iter().all(|m| !reserved_mount(&m.guest_path)),
        "declared mount conflicts with native display"
    );
    Ok(())
}

/// Starts an isolated application display, then the unmodified application's
/// argv, then actual authored probes, and finally the existing scoped gateway.
/// It returns physical readiness only. The caller must use the existing Rust
/// K verifier and receipt authority before publication; dispatch stays closed.
#[allow(clippy::too_many_arguments)]
pub async fn start_pixel(
    spec: &RuntimeLaunchSpecV1,
    selected_ref: &str,
    descriptor: &SessionSurfaceDescriptor,
    context: &ResolvedRuntimeLaunchContext,
    owner: &OciOwner,
    config: NativeLeaseSurfaceConfig,
    helper: PixelHelper,
    application_archive: Option<PortableOciArchive>,
    mut owner_alive: watch::Receiver<bool>,
) -> std::result::Result<OwnedNativeTerminalRun, super::super::lease::StartFailure> {
    let started = Instant::now();
    // All declaration/config/key/scope checks precede mounts, native actions
    // and container creation, just as the Terminal path.
    let (
        declaration,
        mut gateway,
        keys,
        execution,
        authorizer,
        helper_adapter,
        app_adapter,
        dirs,
        runtime,
        socket_directory,
    ) = (|| -> Result<_> {
        spec.validate().map_err(anyhow::Error::new)?;
        ensure!(
            cfg!(target_os = "linux"),
            "native GUI requires Linux Runner"
        );
        ensure!(
            *owner_alive.borrow() && owner.run_id == spec.context.run_id,
            "native owner disconnected or Run mismatch"
        );
        let LaunchRealizationV1::Oci(oci) = &spec.realization else {
            anyhow::bail!("native GUI requires OCI")
        };
        let declaration = NativeLaunchDeclaration::decode(
            oci.native_surface
                .as_ref()
                .context("missing native declaration")?,
        )?;
        declaration.validate_selection(selected_ref, descriptor)?;
        declaration.require_host_pixel_ports()?;
        let SessionSurfaceDescriptor::PixelStream { viewport, .. } = descriptor else {
            unreachable!()
        };
        ensure!(
            (320..=4096).contains(&viewport.width) && (200..=2160).contains(&viewport.height),
            "unsupported display geometry"
        );
        ensure!(
            config.scope.surface_id == descriptor.surface_id().unwrap_or_default(),
            "native surface owner mismatch"
        );
        ensure!(
            config.surface_listen.ip().is_loopback() && config.surface_listen.port() != 0,
            "private assigned gateway required"
        );
        ensure!(
            (Duration::from_secs(1)..=Duration::from_secs(120)).contains(&config.startup_timeout)
                && (Duration::from_secs(1)..=Duration::from_secs(86400))
                    .contains(&config.idle_timeout)
                && (Duration::from_secs(1)..=Duration::from_secs(86400))
                    .contains(&config.maximum_lifetime),
            "invalid bounded native lifetime"
        );
        ensure!(
            spec.lifecycle.graceful_shutdown_ms <= 10000
                && spec.lifecycle.force_kill_after_ms <= 15000,
            "native cleanup exceeds supported budget"
        );
        ensure!(
            helper.controls.text_paste == config.controls.text_paste
                && helper.controls.framebuffer_resize == config.controls.framebuffer_resize,
            "native_surface_controls_unsupported: helper and guarded gateway masks differ"
        );
        let gateway = PixelGatewayConfig {
            listen_addr: config.surface_listen,
            private_rfb_addr: "127.0.0.1:1".parse().unwrap(),
            private_rfb_connect_timeout: config.startup_timeout,
            scope: config.scope.clone(),
            allowed_origins: BTreeSet::from([config.origin.clone()]),
        };
        gateway.validate()?;
        let keys = Arc::new(SurfaceAssertionKeyring::new(BTreeMap::from([(
            "ato.runner-surface@1".into(),
            config.verification_key,
        )]))?);
        let execution = SurfaceExecutionScope {
            run_id: owner.run_id.clone(),
            lease_id: owner.lease_id.clone(),
            runner_id: owner.runner_id.clone(),
        };
        let authorizer = Arc::new(
            HmacSurfaceAccessAuthorizer::new(Arc::clone(&keys))
                .with_execution_scope(execution.clone()),
        );
        ensure!(
            spec.public_env
                .iter()
                .all(|entry| entry.name != "DISPLAY" && entry.name != "XAUTHORITY")
                && spec
                    .secret_grants
                    .iter()
                    .all(|entry| entry.name != "DISPLAY" && entry.name != "XAUTHORITY"),
            "native display owns declared DISPLAY and XAUTHORITY"
        );
        let mut physical = oci.clone();
        physical.native_surface = None;
        let mut app = container_oci_spec(spec, &physical, context, owner.labels(Some("main"))?)?;
        validate_app(&app)?;
        if let Some(owner) = &helper.owner {
            owner.admit(&app.limits, &helper.limits)?;
        }
        ensure!(
            helper.platform == app.platform,
            "display helper platform mismatch"
        );
        let application_archive = application_archive
            .map(|archive| {
                ensure!(
                    archive.image == app.image && archive.platform == app.platform,
                    "GUI application archive differs from selected D"
                );
                verify_oci_archive(&archive)
            })
            .transpose()?;
        let runtime = ato_runtime_attempt::launch::oci::lease_runtime_root(context)?;
        let dirs = DisplayDirectories::create(&runtime)?;
        let metadata = std::fs::metadata(dirs.path("x11"))?;
        let user = OciUser {
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
        .validate()?;
        ensure!(
            helper.prepared_user.is_none_or(|prepared| prepared == user),
            "Native display identity differs from prepared helper"
        );
        let mounts = |writable: bool| {
            vec![
                OciMount {
                    host_path: dirs.path("x11"),
                    guest_path: X11.into(),
                    writable,
                },
                OciMount {
                    host_path: dirs.path("auth"),
                    guest_path: AUTH.into(),
                    writable: false,
                },
            ]
        };
        app.mounts.extend(mounts(false));
        app.environment.insert("DISPLAY".into(), DISPLAY.into());
        app.environment
            .insert("XAUTHORITY".into(), format!("{AUTH}/Xauthority"));
        let mut helper_mounts = mounts(true);
        helper_mounts.push(OciMount {
            host_path: dirs.path("rfb"),
            guest_path: RFB.into(),
            writable: true,
        });
        let helper_spec = OciSpec {
            id: format!("{}-display", app.id),
            image: helper.image.clone(),
            platform: helper.platform.clone(),
            entrypoint: Some(HELPER.into()),
            user: Some(user),
            argv: helper.launch_argv(viewport.width, viewport.height),
            working_dir: "/run".into(),
            workspace_mount_path: "/run/ato-unused-workspace".into(),
            environment: BTreeMap::new(),
            endpoints: vec![],
            mounts: helper_mounts,
            limits: helper.limits.clone(),
            stop_timeout_seconds: app.stop_timeout_seconds,
            labels: owner.labels(Some("native-display"))?,
        };
        let deadline = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64
            + config
                .startup_timeout
                .min(config.maximum_lifetime)
                .min(Duration::from_secs(5))
                .as_millis() as u64;
        let mut helper_adapter = DockerOciAdapter::new_offline_shared(
            helper_spec,
            Arc::clone(&helper.archive),
            helper.config_reference.clone(),
        )?
        .with_deadline(deadline);
        let mut app_adapter = match application_archive {
            Some(verified) => {
                DockerOciAdapter::new_offline(app, verified.bytes, verified.config_reference)?
            }
            None => DockerOciAdapter::new(app)?,
        }
        .with_deadline(deadline);
        if let Some(owner) = &helper.owner {
            helper_adapter = helper_adapter
                .with_cgroup_parent(owner.docker_parent.clone())?
                .with_isolated_owner(&owner.socket, &runtime.join("helper-docker-client"))?;
            app_adapter = app_adapter
                .with_cgroup_parent(owner.docker_parent.clone())?
                .with_isolated_owner(&owner.socket, &runtime.join("app-docker-client"))?;
        }
        let socket_directory = std::fs::File::open(dirs.path("rfb"))?;
        Ok((
            declaration,
            gateway,
            keys,
            execution,
            authorizer,
            helper_adapter,
            app_adapter,
            dirs,
            runtime,
            socket_directory,
        ))
    })()?;
    let network = match helper_adapter.create_internal_network(&owner.run_id) {
        Ok(network) => network,
        Err(error) => {
            return Err(super::super::lease::StartFailure {
                stop: super::super::lease::settle_failed_oci_start(
                    owner,
                    super::super::recovery::stop_budget(&spec.lifecycle),
                    &error,
                ),
                error,
                process: None,
            });
        }
    };
    let mut parts = Parts {
        container: Some(LaunchedOci::Group(OciServiceGroup::new(network))),
        producer: None,
        pixel_relay: None,
        pixel_directories: Some(dirs),
        probe: None,
        surface: None,
        controls: PixelGatewayControls::default(),
        budget: super::super::recovery::stop_budget(&spec.lifecycle),
        _socket_directory: Some(socket_directory),
    };
    let startup = async {
        let empty_workspace = parts.pixel_directories.as_ref().unwrap().path("empty");
        let LaunchedOci::Group(group) = parts.container.as_mut().unwrap() else {
            unreachable!()
        };
        let handle = helper_adapter.spawn_in_network(
            &empty_workspace,
            &runtime.join("display-helper"),
            group.network(),
            Some("native-display"),
        )?;
        group.push("native-display".into(), handle);
        if let Some(owner) = &helper.owner {
            owner.verify_container(group.services().next().unwrap().1)?;
        }
        let dirs = parts.pixel_directories.as_ref().unwrap();
        // X server socket + RFB transport are readiness, never app acceptance.
        loop {
            ensure!(
                !parts.workload_exited()?,
                "display helper exited during startup"
            );
            if dirs.path("x11/X64").exists() && dirs.path("rfb/display.sock").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        use std::os::fd::AsRawFd;
        let socket = PathBuf::from(format!(
            "/proc/self/fd/{}/display.sock",
            parts._socket_directory.as_ref().unwrap().as_raw_fd()
        ));
        let relay = RfbRelay::start(socket).await?;
        gateway.private_rfb_addr = relay.addr;
        parts.pixel_relay = Some(relay);
        let LaunchedOci::Group(group) = parts.container.as_mut().unwrap() else {
            unreachable!()
        };
        let handle = app_adapter.spawn_in_network(
            context.workspace_root(),
            &runtime.join("application"),
            group.network(),
            Some("main"),
        )?;
        group.push("main".into(), handle);
        if let Some(owner) = &helper.owner {
            owner.verify_container(group.services().last().unwrap().1)?;
        }
        // X applications may map their first window asynchronously. Do not
        // replay authored actions: wait for an explicit X client property via
        // the helper's private readiness marker before the one capture.
        loop {
            ensure!(
                !parts.workload_exited()?,
                "native GUI exited during startup"
            );
            if dirs.path("rfb/application-ready").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        parts.probe = Some(
            capture_and_start_native_probe(
                "127.0.0.1:0".parse().unwrap(),
                NativeProbeTarget::Pixel(gateway.private_rfb_addr),
                &declaration.adapter,
            )
            .await?,
        );
        let assertion =
            keys.issue_readiness_assertion_for_execution(&config.scope, Some(execution))?;
        let connect_url = format!("http://{}/__ato/surface", config.surface_listen);
        let gateway = NativeSurfaceConfig::Pixel(gateway);
        let probe = NativeSurfaceProbe {
            connect_url: &connect_url,
            origin: &config.origin,
            assertion: assertion.as_str(),
        };
        parts.surface = Some(
            if config.controls.text_paste || config.controls.framebuffer_resize {
                netd::native_surface::start_ready_interactive_native_surface(
                    gateway,
                    authorizer,
                    probe,
                    config.startup_timeout,
                    config.idle_timeout,
                    config.controls,
                )
                .await?
            } else {
                start_ready_native_surface(
                    gateway,
                    authorizer,
                    probe,
                    config.startup_timeout,
                    config.idle_timeout,
                )
                .await?
            },
        );
        // This value exists only after the authenticated guarded gateway proved
        // transport readiness. It is not a Contract/receipt or D modification.
        parts.controls = config.controls;
        ensure!(
            !parts.workload_exited()?,
            "native GUI exited during startup"
        );
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! {biased;
        _=owner_alive.changed()=>Err(anyhow::anyhow!("native owner disconnected during startup")),
        _=tokio::time::sleep_until(tokio::time::Instant::from_std(started+config.startup_timeout.min(config.maximum_lifetime)))=>Err(anyhow::anyhow!("native startup timeout")),
        result=startup=>result,
    };
    if let Err(error) = result {
        if error
            .downcast_ref::<ato_adapter_oci::SpawnCleanupUnconfirmed>()
            .is_some()
            && let Some(LaunchedOci::Group(group)) = parts.container.as_mut()
        {
            group.retain_networks_for_recovery();
        }
        let stop = parts.shutdown().await;
        // A failed spawn can have created an unreturned container. The same
        // existing label-scoped recovery sweep is the authority for that case.
        let swept = super::super::lease::settle_failed_oci_start(
            owner,
            super::super::recovery::stop_budget(&spec.lifecycle),
            &error,
        );
        return Err(super::super::lease::StartFailure {
            error,
            stop: StopOutcome::worst([&stop, &swept]).unwrap(),
            process: None,
        });
    }
    Ok(finish_owned_run(
        parts,
        owner_alive,
        started + config.maximum_lifetime,
    ))
}

#[cfg(test)]
pub(crate) mod tests;

/// Recovery preserves settled workspaces. Strip this runtime-only credential
/// before that move, including after a Runner SIGKILL when Drop never ran.
/// The lease was already attributed to this slot and all writers stopped.
pub(crate) fn clear_recovered_credentials(lease_root: &Path) -> Result<()> {
    match std::fs::symlink_metadata(lease_root) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "native recovery lease is not an owned directory"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect native recovery lease"),
    }
    let mut path = lease_root.to_path_buf();
    for component in ["oci-runtime", "display", "auth"] {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "native recovery credential parent is not an owned directory"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).context("inspect native recovery credential directory");
            }
        }
    }
    match std::fs::remove_file(path.join("Xauthority")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("clear recovered native credential"),
    }
}
