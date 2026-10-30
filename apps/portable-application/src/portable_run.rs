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
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{
    OCI_CPU_MILLIS_RUNTIME, OCI_ENTRYPOINT_RUNTIME, OCI_IMAGE_RUNTIME, OCI_MEMORY_BYTES_RUNTIME,
    OCI_PIDS_LIMIT_RUNTIME, OCI_PLATFORM_RUNTIME, OCI_USER_RUNTIME, OCI_WORKING_DIR_RUNTIME,
    OCI_WORKSPACE_MOUNT_RUNTIME, PYTHON_RUNTIME, PortableRealizationKind, StaticApplicationServer,
    StaticApplicationServerExt, StaticApplicationState, ValidatedPortableApplication,
};
use anyhow::{Context, Result, bail, ensure};
use ato_adapter_oci::{
    DockerOciAdapter, OciEndpoint, OciMount, OciNetwork, OciResourceLimits, OciSpec, StopBudget,
};
use ato_adapter_process::{ProcessAdapter, ProcessHandle, ProcessSpec};
use ato_formation::request::{AttemptFailure, AttemptOutcomes, RuntimeProfile};
use ato_formation::verify::{
    ContractVerification, ContractVerificationReceipt, VerificationExecutionEvidence,
    VerificationTargetKind,
};
use ato_objects::PortableDependencyProfile;
use ato_runtime_attempt::admission::EffectAuthorization;
use ato_runtime_attempt::build_sandbox::NetworkPolicy;
use ato_runtime_attempt::data_plane::{ModelCache, ModelSetEntry, ModelSetManifest, env_name};
use ato_runtime_attempt::launch::oci::{
    LaunchedOci, OciCandidate, ServiceStart, start_service_group,
};
use ato_runtime_attempt::realize::{CandidateRealizer, RealizeFailure, Realized, RunningCandidate};
use ato_sandbox::SandboxPolicy;

/// Host-side inputs a portable Run needs from whoever starts it.
///
/// The CLI answers with `~/.ato/cache`, its own binary as the sandbox shim and
/// the `__portable-sandbox-exec` subcommand. The Formation worker answers with
/// its work root and its own shim subcommand. Neither value belongs to the
/// bundle, K, D or the receipt.
pub struct PortableRunContext<'a> {
    /// Where cached external objects (Model Sets, wheels) live.
    pub cache_root: &'a Path,
    /// The executable that carries the portable sandbox shim subcommand.
    pub sandbox_shim: &'a Path,
    /// The subcommand that applies a portable sandbox policy.
    pub sandbox_subcommand: &'a str,
}

/// Makes a validated portable route runnable and starts it.
pub struct PortableBundleExecutor<'a> {
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
    /// Everything the host, not the bundle, decides about this start.
    pub context: &'a PortableRunContext<'a>,
}

fn refused(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: "admission".to_owned(),
        message: message.into(),
    })
}

impl CandidateRealizer for PortableBundleExecutor<'_> {
    fn delivers_model_sets(&self) -> bool {
        self.validated.realization == PortableRealizationKind::LocalProcess
    }

    fn admits_host_requirements(&self) -> bool {
        ato_runtime_attempt::launch::host_boundary::active().is_some()
            && self
                .validated
                .derivation
                .requirements
                .host
                .as_ref()
                .is_some_and(|host| !host.accelerators.is_empty())
    }

    fn admit(&self, _profile: &RuntimeProfile) -> Option<AttemptFailure> {
        if let Some(host) = self.validated.derivation.requirements.host.as_ref()
            && !host.accelerators.is_empty()
        {
            let resources =
                ato_runtime_attempt::host_resources::HostResources::probe(self.runtime_root);
            if let Err(reason) =
                ato_runtime_attempt::host_resources::admit_gpu_host(host, &resources)
            {
                return refused("gpu_host_unavailable", reason);
            }
        }
        if ato_runtime_attempt::launch::host_boundary::active().is_some()
            && (self.validated.realization != PortableRealizationKind::LocalProcess
                || !self.validated.derivation.state.is_empty())
        {
            return refused(
                "formation_gpu_route_unsupported",
                "dedicated GPU Formation supports process routes without namespace-dependent state mounts",
            );
        }
        if !self.validated.model_sets.is_empty() {
            let checked = resolve_model_inputs(
                &self.validated.model_sets,
                self.context.cache_root,
                self.runtime_root,
                false,
            );
            if let Err(error) = checked {
                return refused(
                    "model_set_unavailable",
                    format!("local Model Set admission failed: {error:#}"),
                );
            }
        }
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
        if let Some(abi) = route
            .derivation
            .requirements
            .startup
            .as_ref()
            .and_then(|s| s.abi.as_ref())
        {
            ato_runtime_attempt::launch::process_abi::check(&abi.glibc_min, &abi.glibcxx_min)?;
        }
        fs::create_dir_all(self.runtime_root)
            .with_context(|| format!("create {}", self.runtime_root.display()))?;
        let (hydrated, dependency_fetches) =
            crate::dependency_transport::hydrate_external_objects(self.bundle)?;
        for reference in &dependency_fetches {
            eprintln!("dependency fetched and verified: {reference}");
        }
        let state_mounts =
            resolve_portable_state_mounts(route, self.runtime_root, self.filesystem_state)?;
        let input_mounts = if route.model_sets.is_empty() {
            Vec::new()
        } else {
            resolve_model_inputs(
                &route.model_sets,
                self.context.cache_root,
                self.runtime_root,
                true,
            )?
        };
        let workspace = self.runtime_root.join("workspace");
        crate::materialize_tree(&hydrated, route, &workspace).map_err(anyhow::Error::from)?;
        let mut realized = match route.realization {
            PortableRealizationKind::StaticWeb => self.start_static(workspace)?,
            PortableRealizationKind::LocalProcess => {
                self.start_process(workspace, &state_mounts, &input_mounts)?
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

/// Resolves only the declared cached objects. No upstream fetch or alternate
/// Model Set is considered; absence is an admission refusal before execution.
fn resolve_model_inputs(
    sets: &[crate::ValidatedModelSet],
    cache_root: &Path,
    runtime_root: &Path,
    materialize: bool,
) -> Result<Vec<PortableInputMount>> {
    let mut names = BTreeSet::from([env_name("workspace"), env_name("assets")]);
    for set in sets {
        ensure!(
            names.insert(env_name(&set.input_id)),
            "Model Set input environment names collide or use a reserved name"
        );
    }
    let cache = ModelCache::open_existing(cache_root)
        .context("import declared Model Sets with ato model-set import first")?;
    let manifests: Vec<ModelSetManifest> = sets
        .iter()
        .map(|set| ModelSetManifest {
            schema: set.manifest.schema.clone(),
            objects: set
                .manifest
                .objects
                .iter()
                .map(|entry| ModelSetEntry {
                    path: entry.path.clone(),
                    digest: entry.digest.clone(),
                    bytes: entry.bytes,
                })
                .collect(),
        })
        .collect();
    // Check the entire closure before constructing any delivery tree.
    for manifest in &manifests {
        for entry in &manifest.objects {
            cache.verify_cached(entry)?;
        }
    }
    let mut inputs = Vec::new();
    for (index, (set, manifest)) in sets.iter().zip(&manifests).enumerate() {
        let host_path = runtime_root.join("inputs").join(index.to_string());
        if materialize {
            cache.materialize(manifest, &host_path)?;
        }
        inputs.push(PortableInputMount {
            env_name: env_name(&set.input_id),
            host_path,
            guest_path: format!("/ato-inputs/{index}"),
        });
    }
    Ok(inputs)
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
        state_mounts: &[PortableStateMount],
        input_mounts: &[PortableInputMount],
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
        for input in input_mounts {
            environment.insert(
                input.env_name.clone(),
                if state_mounts.is_empty() {
                    input
                        .host_path
                        .canonicalize()
                        .context("canonicalize delivered Model Set")?
                        .display()
                        .to_string()
                } else {
                    input.guest_path.clone()
                },
            );
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
        // The sealed bundle's declaration was checked at admission; this is the
        // final check of what this local launch would exec. A `.capsule` can
        // still name an absolute toolchain path in argv or an authored PATH.
        ato_runtime_attempt::launch::executable_gate::validate_derivation_executable(
            &step.argv[0],
            &workspace.join(&step.cwd),
            &environment,
            &route.derivation,
        )
        .context(ato_runtime_attempt::launch::executable_gate::context_for_launch())?;
        let command = portable_process_sandbox_command(
            &workspace,
            &process_runtime,
            &executable,
            &command,
            host_port.parse().context("host port")?,
            &step.cwd,
            PortableProcessMounts {
                state: state_mounts,
                inputs: input_mounts,
            },
            self.context.sandbox_shim,
            self.context.sandbox_subcommand,
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
                owned: vec![workspace, process_runtime]
                    .into_iter()
                    .chain(input_mounts.iter().map(|input| input.host_path.clone()))
                    .collect(),
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
        state_mounts: &[PortableStateMount],
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
            user: runtime
                .get(OCI_USER_RUNTIME)
                .map(|value| value.parse())
                .transpose()?,
            argv: step.argv.clone(),
            working_dir: runtime
                .get(OCI_WORKING_DIR_RUNTIME)
                .cloned()
                .unwrap_or_else(|| "/app".to_owned()),
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
        state_mounts: &[PortableStateMount],
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
                let name = crate::binding_environment_name(binding);
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
                user: runtime
                    .get(OCI_USER_RUNTIME)
                    .map(|value| value.parse())
                    .transpose()?,
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
        let mut observation_endpoints = BTreeMap::from([(surface_port.id.clone(), base.clone())]);
        for observation in
            ato_runtime_attempt::verification::required_http_observations(&route.attempt_spec(None))
        {
            if observation.port_id == surface_port.id {
                continue;
            }
            let port = derivation
                .ports
                .iter()
                .find(|port| port.id == observation.port_id)
                .context("observation names undeclared service Port")?;
            let (_, handle) = group
                .services()
                .find(|(name, _)| *name == port.from)
                .context("observation service was not started")?;
            let address = std::net::SocketAddr::new(
                handle.container_address(),
                port.guest_port
                    .context("observation Port omitted guest port")?,
            );
            observation_endpoints.insert(port.id.clone(), format!("http://{address}"));
        }
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
                observation_endpoints,
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
    let verified = crate::oci_archive::verify_oci_archive(archive)?;
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
pub struct PortableProcessCandidate {
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
pub struct PortableStaticCandidate {
    server: Option<StaticApplicationServer>,
    endpoints: BTreeMap<String, String>,
    workspace: PathBuf,
}

impl PortableStaticCandidate {
    /// The page state the application kept, before it is stopped.
    pub fn local_storage(&self) -> Result<Option<BTreeMap<String, String>>> {
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

#[cfg(all(test, unix))]
mod model_input_tests {
    use super::*;
    use ato_formation::model_set::{
        MODEL_SET_SCHEMA, ModelSetEntry as ManifestEntry, ModelSetManifest as Manifest,
    };
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    fn model_set(id: &str, data: &[u8]) -> crate::ValidatedModelSet {
        let manifest = Manifest {
            schema: MODEL_SET_SCHEMA.to_owned(),
            objects: vec![ManifestEntry {
                path: "weights.bin".to_owned(),
                digest: format!("sha256:{:x}", Sha256::digest(data)),
                bytes: data.len() as u64,
            }],
        };
        crate::ValidatedModelSet {
            input_id: id.to_owned(),
            reference: manifest.reference().unwrap(),
            manifest,
        }
    }

    fn populate(root: &Path, set: &crate::ValidatedModelSet, data: &[u8]) {
        let cache = ModelCache::open(root).unwrap();
        let e = &set.manifest.objects[0];
        cache
            .ensure(
                &ModelSetEntry {
                    path: e.path.clone(),
                    digest: e.digest.clone(),
                    bytes: e.bytes,
                },
                |_, _| Ok(Box::new(Cursor::new(data.to_vec()))),
                || Ok(()),
            )
            .unwrap();
    }

    #[test]
    fn offline_delivery_uses_readonly_links_outside_writable_process_scratch() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join("cache");
        let runtime_root = root.path().join("run");
        let set = model_set("models", b"weights");
        populate(&cache_root, &set, b"weights");
        let mounts = resolve_model_inputs(&[set], &cache_root, &runtime_root, true).unwrap();
        assert_eq!(mounts[0].env_name, "ATO_INPUT_PATH_MODELS");
        assert!(
            !mounts[0]
                .host_path
                .starts_with(runtime_root.join("process"))
        );
        assert_eq!(
            fs::read(mounts[0].host_path.join("weights.bin")).unwrap(),
            b"weights"
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(mounts[0].host_path.join("weights.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
        fs::remove_dir_all(&runtime_root).unwrap();
        assert_eq!(
            ModelCache::open_existing(&cache_root)
                .unwrap()
                .usage_bytes()
                .unwrap(),
            7
        );
    }

    #[test]
    fn missing_closure_and_colliding_input_names_refuse_before_delivery() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join("cache");
        let runtime_root = root.path().join("run");
        let first = model_set("one", b"one");
        populate(&cache_root, &first, b"one");
        let missing = model_set("two", b"absent");
        assert!(resolve_model_inputs(&[first, missing], &cache_root, &runtime_root, true).is_err());
        assert!(!runtime_root.exists());
        let reserved = [model_set("assets", b"one")];
        assert!(resolve_model_inputs(&reserved, &cache_root, &runtime_root, true).is_err());
        let sets = [model_set("a-b", b"one"), model_set("a_b", b"one")];
        let error = resolve_model_inputs(&sets, &cache_root, &runtime_root, true).unwrap_err();
        assert!(error.to_string().contains("environment names collide"));
        assert!(!runtime_root.exists());
    }
}

// ─── host-side helpers extracted from the CLI ────────────────────────────────
//
// These were private to `apps/cli`. They are realization concerns, not CLI
// concerns: which Python a pinned runtime resolves to, how a filesystem state
// is mounted, and how the process sandbox is invoked. `run_portable_bundle_attempt`
// needs them for every caller, so they live beside the executor that uses them.

#[derive(Debug, Clone)]
struct PortableStateMount {
    id: String,
    host_path: PathBuf,
    guest_path: String,
}

#[derive(Debug, Clone)]
struct PortableInputMount {
    env_name: String,
    host_path: PathBuf,
    guest_path: String,
}

struct PortableProcessMounts<'a> {
    state: &'a [PortableStateMount],
    inputs: &'a [PortableInputMount],
}

fn resolve_pinned_python(version: &str) -> Result<(String, String)> {
    if let Ok(output) = Command::new("uv")
        .args(["python", "find", version])
        .env("UV_PYTHON_DOWNLOADS", "never")
        .output()
        && output.status.success()
    {
        let path = String::from_utf8(output.stdout)?.trim().to_owned();
        if !path.is_empty() {
            let executable = executable_path(&path).unwrap_or_else(|| PathBuf::from(&path));
            return verified_python(&executable.display().to_string(), version);
        }
    }
    let provisioned_root = PathBuf::from("/opt/ato/toolchains/python");
    if let Ok(entries) = std::fs::read_dir(&provisioned_root) {
        let mut candidates = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|candidate| {
                candidate == version || candidate.starts_with(&format!("{version}."))
            })
            .collect::<Vec<_>>();
        candidates.sort();
        for candidate in candidates.into_iter().rev() {
            let executable = provisioned_root.join(candidate).join("bin").join("python3");
            if executable.is_file() {
                return verified_python(&executable.display().to_string(), version);
            }
        }
    }
    for executable in ["python3", "python"] {
        let Ok(output) = Command::new(executable).arg("--version").output() else {
            continue;
        };
        let reported = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success() && python_version_matches(&reported, version) {
            let selected = executable_path(executable)
                .unwrap_or_else(|| PathBuf::from(executable))
                .display()
                .to_string();
            return verified_python(&selected, version);
        }
    }
    bail!(
        "selected derivation requires Python {version}; install that runtime with `uv python install {version}`"
    )
}

fn executable_path(executable: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(executable);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate);
    }
    std::env::var_os("PATH").and_then(|search| {
        std::env::split_paths(&search)
            .map(|directory| directory.join(executable))
            .find(|path| path.is_file())
    })
}

fn python_version_matches(reported: &str, required: &str) -> bool {
    reported
        .trim()
        .strip_prefix("Python ")
        .is_some_and(|actual| actual == required || actual.starts_with(&format!("{required}.")))
}

fn verified_python(executable: &str, version: &str) -> Result<(String, String)> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .with_context(|| format!("inspect selected Python executable {executable}"))?;
    let reported = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() || !python_version_matches(&reported, version) {
        bail!(
            "selected Python executable {executable} does not satisfy {version}: {}",
            reported.trim()
        );
    }
    let identity = Command::new(executable)
        .args([
            "-c",
            "import os,sys; print(os.path.realpath(sys.executable))",
        ])
        .output()
        .with_context(|| format!("resolve selected Python executable {executable}"))?;
    let resolved = String::from_utf8(identity.stdout)?.trim().to_owned();
    if !identity.status.success() || resolved.is_empty() || !PathBuf::from(&resolved).is_file() {
        bail!("selected Python executable {executable} did not resolve to a file");
    }
    Ok((resolved, reported.trim().to_owned()))
}

fn resolve_portable_state_mounts(
    route: &ValidatedPortableApplication,
    runtime_root: &Path,
    persistent: Option<&BTreeMap<String, PathBuf>>,
) -> Result<Vec<PortableStateMount>> {
    if let Some(persistent) = persistent
        && persistent.len() != route.derivation.state.len()
    {
        bail!("local Instance filesystem state does not match the selected Derivation");
    }
    route
        .derivation
        .state
        .iter()
        .map(|state| {
            let host_path = match persistent {
                Some(paths) => paths.get(&state.id).cloned().with_context(|| {
                    format!("local Instance filesystem state `{}` is missing", state.id)
                })?,
                None => {
                    let path = runtime_root.join("state").join(&state.id);
                    fs::create_dir_all(&path).with_context(|| {
                        format!("create portable filesystem state {}", path.display())
                    })?;
                    path
                }
            };
            let host_path = host_path.canonicalize().with_context(|| {
                format!(
                    "canonicalize portable filesystem state {}",
                    host_path.display()
                )
            })?;
            if !host_path.is_dir() {
                bail!(
                    "portable filesystem state `{}` is not a directory",
                    state.id
                );
            }
            Ok(PortableStateMount {
                id: state.id.clone(),
                host_path,
                guest_path: state.mount.clone(),
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn portable_process_sandbox_command(
    workspace: &std::path::Path,
    runtime_root: &std::path::Path,
    executable: &str,
    workload: &[String],
    host_port: u16,
    working_dir: &str,
    mounts: PortableProcessMounts<'_>,
    sandbox_shim: &Path,
    sandbox_subcommand: &str,
) -> Result<Vec<String>> {
    let state_mounts = mounts.state;
    let input_mounts = mounts.inputs;
    #[cfg(not(target_os = "linux"))]
    let _ = working_dir;
    let workspace = workspace
        .canonicalize()
        .context("canonicalize portable workspace")?;
    let runtime_root = runtime_root
        .canonicalize()
        .context("canonicalize portable runtime directory")?;
    let executable_path = PathBuf::from(executable);
    let executable = if executable_path.is_absolute() {
        executable_path
    } else {
        workspace.join(working_dir).join(executable_path)
    }
    .canonicalize()
    .with_context(|| format!("canonicalize portable runtime {executable}"))?;
    let interpreter_root = executable
        .parent()
        .and_then(std::path::Path::parent)
        .context("portable runtime has no readable installation root")?
        .to_path_buf();
    if let Some(boundary) = ato_runtime_attempt::launch::host_boundary::active() {
        anyhow::ensure!(
            state_mounts.is_empty(),
            "host-boundary cannot supply namespace state mounts"
        );
        use ato_runtime_attempt::launch::{
            host_boundary,
            resolved::{ResolvedEndpoint, ResolvedReadOnlyInput, ResolvedRuntimeLaunchContext},
        };
        let context = ResolvedRuntimeLaunchContext::new(
            workspace.clone(),
            if working_dir == "." { "" } else { working_dir },
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
            vec![ResolvedEndpoint {
                name: "portable".into(),
                guest_port: None,
                host_port,
            }],
        )?
        .with_read_only_inputs(
            input_mounts
                .iter()
                .map(|input| ResolvedReadOnlyInput {
                    input_id: None,
                    env_name: input.env_name.clone(),
                    path: input.host_path.clone(),
                })
                .collect(),
        );
        host_boundary::prepare_ownership(&context, &runtime_root, boundary)?;
        let policy_path = runtime_root.join("sandbox-policy.json");
        let mut sandboxed = host_boundary::host_boundary_command(
            &context,
            workload,
            sandbox_shim,
            &policy_path,
            &runtime_root,
            boundary,
            &[interpreter_root],
        )?;
        // The same authority runs in CLI and Worker binaries with different shim names.
        sandboxed.argv[1] = sandbox_subcommand.to_owned();
        fs::write(&policy_path, serde_json::to_vec(&sandboxed.policy)?)?;
        return Ok(sandboxed.argv);
    }
    if !state_mounts.is_empty() {
        #[cfg(target_os = "linux")]
        {
            return portable_process_bwrap_command(
                &workspace,
                &runtime_root,
                &interpreter_root,
                workload,
                host_port,
                working_dir,
                mounts,
                sandbox_shim,
                sandbox_subcommand,
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            bail!(
                "local process runtime admission failed: declared filesystem state requires a Linux bubblewrap sandbox"
            );
        }
    }
    let system_roots = [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc",
        "/dev",
        "/proc",
        "/System",
        "/Library",
        "/private/var/db",
        "/opt",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| path.exists());
    let policy = SandboxPolicy::new()
        .allow_read_write([runtime_root.clone()])
        .allow_read_only(
            [workspace, interpreter_root]
                .into_iter()
                .chain(system_roots)
                .chain(input_mounts.iter().map(|input| input.host_path.clone())),
        )
        .with_network(false)
        .allow_tcp_bind([host_port]);
    let policy_path = runtime_root.join("sandbox-policy.json");
    fs::write(&policy_path, serde_json::to_vec_pretty(&policy)?)
        .with_context(|| format!("write portable sandbox policy {}", policy_path.display()))?;
    let mut command = vec![
        sandbox_shim.display().to_string(),
        sandbox_subcommand.to_owned(),
        "--policy".to_owned(),
        policy_path.display().to_string(),
        "--".to_owned(),
    ];
    command.extend(workload.iter().cloned());
    Ok(command)
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn portable_process_bwrap_command(
    workspace: &Path,
    runtime_root: &Path,
    interpreter_root: &Path,
    workload: &[String],
    host_port: u16,
    working_dir: &str,
    mounts: PortableProcessMounts<'_>,
    sandbox_shim: &Path,
    sandbox_subcommand: &str,
) -> Result<Vec<String>> {
    let state_mounts = mounts.state;
    let input_mounts = mounts.inputs;
    let bwrap = executable_path("bwrap")
        .context("local process runtime admission failed: `bwrap` is not on PATH")?;
    let guest_working_dir = if working_dir == "." {
        "/app".to_owned()
    } else {
        format!("/app/{working_dir}")
    };
    let policy = SandboxPolicy::new()
        .allow_read_write(
            state_mounts
                .iter()
                .map(|state| PathBuf::from(&state.guest_path))
                .chain([PathBuf::from("/tmp")]),
        )
        .allow_read_only(
            [
                PathBuf::from("/app"),
                PathBuf::from("/.ato"),
                interpreter_root.to_path_buf(),
                PathBuf::from("/usr"),
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                PathBuf::from("/etc"),
                PathBuf::from("/dev"),
                PathBuf::from("/proc"),
            ]
            .into_iter()
            .chain(
                input_mounts
                    .iter()
                    .map(|input| PathBuf::from(&input.guest_path)),
            ),
        )
        .with_network(false)
        .allow_tcp_bind([host_port]);
    let policy_path = runtime_root.join("sandbox-policy.json");
    fs::write(&policy_path, serde_json::to_vec_pretty(&policy)?)
        .with_context(|| format!("write portable sandbox policy {}", policy_path.display()))?;

    let mut command = vec![
        bwrap.display().to_string(),
        "--unshare-all".to_owned(),
        "--share-net".to_owned(),
        "--die-with-parent".to_owned(),
        "--new-session".to_owned(),
        "--proc".to_owned(),
        "/proc".to_owned(),
        "--dev".to_owned(),
        "/dev".to_owned(),
        "--tmpfs".to_owned(),
        "/tmp".to_owned(),
    ];
    for (source, target, required) in [
        ("/bin", "/bin", false),
        ("/sbin", "/sbin", false),
        ("/lib", "/lib", false),
        ("/lib64", "/lib64", false),
        ("/usr", "/usr", true),
        ("/etc/resolv.conf", "/etc/resolv.conf", false),
        ("/etc/hosts", "/etc/hosts", false),
        ("/etc/ssl", "/etc/ssl", false),
    ] {
        command.extend([
            if required {
                "--ro-bind"
            } else {
                "--ro-bind-try"
            }
            .to_owned(),
            source.to_owned(),
            target.to_owned(),
        ]);
    }
    if !interpreter_root.starts_with("/usr") {
        command.extend([
            "--ro-bind".to_owned(),
            interpreter_root.display().to_string(),
            interpreter_root.display().to_string(),
        ]);
    }
    command.extend([
        "--ro-bind".to_owned(),
        workspace.display().to_string(),
        "/app".to_owned(),
    ]);
    for state in state_mounts {
        command.extend([
            "--bind".to_owned(),
            state.host_path.display().to_string(),
            state.guest_path.clone(),
        ]);
    }
    if !input_mounts.is_empty() {
        command.extend(["--dir".to_owned(), "/ato-inputs".to_owned()]);
    }
    for input in input_mounts {
        command.extend([
            "--ro-bind".to_owned(),
            input.host_path.display().to_string(),
            input.guest_path.clone(),
        ]);
    }
    command.extend([
        "--dir".to_owned(),
        "/.ato".to_owned(),
        "--ro-bind".to_owned(),
        sandbox_shim.display().to_string(),
        "/.ato/ato".to_owned(),
        "--ro-bind".to_owned(),
        policy_path.display().to_string(),
        "/.ato/sandbox-policy.json".to_owned(),
        "--chdir".to_owned(),
        guest_working_dir,
        "/.ato/ato".to_owned(),
        sandbox_subcommand.to_owned(),
        "--policy".to_owned(),
        "/.ato/sandbox-policy.json".to_owned(),
        "--".to_owned(),
    ]);
    command.extend(workload.iter().cloned());
    Ok(command)
}

fn endpoint_port_env_name(port_id: &str) -> String {
    format!(
        "ATO_ENDPOINT_{}_PORT",
        port_id
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

fn state_path_env_name(state_id: &str) -> String {
    format!(
        "ATO_STATE_PATH_{}",
        state_id
            .chars()
            .map(|character| if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    )
}

fn parse_runtime_limit(runtimes: &BTreeMap<String, String>, key: &str) -> Result<u64> {
    runtimes
        .get(key)
        .with_context(|| format!("OCI route omitted {key}"))?
        .parse()
        .with_context(|| format!("OCI route has invalid {key}"))
}

// ─── the shared attempt entry ───────────────────────────────────────────────

/// What one portable-bundle attempt established.
///
/// The receipt and the verification are the same Rust-owned values a CLI Run
/// gets; this API adds no second verdict path. `attempt` carries the typed
/// failure when the attempt was refused, and `stop` says how the candidate's
/// cleanup went when one was stopped.
pub struct PortableBundleAttemptOutcome {
    pub attempt: ato_formation::request::FormationAttempt,
    pub attempt_record: ato_runtime_attempt::journal::AttemptRecordState,
    pub receipt: Option<ContractVerificationReceipt>,
    pub verification: Option<ContractVerification>,
    /// The realization facts the attempt recorded (executor, containment…).
    pub realization: Option<ato_formation::request::RealizationEvidence>,
    pub outcomes: AttemptOutcomes,
    pub stop: Option<ato_runtime_attempt::realize::StopClass>,
    pub error: Option<anyhow::Error>,
}

/// Everything a caller decides before a portable bundle can run.
///
/// The bundle, K, D and tree travel as bytes; everything host-specific is a
/// field here, so the same function serves `ato run`, a local Instance and a
/// hosted Formation attempt.
pub struct PortableBundleAttempt<'a> {
    /// The bundle file as it arrived (transport bytes).
    pub bundle_bytes: &'a [u8],
    /// The decoded bundle those bytes carry.
    pub bundle: &'a ato_objects::PortableApplicationBundle,
    /// The one Derivation the caller selected. Never inferred here.
    pub derivation_ref: &'a str,
    /// Scratch this attempt owns and the attempt removes again.
    pub runtime_root: &'a Path,
    /// Host cache for declared external objects.
    pub cache_root: &'a Path,
    /// Executable carrying the portable sandbox shim subcommand.
    pub sandbox_shim: &'a Path,
    /// The sandbox shim subcommand for this caller.
    pub sandbox_subcommand: &'a str,
    pub request_id: &'a str,
    pub attempt_id: &'a str,
    /// How the caller names this candidate in evidence.
    pub label: &'a str,
    pub runtime_id: &'a str,
    pub profile: &'a RuntimeProfile,
    pub authorization: EffectAuthorization<'a>,
    pub network: NetworkPolicy,
    /// What the receipt names besides K, D and this attempt.
    pub receipt_target: VerificationTargetKind,
    pub run_id: Option<&'a str>,
    pub binding_environment: &'a BTreeMap<String, String>,
    pub interrupt: Option<&'a std::sync::atomic::AtomicBool>,
}

/// Run one already-built portable bundle through the Runtime's common attempt:
/// admission, the start record, realization, observation and verification.
///
/// The caller keeps the choices: which Derivation, what authorization, what a
/// build may reach, and what the receipt names. This function owns only the
/// shared pipeline, so the CLI and the Formation worker cannot drift.
pub fn run_portable_bundle_attempt(
    request: &PortableBundleAttempt<'_>,
    permit: Box<dyn ato_runtime_attempt::journal::AttemptPermit>,
) -> Result<PortableBundleAttemptOutcome> {
    use ato_runtime_attempt::attempt::{
        AttemptRequest, Continuation, ReceiptContext, run_reserved_attempt,
    };

    let validated = crate::validate_bundle_for_derivation(request.bundle, request.derivation_ref)?;
    let transport = crate::bundle_sha256(request.bundle_bytes);
    let portability_profile =
        request
            .bundle
            .portability
            .as_ref()
            .map(|portability| match portability.profile {
                PortableDependencyProfile::Thin => "thin",
                PortableDependencyProfile::Cached => "cached",
                PortableDependencyProfile::Offline => "offline",
            });
    let spec = validated.attempt_spec(None);
    let context = PortableRunContext {
        cache_root: request.cache_root,
        sandbox_shim: request.sandbox_shim,
        sandbox_subcommand: request.sandbox_subcommand,
    };
    let executor = PortableBundleExecutor {
        bundle: request.bundle,
        validated: &validated,
        runtime_root: request.runtime_root,
        static_state: None,
        filesystem_state: None,
        binding_environment: request.binding_environment,
        context: &context,
    };
    let outcome = run_reserved_attempt(
        &AttemptRequest {
            request_id: request.request_id,
            attempt_id: request.attempt_id,
            label: request.label,
            spec: &spec,
            contract_ref: validated.contract_ref.as_str(),
            runtime_id: request.runtime_id,
            profile: request.profile,
            authorization: request.authorization,
            network: request.network,
            browser: None,
            attempt_root: request.runtime_root,
            continuation: Continuation::Stop,
            receipt: ReceiptContext {
                target: request.receipt_target,
                bundle_sha256: Some(&transport),
                portability_profile,
                run_id: request.run_id,
            },
            interrupt: request.interrupt,
            control: None,
        },
        &executor,
        Ok(permit),
    );
    let ato_runtime_attempt::attempt::AttemptOutcome {
        attempt,
        attempt_record,
        verified: _,
        live: _,
        error,
        stop,
    } = outcome;
    Ok(PortableBundleAttemptOutcome {
        receipt: attempt.receipt.clone(),
        verification: attempt.verification.clone(),
        realization: attempt.realization.clone(),
        outcomes: attempt.outcomes.clone(),
        attempt,
        attempt_record,
        stop,
        error,
    })
}
