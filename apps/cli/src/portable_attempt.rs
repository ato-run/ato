//! Running a validated `.capsule` through the Runtime's common attempt.
//!
//! The bundle is already built: its K, D and tree are canonical objects the
//! portable validator checked. `PortableBundleExecutor` only makes that tree
//! runnable and starts it — it fetches the declared external objects a thin
//! bundle names, unpacks the tree, and launches the Derivation's serving
//! step (or serves the static tree). It builds nothing, installs nothing,
//! provisions no toolchain, and never produces a ProgramIntent or a build
//! plan. Admission, the start record, the HTTP observation, K's verdicts
//! and the receipt are `ato_runtime_attempt::attempt::run_attempt`'s.
//!
//! Every declared realization runs here: StaticWeb and LocalProcess (stage
//! 2c), and an OCI container or OCI service group (stage 2e-c) through the
//! Runtime's common OCI launch and its [`OciCandidate`], whose stop is
//! confirmed per container before the runtime scratch is removed.

use std::any::Any;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ato_adapter_oci::{
    DockerOciAdapter, OciEndpoint, OciMount, OciNetwork, OciResourceLimits, OciSpec, StopBudget,
};
use ato_adapter_process::{ProcessAdapter, ProcessHandle, ProcessSpec};
use ato_formation::request::{AttemptFailure, RuntimeProfile};
use ato_formation::verify::VerificationExecutionEvidence;
use ato_objects::PortableDependencyProfile;
use ato_portable_application::{
    OCI_CPU_MILLIS_RUNTIME, OCI_ENTRYPOINT_RUNTIME, OCI_IMAGE_RUNTIME, OCI_MEMORY_BYTES_RUNTIME,
    OCI_PIDS_LIMIT_RUNTIME, OCI_PLATFORM_RUNTIME, OCI_WORKSPACE_MOUNT_RUNTIME, PYTHON_RUNTIME,
    PortableRealizationKind, StaticApplicationServer, StaticApplicationServerExt,
    StaticApplicationState, ValidatedPortableApplication,
};
use ato_runtime_attempt::launch::oci::{
    LaunchedOci, OciCandidate, ServiceStart, start_service_group,
};
use ato_runtime_attempt::realize::{CandidateRealizer, RealizeFailure, Realized, RunningCandidate};

use super::{
    endpoint_port_env_name, parse_runtime_limit, portable_process_sandbox_command,
    resolve_pinned_python, resolve_portable_state_mounts, state_path_env_name,
};

/// Makes a validated portable route runnable and starts it.
pub(crate) struct PortableBundleExecutor<'a> {
    /// The bundle as it arrived; a thin bundle's external objects are
    /// fetched and verified when the candidate is realized.
    pub bundle: &'a ato_objects::PortableApplicationBundle,
    pub validated: &'a ValidatedPortableApplication,
    /// Where this Run's runtime scratch lives: the unpacked tree, process
    /// runtime directories, ephemeral state.
    pub runtime_root: &'a Path,
    pub static_state: Option<StaticApplicationState>,
    pub filesystem_state: Option<&'a BTreeMap<String, PathBuf>>,
    pub binding_environment: &'a BTreeMap<String, String>,
}

fn refused(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: "admission".to_owned(),
        message: message.into(),
    })
}

impl CandidateRealizer for PortableBundleExecutor<'_> {
    fn admit(&self, _profile: &RuntimeProfile) -> Option<AttemptFailure> {
        match self.validated.realization {
            PortableRealizationKind::StaticWeb => None,
            PortableRealizationKind::LocalProcess => {
                if !self.validated.derivation.state.is_empty() && !cfg!(target_os = "linux") {
                    return refused(
                        "state_needs_linux_sandbox",
                        "local process runtime admission failed: declared filesystem state \
                         requires a Linux bubblewrap sandbox",
                    );
                }
                None
            }
            // The adapter forwards a container's Port from its internal
            // bridge, which Docker Desktop keeps inside its VM.
            PortableRealizationKind::OciContainer | PortableRealizationKind::OciServiceGroup
                if !cfg!(target_os = "linux") =>
            {
                refused(
                    "oci_needs_native_linux",
                    format!(
                        "{} routes require a native Linux Docker host",
                        self.validated.realization.label()
                    ),
                )
            }
            PortableRealizationKind::OciContainer | PortableRealizationKind::OciServiceGroup => {
                None
            }
        }
    }

    fn realize(&self, _attempt_id: &str, _attempt_root: &Path) -> Result<Realized, RealizeFailure> {
        let route = self.validated;
        fs::create_dir_all(self.runtime_root)
            .with_context(|| format!("create {}", self.runtime_root.display()))?;
        let (hydrated, dependency_fetches) =
            crate::portable_dependency::hydrate_external_objects(self.bundle)?;
        for reference in &dependency_fetches {
            eprintln!("dependency fetched and verified: {reference}");
        }
        let state_mounts =
            resolve_portable_state_mounts(route, self.runtime_root, self.filesystem_state)?;
        let workspace = self.runtime_root.join("workspace");
        ato_portable_application::materialize_tree(&hydrated, route, &workspace)
            .map_err(anyhow::Error::from)?;
        let mut realized = match route.realization {
            PortableRealizationKind::StaticWeb => self.start_static(workspace)?,
            PortableRealizationKind::LocalProcess => {
                self.start_process(workspace, &state_mounts)?
            }
            PortableRealizationKind::OciContainer => {
                self.start_container(&hydrated, workspace, &state_mounts)?
            }
            PortableRealizationKind::OciServiceGroup => {
                self.start_service_group(&hydrated, workspace, &state_mounts)?
            }
        };
        realized.execution.dependency_fetches = dependency_fetches;
        Ok(realized)
    }
}

impl PortableBundleExecutor<'_> {
    fn start_static(&self, workspace: PathBuf) -> Result<Realized, RealizeFailure> {
        let server = StaticApplicationServer::start_with_state(
            &workspace,
            self.validated,
            self.static_state.clone(),
        )
        .map_err(|error| launch(error.into(), &workspace))?;
        let base = server.base_url();
        let endpoints = self
            .validated
            .derivation
            .ports
            .iter()
            .map(|port| (port.id.clone(), base.clone()))
            .collect();
        Ok(Realized {
            candidate: Box::new(PortableStaticCandidate {
                server: Some(server),
                endpoints,
                workspace,
            }),
            evidence: None,
            execution: evidence("static_web", None, None, None, &base),
            kept: None,
        })
    }

    /// The route's one serving step, as its D declares it: argv, cwd, env,
    /// the pinned runtime, the port and the state mounts.
    fn start_process(
        &self,
        workspace: PathBuf,
        state_mounts: &[super::PortableStateMount],
    ) -> Result<Realized, RealizeFailure> {
        let route = self.validated;
        let step = &route.derivation.steps[0];
        let guest_port = route.derivation.ports[0]
            .guest_port
            .context("process derivation omitted guest_port")?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").context("allocate a port")?;
        let host_port = listener.local_addr().context("allocate a port")?.port();
        drop(listener);
        let guest_port = guest_port.to_string();
        let host_port = host_port.to_string();
        let mut command = step.argv.clone();
        let (executable, version) =
            if let Some(python_version) = route.derivation.runtimes.get(PYTHON_RUNTIME) {
                if !matches!(command[0].as_str(), "python" | "python3") {
                    return Err(RealizeFailure::Execution(anyhow::anyhow!(
                        "Python process derivation must use a logical python executable in argv[0]"
                    )));
                }
                resolve_pinned_python(python_version)?
            } else {
                (command[0].clone(), "unconstrained".to_owned())
            };
        command[0] = executable.clone();
        let mut replaced = 0;
        for argument in &mut command {
            if argument == &guest_port {
                *argument = host_port.clone();
                replaced += 1;
            }
        }
        if replaced > 1 {
            return Err(RealizeFailure::Execution(anyhow::anyhow!(
                "process derivation names its declared guest port more than once in argv"
            )));
        }
        let endpoint_name = endpoint_port_env_name(&route.derivation.ports[0].id);
        let mut environment = step.env.clone();
        environment.extend(self.binding_environment.clone());
        environment.insert(endpoint_name, host_port.clone());
        for state in state_mounts {
            environment.insert(state_path_env_name(&state.id), state.guest_path.clone());
        }
        let process_runtime = self.runtime_root.join("process");
        fs::create_dir_all(&process_runtime).with_context(|| {
            format!(
                "create portable process runtime {}",
                process_runtime.display()
            )
        })?;
        let process_tmp = process_runtime.join("tmp");
        let process_home = process_runtime.join("home");
        fs::create_dir_all(&process_tmp).context("create the process tmp directory")?;
        fs::create_dir_all(&process_home).context("create the process home directory")?;
        if state_mounts.is_empty() {
            environment.insert(
                "ATO_RUNTIME_DIR".to_owned(),
                process_runtime.display().to_string(),
            );
            for name in ["TMPDIR", "TMP", "TEMP"] {
                environment.insert(name.to_owned(), process_tmp.display().to_string());
            }
            environment.insert("HOME".to_owned(), process_home.display().to_string());
            environment.insert(
                "XDG_CACHE_HOME".to_owned(),
                process_home.join(".cache").display().to_string(),
            );
        } else {
            for name in ["ATO_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP", "HOME"] {
                environment.insert(name.to_owned(), "/tmp".to_owned());
            }
            environment.insert("XDG_CACHE_HOME".to_owned(), "/tmp/.cache".to_owned());
        }
        let command = portable_process_sandbox_command(
            &workspace,
            &process_runtime,
            &executable,
            &command,
            host_port.parse().context("host port")?,
            &step.cwd,
            state_mounts,
        )?;
        let adapter = ProcessAdapter::new(ProcessSpec {
            id: step.id.clone(),
            command,
            cwd: PathBuf::from(&step.cwd),
            environment,
            isolated_group: true,
        })
        .map_err(|error| launch(error.into(), &workspace))?;
        let handle = adapter
            .spawn(&workspace)
            .map_err(|error| launch(error.into(), &workspace))?;
        let base = format!("http://127.0.0.1:{host_port}");
        let pid = handle.pid();
        Ok(Realized {
            candidate: Box::new(PortableProcessCandidate {
                handle: Some(handle),
                endpoints: BTreeMap::from([(route.derivation.ports[0].id.clone(), base.clone())]),
                owned: vec![workspace, process_runtime],
            }),
            evidence: None,
            execution: evidence("process", Some(executable), Some(version), Some(pid), &base),
            kept: None,
        })
    }
}

impl PortableBundleExecutor<'_> {
    /// The route's one OCI container, as its D declares it: pinned image and
    /// platform, argv, env, limits, the Surface Port and the state mounts.
    fn start_container(
        &self,
        hydrated: &ato_objects::PortableApplicationBundle,
        workspace: PathBuf,
        state_mounts: &[super::PortableStateMount],
    ) -> Result<Realized, RealizeFailure> {
        let route = self.validated;
        let step = &route.derivation.steps[0];
        let port = &route.derivation.ports[0];
        let guest_port = port.guest_port.context("OCI route omitted guest_port")?;
        let host_port = free_loopback_port()?;
        let runtime = &route.derivation.runtimes;
        let mut environment = step.env.clone();
        environment.extend(self.binding_environment.clone());
        let mounts = state_mounts
            .iter()
            .map(|state| {
                environment.insert(state_path_env_name(&state.id), state.guest_path.clone());
                OciMount {
                    host_path: state.host_path.clone(),
                    guest_path: state.guest_path.clone(),
                    writable: true,
                }
            })
            .collect();
        let spec = OciSpec {
            id: route.derivation_ref.to_string(),
            image: runtime
                .get(OCI_IMAGE_RUNTIME)
                .context("OCI route omitted image")?
                .clone(),
            platform: runtime
                .get(OCI_PLATFORM_RUNTIME)
                .context("OCI route omitted platform")?
                .clone(),
            entrypoint: runtime.get(OCI_ENTRYPOINT_RUNTIME).cloned(),
            argv: step.argv.clone(),
            working_dir: "/app".to_owned(),
            workspace_mount_path: runtime
                .get(OCI_WORKSPACE_MOUNT_RUNTIME)
                .cloned()
                .unwrap_or_else(|| "/app".to_owned()),
            environment,
            endpoints: vec![OciEndpoint {
                host_port,
                guest_port,
            }],
            mounts,
            limits: oci_limits(runtime)?,
            stop_timeout_seconds: 5,
            // A local Run is not Runner-owned; recovery never scans it.
            labels: BTreeMap::new(),
        };
        let oci_runtime = self.runtime_root.join("oci");
        let handle = local_oci_adapter(hydrated, spec)
            .and_then(|adapter| adapter.spawn(&workspace, &oci_runtime))
            .map_err(|error| oci_launch_failure(error, &workspace, &oci_runtime))?;
        let base = format!("http://127.0.0.1:{host_port}");
        let mut execution = evidence("oci", Some("docker".to_owned()), None, None, &base);
        execution.container_id = Some(handle.container_id().to_owned());
        execution.image = Some(handle.image().to_owned());
        execution.platform = Some(handle.platform().to_owned());
        if self.offline() {
            execution.embedded_oci_image_loaded = execution.image.clone();
        }
        Ok(Realized {
            candidate: Box::new(OciCandidate::new(
                LaunchedOci::Container(handle),
                BTreeMap::from([(port.id.clone(), base)]),
                vec![workspace, oci_runtime],
                StopBudget::DEFAULT,
            )),
            evidence: None,
            execution,
            kept: None,
        })
    }

    /// The route's OCI service group: one `--internal` network, each service
    /// under its own DNS alias, started in authored order after the previous
    /// one accepts TCP on its Port. Only the Surface Port is forwarded to
    /// loopback. Each service receives only its own Bindings and state.
    fn start_service_group(
        &self,
        hydrated: &ato_objects::PortableApplicationBundle,
        workspace: PathBuf,
        state_mounts: &[super::PortableStateMount],
    ) -> Result<Realized, RealizeFailure> {
        let route = self.validated;
        let derivation = &route.derivation;
        let platform = derivation
            .runtimes
            .get(OCI_PLATFORM_RUNTIME)
            .context("OCI service group omitted platform")?;
        let surface_port = derivation
            .ports
            .iter()
            .find(|port| port.protocol == ato_formation::authoring::HTTP_PROTOCOL)
            .context("OCI service group omitted its Surface Port")?;
        let host_port = free_loopback_port()?;
        let oci_runtime = self.runtime_root.join("oci");
        let mut services = Vec::with_capacity(derivation.steps.len());
        for step in &derivation.steps {
            let runtime = &step.runtimes;
            let mut environment = step.env.clone();
            for binding in &step.bindings {
                let name = ato_portable_application::binding_environment_name(binding);
                if let Some(value) = self.binding_environment.get(&name) {
                    environment.insert(name, value.clone());
                }
            }
            let mounts = state_mounts
                .iter()
                .filter(|state| step.state.contains(&state.id))
                .map(|state| {
                    environment.insert(state_path_env_name(&state.id), state.guest_path.clone());
                    OciMount {
                        host_path: state.host_path.clone(),
                        guest_path: state.guest_path.clone(),
                        writable: true,
                    }
                })
                .collect();
            let spec = OciSpec {
                id: format!("{}-{}", route.derivation_ref, step.id),
                image: runtime
                    .get(OCI_IMAGE_RUNTIME)
                    .context("OCI service omitted image")?
                    .clone(),
                platform: platform.clone(),
                entrypoint: runtime.get(OCI_ENTRYPOINT_RUNTIME).cloned(),
                argv: step.argv.clone(),
                working_dir: "/app".to_owned(),
                workspace_mount_path: runtime
                    .get(OCI_WORKSPACE_MOUNT_RUNTIME)
                    .cloned()
                    .unwrap_or_else(|| "/app".to_owned()),
                environment,
                endpoints: if surface_port.from == step.id {
                    vec![OciEndpoint {
                        host_port,
                        guest_port: surface_port
                            .guest_port
                            .context("Surface Port omitted guest_port")?,
                    }]
                } else {
                    Vec::new()
                },
                mounts,
                limits: oci_limits(runtime)?,
                stop_timeout_seconds: 5,
                // A local Run is not Runner-owned; recovery never scans it.
                labels: BTreeMap::new(),
            };
            services.push(ServiceStart {
                name: step.id.clone(),
                adapter: local_oci_adapter(hydrated, spec)?,
                runtime_root: oci_runtime.join(&step.id),
                networks: Vec::new(),
                guards: Vec::new(),
            });
        }
        let group = OciNetwork::create(&route.derivation_ref.to_string(), &BTreeMap::new())
            .and_then(|network| {
                start_service_group(
                    network,
                    &workspace,
                    services,
                    StopBudget::DEFAULT,
                    &mut |name, group| wait_until_service_accepts_tcp(route, name, group),
                )
            })
            .map_err(|error| oci_launch_failure(error, &workspace, &oci_runtime))?;
        let base = format!("http://127.0.0.1:{host_port}");
        let surface = group
            .services()
            .find(|(name, _)| *name == surface_port.from)
            .map(|(_, handle)| handle);
        let mut execution = evidence(
            "oci_service_group",
            Some("docker".to_owned()),
            None,
            None,
            &base,
        );
        execution.container_id = surface.map(|handle| handle.container_id().to_owned());
        execution.image = surface.map(|handle| handle.image().to_owned());
        execution.platform = surface.map(|handle| handle.platform().to_owned());
        execution.services = group
            .services()
            .map(
                |(name, handle)| ato_formation::verify::VerificationServiceEvidence {
                    name: name.to_owned(),
                    container_id: handle.container_id().to_owned(),
                    image: handle.image().to_owned(),
                },
            )
            .collect();
        Ok(Realized {
            candidate: Box::new(OciCandidate::new(
                LaunchedOci::Group(group),
                BTreeMap::from([(surface_port.id.clone(), base)]),
                vec![workspace, oci_runtime],
                StopBudget::DEFAULT,
            )),
            evidence: None,
            execution,
            kept: None,
        })
    }

    fn offline(&self) -> bool {
        self.bundle
            .portability
            .as_ref()
            .is_some_and(|portability| portability.profile == PortableDependencyProfile::Offline)
    }
}

/// The service accepts TCP on its first Port within 60s, and no service of
/// the group has exited meanwhile.
fn wait_until_service_accepts_tcp(
    route: &ValidatedPortableApplication,
    name: &str,
    group: &ato_adapter_oci::OciServiceGroup,
) -> Result<()> {
    let address = group
        .services()
        .find(|(service, _)| *service == name)
        .map(|(_, handle)| handle.container_address())
        .context("service was not started")?;
    let guest_port = route
        .derivation
        .ports
        .iter()
        .find(|port| port.from == name)
        .and_then(|port| port.guest_port)
        .context("OCI service serves no Port")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some((service, code)) = group.exited_service()? {
            bail!("OCI service `{service}` exited before the group was ready: {code}");
        }
        let target = std::net::SocketAddr::new(address, guest_port);
        if std::net::TcpStream::connect_timeout(&target, std::time::Duration::from_millis(500))
            .is_ok()
        {
            return Ok(());
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "OCI service `{name}` did not accept TCP on {guest_port} within 60s"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn oci_limits(runtime: &BTreeMap<String, String>) -> Result<OciResourceLimits> {
    Ok(OciResourceLimits {
        memory_bytes: parse_runtime_limit(runtime, OCI_MEMORY_BYTES_RUNTIME)?,
        cpu_limit_millis: parse_runtime_limit(runtime, OCI_CPU_MILLIS_RUNTIME)?,
        pids_limit: parse_runtime_limit(runtime, OCI_PIDS_LIMIT_RUNTIME)?,
    })
}

fn free_loopback_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("allocate a port")?;
    Ok(listener.local_addr().context("allocate a port")?.port())
}

/// A Docker adapter for one image, loading it from the bundle's verified
/// archive when the bundle is an offline export.
fn local_oci_adapter(
    bundle: &ato_objects::PortableApplicationBundle,
    spec: OciSpec,
) -> Result<DockerOciAdapter> {
    if !bundle
        .portability
        .as_ref()
        .is_some_and(|portability| portability.profile == PortableDependencyProfile::Offline)
    {
        return DockerOciAdapter::new(spec);
    }
    let archive = bundle
        .portability
        .as_ref()
        .and_then(|portability| {
            portability
                .oci_archives
                .iter()
                .find(|archive| archive.image == spec.image && archive.platform == spec.platform)
        })
        .context("offline OCI image archive is missing")?;
    let verified = ato_portable_application::oci_archive::verify_oci_archive(archive)?;
    DockerOciAdapter::new_offline(spec, verified.bytes, verified.config_reference)
}

/// Preserve partial execution separately from a failure that started nothing.
fn oci_launch_failure(error: anyhow::Error, workspace: &Path, runtime: &Path) -> RealizeFailure {
    let mut resources =
        if let Some(left) = error.downcast_ref::<ato_adapter_oci::SpawnCleanupUnconfirmed>() {
            let mut ids = vec![format!("container:{}", left.container)];
            ids.extend(left.networks.iter().map(|name| format!("network:{name}")));
            Some(ids)
        } else {
            error
                .downcast_ref::<ato_runtime_attempt::launch::oci::OciStartFailure>()
                .filter(|failure| !failure.confirmed())
                .map(|failure| failure.resources())
        };
    if let Some(resources) = resources.as_mut() {
        resources.push(format!("scratch:{}", workspace.display()));
        resources.push(format!("scratch:{}", runtime.display()));
        return RealizeFailure::Abandoned {
            cleanup: format!("{error:#}"),
            error,
            resources: resources.clone(),
        };
    }
    let _ = fs::remove_dir_all(runtime);
    launch(error, workspace)
}

/// A candidate that could not be started: whatever was unpacked for it goes.
fn launch(error: anyhow::Error, workspace: &Path) -> RealizeFailure {
    let _ = fs::remove_dir_all(workspace);
    RealizeFailure::Launch {
        error,
        evidence: None,
    }
}

fn evidence(
    realization: &str,
    executable: Option<String>,
    version: Option<String>,
    pid: Option<u32>,
    base: &str,
) -> VerificationExecutionEvidence {
    VerificationExecutionEvidence {
        realization: realization.to_owned(),
        runtime_executable: executable,
        runtime_version: version,
        pid,
        container_id: None,
        image: None,
        platform: None,
        endpoint: Some(base.to_owned()),
        run_id: None,
        lease_id: None,
        attempt_id: None,
        request_id: None,
        dependency_fetches: Vec::new(),
        portability_profile: None,
        embedded_oci_image_loaded: None,
        services: Vec::new(),
    }
}

/// Remove what a Run's realization unpacked or created for itself.
fn remove_owned(paths: &[PathBuf]) -> Result<()> {
    let mut failed = Vec::new();
    for path in paths {
        if path.exists()
            && let Err(error) = fs::remove_dir_all(path)
        {
            failed.push(format!("{}: {error}", path.display()));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        bail!("runtime scratch was not removed: {}", failed.join("; "))
    }
}

/// A Run's process: its process group, and the unpacked tree and process
/// runtime directories it owns.
pub(crate) struct PortableProcessCandidate {
    handle: Option<ProcessHandle>,
    endpoints: BTreeMap<String, String>,
    owned: Vec<PathBuf>,
}

impl RunningCandidate for PortableProcessCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }

    fn exited(&mut self) -> Result<Option<String>> {
        match self.handle.as_mut() {
            Some(handle) => Ok(handle
                .try_wait()?
                .map(|status| format!("selected process derivation exited: {status}"))),
            None => Ok(Some("stopped".to_owned())),
        }
    }

    fn stop(mut self: Box<Self>) -> Result<()> {
        if let Some(mut handle) = self.handle.take()
            && handle.try_wait()?.is_none()
        {
            handle.terminate()?;
            for _ in 0..200 {
                if handle.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            // Still running: dropping the handle kills the group and reaps.
        }
        remove_owned(&self.owned)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for PortableProcessCandidate {
    fn drop(&mut self) {
        // The handle's own Drop terminates and reaps the group first.
        drop(self.handle.take());
        if let Err(error) = remove_owned(&self.owned) {
            eprintln!("[ato run] {error:#}");
        }
    }
}

/// A Run's static server, and the unpacked tree it serves from.
pub(crate) struct PortableStaticCandidate {
    server: Option<StaticApplicationServer>,
    endpoints: BTreeMap<String, String>,
    workspace: PathBuf,
}

impl PortableStaticCandidate {
    /// The page state the application kept, before it is stopped.
    pub(crate) fn local_storage(&self) -> Result<Option<BTreeMap<String, String>>> {
        match &self.server {
            Some(server) => Ok(server.local_storage()?),
            None => Ok(None),
        }
    }
}

impl RunningCandidate for PortableStaticCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }

    fn exited(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn stop(mut self: Box<Self>) -> Result<()> {
        drop(self.server.take());
        remove_owned(std::slice::from_ref(&self.workspace))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for PortableStaticCandidate {
    fn drop(&mut self) {
        // Stop serving first, then remove the tree it served from.
        drop(self.server.take());
        if let Err(error) = remove_owned(std::slice::from_ref(&self.workspace)) {
            eprintln!("[ato run] {error:#}");
        }
    }
}
