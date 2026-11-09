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

// The native process group (and its peer broker) is a Unix-only
// materialization: it needs process groups and Unix sockets, and it refuses
// the whole route on other hosts. The import follows the code it belongs to.
#[cfg(unix)]
use crate::process_group::{HelperProcess, ProcessServiceGroup, ServiceSpec};
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
use ato_adapter_process::{
    ProcessAdapter, ProcessHandle, ProcessSpec, StopBudget as ProcessStopBudget,
};
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
use ato_runtime_attempt::launch::process_executor::state_path_env_name;
#[cfg(unix)]
use ato_runtime_attempt::peer_broker::PeerBroker;
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
    /// What this host can do, measured once by the caller. `None` means the
    /// executor probes for itself at admission time; a caller that already
    /// probed (the CLI) passes its own so `ato pack` and `ato run` cannot
    /// disagree.
    pub host: Option<&'a ato_host_facts::HostFacts>,
}

/// Project a validated native OCI route into the existing Runner wire contract.
/// The caller supplies attempt identity, never a replacement for K, D or their
/// Ports. This performs no execution and grants no physical capability.
pub fn native_launch_spec(
    bundle: &ato_objects::PortableApplicationBundle,
    derivation_ref: &str,
    context: ato_ipc::runtime_launch::LaunchContextV1,
    materialization_ref: String,
) -> Result<ato_ipc::runtime_launch::RuntimeLaunchSpecV1> {
    use ato_ipc::runtime_launch::{
        LaunchRealizationV1, LaunchWorkspaceV1, LifecycleV1, OciRealizationV1, OciResourceLimitsV1,
        OciUserV1, PublicEnvV1, RUNTIME_LAUNCH_SPEC_V1_PROTOCOL, ReadinessV1, RuntimeLaunchSpecV1,
    };
    let route = crate::validate_bundle_for_derivation(bundle, derivation_ref)?;
    let d = &route.derivation;
    ensure!(
        route.realization == PortableRealizationKind::OciContainer,
        "native launch needs OCI"
    );
    ensure!(
        d.state.is_empty() && route.model_sets.is_empty() && route.application.bindings.is_empty(),
        "native launch does not yet project state, Model Sets or Bindings"
    );
    let declaration =
        ato_runtime_attempt::native_launch::NativeLaunchDeclaration::from_derivation(d)?;
    match &declaration.adapter.surface {
        ato_formation::native_surface::NativePresentation::Terminal { .. } => {
            declaration.require_host_terminal_ports()?
        }
        ato_formation::native_surface::NativePresentation::PixelStream { .. } => {
            declaration.require_host_pixel_ports()?
        }
    }
    ensure!(
        declaration.derivation_ref == derivation_ref,
        "selected native D changed"
    );
    let step = d.steps.first().context("native route omitted step")?;
    let runtime = &d.runtimes;
    let image = runtime
        .get(OCI_IMAGE_RUNTIME)
        .context("native OCI image missing")?;
    let digest = image
        .rsplit_once('@')
        .context("native image is not pinned")?
        .1
        .to_owned();
    let limits = oci_limits(runtime)?;
    let user: ato_adapter_oci::OciUser = runtime
        .get(OCI_USER_RUNTIME)
        .context("native OCI user missing")?
        .parse()?;
    let spec = RuntimeLaunchSpecV1 {
        protocol: RUNTIME_LAUNCH_SPEC_V1_PROTOCOL.to_owned(),
        context,
        workspace: LaunchWorkspaceV1 {
            materialization_ref,
            cwd_relative: ato_formation::projection::workspace_relative_cwd(&step.cwd)
                .map_err(anyhow::Error::msg)?,
        },
        realization: LaunchRealizationV1::Oci(OciRealizationV1 {
            native_surface: Some(serde_json::to_value(declaration)?),
            image_digest_ref: digest,
            image_reference: Some(image.clone()),
            platform: runtime.get(OCI_PLATFORM_RUNTIME).cloned(),
            resource_limits: Some(OciResourceLimitsV1 {
                memory_bytes: limits.memory_bytes,
                cpu_limit_millis: limits.cpu_limit_millis,
                pids_limit: limits.pids_limit,
            }),
            entrypoint: runtime.get(OCI_ENTRYPOINT_RUNTIME).cloned(),
            user: Some(OciUserV1 {
                uid: user.uid,
                gid: user.gid,
            }),
            argv: Some(step.argv.clone()),
            working_dir: runtime.get(OCI_WORKING_DIR_RUNTIME).cloned(),
            workspace_mount_path: runtime.get(OCI_WORKSPACE_MOUNT_RUNTIME).cloned(),
        }),
        public_env: step
            .env
            .iter()
            .map(|(name, value)| PublicEnvV1 {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        secret_grants: vec![],
        state_attachments: vec![],
        endpoints: vec![],
        readiness: ReadinessV1::Process { timeout_ms: 5000 },
        lifecycle: LifecycleV1 {
            graceful_shutdown_ms: 5000,
            force_kill_after_ms: 10000,
        },
    };
    spec.validate()?;
    Ok(spec)
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

impl PortableBundleExecutor<'_> {
    /// This host's facts: the caller's, or one probe of the selected route's
    /// requirements when the caller did not measure it.
    fn host_facts(&self) -> ato_host_facts::HostFacts {
        match self.context.host {
            Some(host) => host.clone(),
            None => ato_host_facts::HostFacts::probe(std::slice::from_ref(
                &self.execution_requirements(),
            )),
        }
    }

    /// What this route needs from the host, read from the selected Derivation.
    pub fn execution_requirements(&self) -> ato_host_facts::ExecutionRequirements {
        execution_requirements_for(self.validated, self.runtime_root)
    }
}

/// What a validated route needs from this host.
///
/// Read from the D that was selected: the platform the route's images are
/// pinned to, whether it declares filesystem state, and the one path this Run
/// will hand the daemon. Nothing here is derived from argv.
pub fn execution_requirements_for(
    validated: &ValidatedPortableApplication,
    runtime_root: &Path,
) -> ato_host_facts::ExecutionRequirements {
    let derivation = &validated.derivation;
    let mut requirements = match validated.realization {
        PortableRealizationKind::StaticWeb => ato_host_facts::ExecutionRequirements::default(),
        PortableRealizationKind::LocalProcess | PortableRealizationKind::ProcessServiceGroup => {
            ato_host_facts::ExecutionRequirements::process()
        }
        PortableRealizationKind::OciContainer | PortableRealizationKind::OciServiceGroup => {
            ato_host_facts::ExecutionRequirements::container(route_platform(derivation))
        }
    };
    // A state slot that declares it can be found through
    // `ATO_STATE_PATH_<ID>` does not need the Linux sandbox's mount namespace,
    // so it is not a state requirement for admission. Linux still bind-mounts
    // it; the host facts decide only what the host must provide.
    if !derivation.state.is_empty() && !declares_path_delivery(&derivation.state) {
        requirements = requirements.with_state();
    }
    requirements.binding(runtime_root)
}

/// The `os/arch` platform this route's containers are pinned to.
///
/// A service group states it on the Derivation; a single service states it on
/// its serving step. A route that states none returns `None`, and a platform
/// that was not declared is not guessed.
fn route_platform(derivation: &ato_formation::authoring::BoundDerivation) -> Option<String> {
    derivation
        .runtimes
        .get(OCI_PLATFORM_RUNTIME)
        .cloned()
        .or_else(|| {
            derivation
                .steps
                .iter()
                .find_map(|step| step.runtimes.get(OCI_PLATFORM_RUNTIME).cloned())
        })
}

fn refused(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: "admission".to_owned(),
        message: message.into(),
    })
}

/// Whether this host delivers the route's state by path instead of by mount.
///
/// Only a route that says every slot can be found through
/// `ATO_STATE_PATH_<ID>` (`mount_required = false`) qualifies, and only on a
/// host whose process sandbox has no mount namespace: macOS Seatbelt. There
/// the state working copy is handed to the workload as its real host path and
/// only that path is made writable. Anything else keeps the pre-existing
/// behaviour — a Linux bind mount, or refusal — because silently ignoring a
/// declared mount would give the workload a path that does not exist.
/// Whether every declared state slot says it can be found by path.
///
/// Platform-independent: it is a property of the Derivation. Whether a host
/// can use it is decided by the host facts and by `state_delivered_by_path`
/// below (which needs a place to put the path).
fn declares_path_delivery(state: &[ato_formation::authoring::BoundState]) -> bool {
    !state.is_empty() && state.iter().all(|slot| !slot.requires_fixed_mount())
}

fn state_delivered_by_path(state: &[ato_formation::authoring::BoundState]) -> bool {
    cfg!(target_os = "macos") && declares_path_delivery(state)
}

impl CandidateRealizer for PortableBundleExecutor<'_> {
    fn delivers_model_sets(&self) -> bool {
        self.validated.realization == PortableRealizationKind::LocalProcess
    }

    fn admit(&self, _profile: &RuntimeProfile) -> Option<AttemptFailure> {
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
        // The capability question is answered from observed host facts, not
        // from the OS this binary was compiled for: a container route can run
        // on a VM-backed or rootless daemon through the Run's connector, and
        // the facts are what decide which connector that is.
        let requirements = self.execution_requirements();
        let probed;
        let host = match self.context.host {
            Some(host) => host,
            None => {
                probed = ato_host_facts::HostFacts::probe(std::slice::from_ref(&requirements));
                &probed
            }
        };
        let status = ato_host_facts::execution_status(host, &requirements);
        if status.state != ato_host_facts::CapabilityState::Advertised {
            let reason = status
                .reasons
                .iter()
                .find(|reason| !reason.code.is_warning())
                .or_else(|| status.reasons.first());
            let (code, detail) = match reason {
                Some(reason) => (reason.code.as_str(), reason.message.clone()),
                None => ("execution_unavailable", status.describe()),
            };
            return refused(
                code,
                format!(
                    "{} runtime admission failed on this host: {detail}",
                    self.validated.realization.label()
                ),
            );
        }
        if self.validated.realization == PortableRealizationKind::OciContainer
            && host.connector_choice() == Some(ato_host_facts::ConnectorChoice::Relay)
        {
            // A single container has no group network for a relay to join, so
            // this Run has no materialization for it. Refusing here is honest;
            // silently trying the bridge would fail later, less clearly.
            return refused(
                "oci_container_relay_unsupported",
                "this OCI container route needs a relay on this host (a VM-backed or rootless \
                 daemon), which this Run does not implement for a single container; a service \
                 group route runs through the relay, and a native Linux daemon runs directly",
            );
        }
        if requirements.needs_container && host.connector_choice().is_none() {
            return refused(
                "oci_connector_unavailable",
                format!(
                    "{} runtime admission failed: no connector reaches this daemon \
                     (a remote daemon cannot receive this Run's workspace)",
                    self.validated.realization.label()
                ),
            );
        }
        None
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
        // One measurement of this host, used for both the connector choice and
        // the evidence that records it.
        let host = self.host_facts();
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
                self.start_container(&hydrated, workspace, &state_mounts, &host)?
            }
            PortableRealizationKind::OciServiceGroup => {
                self.start_service_group(&hydrated, workspace, &state_mounts, &host)?
            }
            PortableRealizationKind::ProcessServiceGroup => {
                self.start_process_group(workspace, &state_mounts, &input_mounts)?
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
        // A host without a mount namespace has nowhere to put `mount`, so the
        // declared path is the working copy on this filesystem and the
        // workload is told where it is.
        let state_by_path = state_delivered_by_path(&route.derivation.state);
        let mut environment = step.env.clone();
        environment.extend(self.binding_environment.clone());
        environment.insert(endpoint_name, host_port.clone());
        for state in state_mounts {
            let delivered = if state_by_path {
                state.host_path.clone()
            } else {
                PathBuf::from(&state.guest_path)
            };
            environment.insert(
                state_path_env_name(&state.id),
                delivered
                    .to_str()
                    .context("portable state path is not valid UTF-8")?
                    .to_owned(),
            );
        }
        for input in input_mounts {
            environment.insert(
                input.env_name.clone(),
                if state_mounts.is_empty() || state_by_path {
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
        if state_mounts.is_empty() || state_by_path {
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
                state_by_path,
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
        host: &ato_host_facts::HostFacts,
    ) -> Result<Realized, RealizeFailure> {
        let route = self.validated;
        // A single container has no group network to join, so the relay
        // materialization is not implemented for it. Admission refuses that
        // combination first; this is the same decision, stated where the
        // container would have been started.
        if host.connector_choice() == Some(ato_host_facts::ConnectorChoice::Relay) {
            return Err(launch(
                anyhow::anyhow!(
                    "a single OCI container route needs a relay on this host, which this Run does \
                     not implement; use a service group route or a native Linux daemon"
                ),
                &workspace,
            ));
        }
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
        host: &ato_host_facts::HostFacts,
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
        // Which materialization reaches the Ports is a fact about the daemon,
        // recorded in this Run's evidence. A native Linux daemon keeps the
        // direct bridge; a VM-backed or rootless one needs the Run's relay.
        let connector_choice = host.connector_choice();
        let relay = match connector_choice {
            Some(ato_host_facts::ConnectorChoice::Relay) => {
                let destinations = derivation
                    .ports
                    .iter()
                    .filter_map(|port| {
                        port.guest_port
                            .map(|guest_port| (port.from.clone(), guest_port))
                    })
                    .collect::<Vec<_>>();
                Some(ato_runtime_attempt::launch::oci::RelayStart {
                    destinations,
                    architecture: host.docker.architecture.clone(),
                    // The Run's own scratch, not just the route: two Runs of
                    // the same bundle are two relays.
                    label: self.runtime_root.display().to_string(),
                })
            }
            _ => None,
        };
        let mut group = OciNetwork::create(&route.derivation_ref.to_string(), &BTreeMap::new())
            .and_then(|network| {
                start_service_group(
                    network,
                    &workspace,
                    services,
                    StopBudget::DEFAULT,
                    relay,
                    &mut |name, group| wait_until_service_accepts_tcp(route, name, group),
                )
            })
            .map_err(|error| oci_launch_failure(error, &workspace, &oci_runtime))?;
        let base = format!("http://127.0.0.1:{host_port}");
        let mut observation_endpoints = BTreeMap::from([(surface_port.id.clone(), base.clone())]);
        let attempt_spec = route.attempt_spec(None);
        let mut observation_ports: Vec<String> =
            ato_runtime_attempt::verification::required_http_observations(&attempt_spec)
                .into_iter()
                .map(|observation| observation.port_id)
                .collect();
        // A TLS observation reaches the same logical Port as an HTTP one; the
        // endpoint map is what the attempt connects through either way.
        observation_ports.extend(
            ato_runtime_attempt::verification::required_tls_observations(&attempt_spec)
                .into_iter()
                .map(|observation| observation.port_id),
        );
        for port_id in observation_ports {
            if port_id == surface_port.id || observation_endpoints.contains_key(&port_id) {
                continue;
            }
            let port = derivation
                .ports
                .iter()
                .find(|port| port.id == port_id)
                .context("observation names undeclared service Port")?;
            let guest_port = port
                .guest_port
                .context("observation Port omitted guest port")?;
            // The observation reaches the Port the way this Run reaches any
            // Port: the group's connector decides, and hands back an address
            // the observation path can dial.
            let address = group
                .expose(port.from.as_str(), guest_port)
                .context("expose the observation's service Port")?;
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
        // Connector evidence exists only for the materialization that needs
        // it. A direct Run's receipt is byte-for-byte the receipt stg already
        // produced: no /4, no new fields, the same schema.
        if let Some(relay) = group.relay_evidence() {
            execution.connector = Some(group.connector_kind().as_str().to_owned());
            execution.relay_image_digest = Some(relay.image_digest);
            execution.relay_container_id = Some(relay.container_id);
            execution.platform_emulated = Some(matches!(
                host.docker.platforms.get(platform.as_str()),
                Some(ato_host_facts::PlatformSupport::Emulated { .. })
            ));
        }
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

    /// The route's native process group: one service at a time, each started
    /// only after the previous one accepts TCP, and each receiving only its
    /// own Ports, Bindings, and declared peer edges.
    ///
    /// A service's Port is bound from `ATO_ENDPOINT_<PORT_ID>_PORT`; this path
    /// never rewrites an argv element, so a hardcoded guest port fails in the
    /// sandbox instead of being silently replaced. A declared peer edge is
    /// reached through a runtime-owned loopback shim and Unix-socket broker,
    /// never through the peer's real host port.
    #[cfg(unix)]
    fn start_process_group(
        &self,
        workspace: PathBuf,
        state_mounts: &[PortableStateMount],
        input_mounts: &[PortableInputMount],
    ) -> Result<Realized, RealizeFailure> {
        let route = self.validated;
        let derivation = &route.derivation;
        // Admission refuses both; this is the last check of what this launch
        // would actually do, not a second policy.
        if !state_mounts.is_empty() || !input_mounts.is_empty() {
            return Err(launch(
                anyhow::anyhow!(
                    "a native process service group does not model state or Model Set mounts"
                ),
                &workspace,
            ));
        }
        let process_runtime = self.runtime_root.join("process");
        let peer_root = self.runtime_root.join("peer");
        fs::create_dir_all(&process_runtime)
            .and_then(|()| fs::create_dir_all(&peer_root))
            .with_context(|| format!("create {}", process_runtime.display()))
            .map_err(|error| launch(error, &workspace))?;

        // Allocate one loopback host port per declared Port before anything
        // starts, so the env the service reads, the sandbox rule that permits
        // the bind, and readiness all name the same port.
        let mut host_ports = BTreeMap::new();
        for port in &derivation.ports {
            host_ports.insert(
                port.id.as_str(),
                free_loopback_port().map_err(|error| launch(error, &workspace))?,
            );
        }

        // One token per declared edge. The broker table is the authority: a
        // token that is not here cannot name any target.
        struct Edge<'a> {
            consumer: &'a str,
            peer: &'a str,
            token: String,
            shim_port: u16,
        }
        let mut edges = Vec::new();
        let mut broker_routes = BTreeMap::new();
        for (index, step) in derivation.steps.iter().enumerate() {
            for peer in &step.peer_ports {
                let host_port = *host_ports
                    .get(peer.as_str())
                    .expect("validated peer Port has an allocated port");
                let token = format!("{index}-{}-{peer}", step.id);
                broker_routes.insert(
                    token.clone(),
                    std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, host_port)),
                );
                edges.push(Edge {
                    consumer: step.id.as_str(),
                    peer: peer.as_str(),
                    token,
                    shim_port: free_loopback_port().map_err(|error| launch(error, &workspace))?,
                });
            }
        }
        let broker = if broker_routes.is_empty() {
            None
        } else {
            Some(
                PeerBroker::start(&peer_root, broker_routes)
                    .map_err(|error| launch(error.context("start the peer broker"), &workspace))?,
            )
        };
        let broker_socket = broker
            .as_ref()
            .map(|broker| broker.socket_path().to_path_buf());

        let mut services = Vec::with_capacity(derivation.steps.len());
        let mut endpoints = BTreeMap::new();
        let mut runtime_identity = BTreeMap::new();
        for step in &derivation.steps {
            let service_runtime = process_runtime.join(&step.id);
            fs::create_dir_all(&service_runtime)
                .map_err(|error| launch(error.into(), &workspace))?;
            let process_tmp = service_runtime.join("tmp");
            let process_home = service_runtime.join("home");
            fs::create_dir_all(&process_tmp)
                .and_then(|()| fs::create_dir_all(&process_home))
                .map_err(|error| launch(error.into(), &workspace))?;

            let mut environment = step.env.clone();
            for binding in &step.bindings {
                let name = crate::binding_environment_name(binding);
                if let Some(value) = self.binding_environment.get(&name) {
                    environment.insert(name, value.clone());
                }
            }
            for port in derivation.ports.iter().filter(|port| port.from == step.id) {
                environment.insert(
                    endpoint_port_env_name(&port.id),
                    host_ports[port.id.as_str()].to_string(),
                );
            }
            let step_edges = edges
                .iter()
                .filter(|edge| edge.consumer == step.id)
                .collect::<Vec<_>>();
            for edge in &step_edges {
                environment.insert(
                    endpoint_port_env_name(edge.peer),
                    edge.shim_port.to_string(),
                );
            }
            environment.insert(
                "ATO_RUNTIME_DIR".to_owned(),
                service_runtime.display().to_string(),
            );
            for name in ["TMPDIR", "TMP", "TEMP"] {
                environment.insert(name.to_owned(), process_tmp.display().to_string());
            }
            environment.insert("HOME".to_owned(), process_home.display().to_string());
            environment.insert(
                "XDG_CACHE_HOME".to_owned(),
                process_home.join(".cache").display().to_string(),
            );

            let python_version = step
                .runtimes
                .get(PYTHON_RUNTIME)
                .context("native process service omitted python")
                .map_err(|error| launch(error, &workspace))?;
            let mut command = step.argv.clone();
            if !matches!(command[0].as_str(), "python" | "python3") {
                return Err(RealizeFailure::Execution(anyhow::anyhow!(
                    "Python process service `{}` must use a logical python executable in argv[0]",
                    step.id
                )));
            }
            let (executable, version) =
                resolve_pinned_python(python_version).map_err(|error| launch(error, &workspace))?;
            command[0] = executable.clone();
            // No argv port rewrite: the new path binds explicitly through the
            // environment and refuses a hardcoded guest port.

            ato_runtime_attempt::launch::executable_gate::validate_derivation_executable(
                &step.argv[0],
                &workspace.join(&step.cwd),
                &environment,
                derivation,
            )
            .context(ato_runtime_attempt::launch::executable_gate::context_for_launch())
            .map_err(|error| launch(error, &workspace))?;

            // Start this service's peer shims before the service itself. The
            // shim is runtime-owned; its target and token come from D's
            // declared edges, never from the workload.
            let mut helpers = Vec::with_capacity(step_edges.len());
            for edge in &step_edges {
                let socket = broker_socket
                    .as_deref()
                    .expect("a declared edge implies a broker");
                let policy_path =
                    service_runtime.join(format!("peer-{}.json", edge.peer.replace('.', "_")));
                let shim_command = portable_peer_shim_command(
                    &workspace,
                    &service_runtime,
                    socket,
                    edge.shim_port,
                    &edge.token,
                    self.context.sandbox_shim,
                    self.context.sandbox_subcommand,
                    &policy_path,
                )
                .map_err(|error| launch(error, &workspace))?;
                let adapter = ProcessAdapter::new(ProcessSpec {
                    id: format!("{}-peer-{}", step.id, edge.peer),
                    command: shim_command,
                    cwd: PathBuf::from("."),
                    environment: BTreeMap::new(),
                    isolated_group: true,
                })
                .map_err(|error| launch(error.into(), &workspace))?;
                let mut handle = adapter
                    .spawn(&workspace)
                    .map_err(|error| launch(error.into(), &workspace))?;
                wait_for_loopback(
                    std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, edge.shim_port)),
                    &mut handle,
                    &format!("{} peer {}", step.id, edge.peer),
                )
                .map_err(|error| launch(error, &workspace))?;
                helpers.push(HelperProcess {
                    name: format!("{} peer {}", step.id, edge.peer),
                    handle,
                });
            }

            let bind_ports = derivation
                .ports
                .iter()
                .filter(|port| port.from == step.id)
                .map(|port| host_ports[port.id.as_str()])
                .collect::<Vec<_>>();
            let connect_ports = step_edges
                .iter()
                .map(|edge| edge.shim_port)
                .collect::<Vec<_>>();
            let policy_path = service_runtime.join("sandbox-policy.json");
            let command = portable_process_group_sandbox_command(
                &workspace,
                &service_runtime,
                &executable,
                &command,
                &bind_ports,
                &connect_ports,
                broker_socket.as_deref(),
                self.context.sandbox_shim,
                self.context.sandbox_subcommand,
                &policy_path,
            )
            .map_err(|error| launch(error, &workspace))?;

            let first_port = derivation
                .ports
                .iter()
                .find(|port| port.from == step.id)
                .context("service serves no Port")
                .map_err(|error| launch(error, &workspace))?;
            let readiness = std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                host_ports[first_port.id.as_str()],
            ));
            for port in derivation.ports.iter().filter(|port| port.from == step.id) {
                endpoints.insert(
                    port.id.clone(),
                    format!("http://127.0.0.1:{}", host_ports[port.id.as_str()]),
                );
            }
            runtime_identity.insert(step.id.clone(), (executable, version));
            services.push(ServiceSpec {
                name: step.id.clone(),
                command,
                cwd: PathBuf::from(&step.cwd),
                environment,
                readiness,
                helpers,
            });
        }

        let group = ProcessServiceGroup::start(&workspace, services)
            .map_err(|error| launch(error, &workspace))?;
        let surface_port = derivation
            .ports
            .iter()
            .find(|port| port.protocol == ato_formation::authoring::HTTP_PROTOCOL)
            .context("native process group omitted its Surface Port")
            .map_err(|error| launch(error, &workspace))?;
        let surface_step = derivation
            .steps
            .iter()
            .find(|step| step.id == surface_port.from)
            .context("Surface Port has no serving step")
            .map_err(|error| launch(error, &workspace))?;
        let (executable, version) = runtime_identity
            .get(&surface_step.id)
            .cloned()
            .expect("every service resolved its runtime");
        let pid = group
            .pids()
            .into_iter()
            .find(|(name, _)| name == &surface_step.id)
            .map(|(_, pid)| pid);
        let base = endpoints
            .get(&surface_port.id)
            .cloned()
            .expect("the Surface Port has an endpoint");
        let mut execution = evidence(
            "process_service_group",
            Some(executable),
            Some(version),
            pid,
            &base,
        );
        execution.endpoint = Some(base.clone());
        Ok(Realized {
            candidate: Box::new(ProcessServiceGroupCandidate {
                group: Some(group),
                broker,
                endpoints,
                owned: vec![workspace, process_runtime, peer_root],
            }),
            evidence: None,
            execution,
            kept: None,
        })
    }

    #[cfg(not(unix))]
    fn start_process_group(
        &self,
        workspace: PathBuf,
        _state_mounts: &[PortableStateMount],
        _input_mounts: &[PortableInputMount],
    ) -> Result<Realized, RealizeFailure> {
        Err(launch(
            anyhow::anyhow!("a native process service group requires a Unix host"),
            &workspace,
        ))
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
        // Readiness is a connection to the logical Port through the Run's
        // connector, not a dial to a container address.
        if group.connect(name, guest_port).is_ok() {
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
        ..Default::default()
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
        if let Some(mut handle) = self.handle.take() {
            // The leader's exit is not the question: a workload that left a
            // child behind is still running in the same group. The group is
            // stopped and shown empty, and a group that survives is an error
            // this Run reports instead of a cleanup the Drop path hides.
            handle
                .stop_group(ProcessStopBudget::default())
                .context("stopping the selected process group")?;
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

/// A Run's native process service group: every service and peer shim, the
/// broker that authorizes declared edges, and the directories it owns.
#[cfg(unix)]
pub struct ProcessServiceGroupCandidate {
    group: Option<ProcessServiceGroup>,
    broker: Option<PeerBroker>,
    endpoints: BTreeMap<String, String>,
    owned: Vec<PathBuf>,
}

#[cfg(unix)]
impl RunningCandidate for ProcessServiceGroupCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }

    fn exited(&mut self) -> Result<Option<String>> {
        match self.group.as_mut() {
            Some(group) => group.exited(),
            None => Ok(Some("stopped".to_owned())),
        }
    }

    fn stop(mut self: Box<Self>) -> Result<()> {
        if let Some(group) = self.group.take() {
            group.stop()?;
        }
        // The broker goes after every consumer and shim is gone, so no live
        // connection is left pointing at a removed socket.
        drop(self.broker.take());
        remove_owned(&self.owned)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(unix)]
impl Drop for ProcessServiceGroupCandidate {
    fn drop(&mut self) {
        drop(self.group.take());
        drop(self.broker.take());
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

/// Wait until a runtime-owned helper accepts TCP on its loopback port.
///
/// A helper that exits before listening is a launch failure with its own
/// message: the shim's own stderr already says why (for example, the host
/// refused to apply its sandbox).
#[cfg(unix)]
fn wait_for_loopback(
    address: std::net::SocketAddr,
    helper: &mut ProcessHandle,
    name: &str,
) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = helper.try_wait()? {
            bail!("{name} exited before it listened on {address}: {status}");
        }
        if std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(200))
            .is_ok()
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!("{name} did not accept TCP on {address} within 10s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Host paths a contained workload may read: its own runtime and the system
/// libraries it needs, never the host's home or a sibling Run.
#[cfg(unix)]
fn portable_system_roots() -> Vec<PathBuf> {
    [
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
    .filter(|path| path.exists())
    .collect()
}

/// The sandbox wrapper for one service of a native process group.
///
/// `bind_ports` are the host ports this service may bind; `connect_ports` are
/// the loopback shim ports of the edges its D declares. No other TCP is
/// reachable. `broker_socket` is the single Unix socket a shim may open.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn portable_process_group_sandbox_command(
    workspace: &Path,
    runtime_root: &Path,
    executable: &str,
    workload: &[String],
    bind_ports: &[u16],
    connect_ports: &[u16],
    broker_socket: Option<&Path>,
    sandbox_shim: &Path,
    sandbox_subcommand: &str,
    policy_path: &Path,
) -> Result<Vec<String>> {
    let workspace = workspace
        .canonicalize()
        .context("canonicalize portable workspace")?;
    let runtime_root = runtime_root
        .canonicalize()
        .context("canonicalize portable service runtime")?;
    let executable = PathBuf::from(executable)
        .canonicalize()
        .with_context(|| format!("canonicalize portable runtime {executable}"))?;
    let interpreter_root = executable
        .parent()
        .and_then(Path::parent)
        .context("portable runtime has no readable installation root")?
        .to_path_buf();
    let mut policy = SandboxPolicy::new()
        .allow_read_write([runtime_root.clone()])
        .allow_read_only(
            [workspace, interpreter_root, sandbox_shim.to_path_buf()]
                .into_iter()
                .chain(portable_system_roots()),
        )
        .with_network(false)
        .allow_tcp_bind(bind_ports.iter().copied())
        .allow_tcp_connect(connect_ports.iter().copied());
    if let Some(socket) = broker_socket {
        policy = policy.with_ipc_socket_paths([socket.to_path_buf()]);
    }
    fs::write(policy_path, serde_json::to_vec_pretty(&policy)?)
        .with_context(|| format!("write service sandbox policy {}", policy_path.display()))?;
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

/// The sandbox wrapper for one peer shim. It may bind only its own loopback
/// port and open only the broker socket.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn portable_peer_shim_command(
    workspace: &Path,
    runtime_root: &Path,
    broker_socket: &Path,
    shim_port: u16,
    token: &str,
    sandbox_shim: &Path,
    sandbox_subcommand: &str,
    policy_path: &Path,
) -> Result<Vec<String>> {
    let workspace = workspace
        .canonicalize()
        .context("canonicalize portable workspace")?;
    let runtime_root = runtime_root
        .canonicalize()
        .context("canonicalize portable service runtime")?;
    // The broker may live outside this service's runtime root: when the Run
    // root is too deep for `sun_path`, it moves to a short directory under the
    // platform temp dir. The shim still has to resolve that path, which needs
    // metadata lookup on its ancestors, so the socket's directory is readable
    // as well as the socket itself.
    let broker_root = broker_socket.parent().map(Path::to_path_buf);
    let read_only = [
        Some(workspace.clone()),
        Some(sandbox_shim.to_path_buf()),
        broker_root,
    ]
    .into_iter()
    .flatten()
    .chain(portable_system_roots());
    let policy = SandboxPolicy::new()
        .allow_read_write([runtime_root.clone()])
        .allow_read_only(read_only)
        .with_network(false)
        .allow_tcp_bind([shim_port])
        .with_ipc_socket_paths([broker_socket.to_path_buf()]);
    fs::write(policy_path, serde_json::to_vec_pretty(&policy)?)
        .with_context(|| format!("write peer shim policy {}", policy_path.display()))?;
    Ok(vec![
        sandbox_shim.display().to_string(),
        sandbox_subcommand.to_owned(),
        "--policy".to_owned(),
        policy_path.display().to_string(),
        "--".to_owned(),
        sandbox_shim.display().to_string(),
        "__portable-peer-shim".to_owned(),
        "--broker".to_owned(),
        broker_socket.display().to_string(),
        "--token".to_owned(),
        token.to_owned(),
        "--port".to_owned(),
        shim_port.to_string(),
    ])
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
    /// The host has no mount namespace: state and input paths are delivered as
    /// real host paths and only those paths are made writable/readable.
    state_by_path: bool,
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
    let executable = PathBuf::from(executable)
        .canonicalize()
        .with_context(|| format!("canonicalize portable runtime {executable}"))?;
    let interpreter_root = executable
        .parent()
        .and_then(std::path::Path::parent)
        .context("portable runtime has no readable installation root")?
        .to_path_buf();
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
            if !mounts.state_by_path {
                // A route that declares a fixed mount but does not say it can
                // be found by path cannot run where there is no mount
                // namespace. Refusing is the point: `mount` is not ignored.
                bail!(
                    "local process runtime admission failed: declared filesystem state requires a Linux bubblewrap sandbox"
                );
            }
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
    // Without a mount namespace the slot is not mapped to `mount`; it lives
    // where the Instance keeps it and only that directory is writable.
    let mut read_write = vec![runtime_root.clone()];
    if mounts.state_by_path {
        read_write.extend(state_mounts.iter().map(|state| state.host_path.clone()));
    }
    let policy = SandboxPolicy::new()
        .allow_read_write(read_write)
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
    /// What the caller already measured about this host. `None` probes at
    /// admission time; a caller that probed passes its answer so one Run
    /// cannot report two different capabilities.
    pub host: Option<&'a ato_host_facts::HostFacts>,
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
        host: request.host,
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

#[cfg(all(test, unix))]
mod state_path_tests {
    use super::*;
    use ato_formation::authoring::{BoundState, StateAccess};

    fn slot(mount_required: Option<bool>) -> BoundState {
        BoundState {
            id: "data".to_owned(),
            protocol: ato_formation::authoring::STATE_FILESYSTEM_PROTOCOL.to_owned(),
            mount: "/data".to_owned(),
            access: StateAccess::ReadWrite,
            mount_required,
        }
    }

    #[test]
    fn only_a_slot_that_explicitly_needs_no_mount_may_be_delivered_by_path() {
        // The declaration is necessary on every platform; the host decides
        // whether it can use it. On Linux the bind mount still happens.
        assert!(!state_delivered_by_path(&[]));
        assert!(!state_delivered_by_path(&[slot(None)]));
        assert_eq!(
            state_delivered_by_path(&[slot(Some(false))]),
            cfg!(target_os = "macos")
        );
        assert!(!state_delivered_by_path(&[slot(None), slot(Some(false))]));
        assert!(!state_delivered_by_path(&[slot(Some(false)), slot(None)]));
        if cfg!(target_os = "macos") {
            assert!(state_delivered_by_path(&[slot(Some(false))]));
            assert!(state_delivered_by_path(&[
                slot(Some(false)),
                slot(Some(false))
            ]));
        }
    }

    const AUTHORED_SOURCE: &str = r#"
schema = "ato.capsule/2"

[application]
title = "Native state"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[state]]
id = "data"
use = "ato.state.filesystem@1"
mount = "/data"
access = "read-write"
MOUNT_REQUIRED

[contract]
mode = "all"

[[contract.observation]]
id = "health"
use = "ato.contract.http@1"
port = "app.http"
path = "/health"
status = 200

[[derivation]]
id = "serve"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }

[effects]
default = "pure"
"#;

    fn admission_of(source: &str) -> Option<AttemptFailure> {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("app.py"), "print('ok')\n").unwrap();
        std::fs::write(root.path().join("capsule.toml"), source).unwrap();
        let (_, bundle) = crate::build_authored_bundle_v2(root.path()).unwrap();
        let route_ref = bundle.index.derivations[0].to_string();
        let validated = crate::validate_bundle_for_derivation(&bundle, &route_ref).unwrap();
        let context = PortableRunContext {
            cache_root: root.path(),
            sandbox_shim: Path::new("/usr/bin/true"),
            sandbox_subcommand: "__portable-sandbox-exec",
            host: None,
        };
        let executor = PortableBundleExecutor {
            bundle: &bundle,
            validated: &validated,
            runtime_root: root.path(),
            static_state: None,
            filesystem_state: None,
            binding_environment: &BTreeMap::new(),
            context: &context,
        };
        // Keep the source tree alive for the duration of the call.
        let outcome = executor.admit(&RuntimeProfile::default());
        drop(root);
        outcome
    }

    #[test]
    fn state_admission_uses_the_declaration_instead_of_ignoring_the_mount() {
        let fixed = admission_of(&AUTHORED_SOURCE.replace("MOUNT_REQUIRED", ""));
        let by_path =
            admission_of(&AUTHORED_SOURCE.replace("MOUNT_REQUIRED", "mount_required = false"));
        if cfg!(target_os = "macos") {
            let refused = fixed.expect("a fixed-mount slot is still refused on macOS");
            assert_eq!(refused.code, "state_needs_linux_sandbox");
            assert!(
                by_path.is_none(),
                "a path-delivered slot is admitted without a Linux sandbox"
            );
        } else {
            assert!(fixed.is_none());
            assert!(by_path.is_none());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_path_delivered_slot_is_the_only_extra_writable_path_and_is_not_mounted() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let runtime = root.path().join("run");
        let toolchain = root.path().join("toolchain/bin");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&toolchain).unwrap();
        fs::write(toolchain.join("python3"), b"#!/bin/sh\n").unwrap();
        let state = root.path().join("state/data");
        fs::create_dir_all(&state).unwrap();
        let state = state.canonicalize().unwrap();
        let state_mounts = vec![PortableStateMount {
            id: "data".to_owned(),
            host_path: state.clone(),
            guest_path: "/data".to_owned(),
        }];
        let command = portable_process_sandbox_command(
            &workspace,
            &runtime,
            toolchain.join("python3").to_str().unwrap(),
            &["python3".to_owned()],
            8700,
            ".",
            PortableProcessMounts {
                state: &state_mounts,
                inputs: &[],
                state_by_path: true,
            },
            Path::new("/usr/bin/true"),
            "__portable-sandbox-exec",
        )
        .expect("a path-delivered slot builds a sandbox command on macOS");
        assert_eq!(command[0], "/usr/bin/true");
        assert!(
            !command.iter().any(|argument| argument == "bwrap"),
            "there is no mount namespace to build: {command:?}"
        );
        let policy: SandboxPolicy = serde_json::from_slice(
            &fs::read(runtime.join("sandbox-policy.json")).expect("policy written"),
        )
        .unwrap();
        assert!(
            policy.read_write_paths.contains(&state),
            "the slot's real host path is writable: {:?}",
            policy.read_write_paths
        );
        assert!(
            !policy.read_write_paths.contains(&PathBuf::from("/data")),
            "the guest path is not a host path and must not be allowed"
        );
    }

    /// A real process launch, twice, over the same Instance state directory.
    ///
    /// `sandbox_init` is unavailable when the test itself already runs inside
    /// a macOS sandbox (the pre-existing stateless process route fails the
    /// same way), so the shim here only forwards the workload. What the
    /// sandbox *would* allow is asserted by
    /// `a_path_delivered_slot_is_the_only_extra_writable_path_and_is_not_mounted`;
    /// this test pins the delivery itself: the host path is in
    /// `ATO_STATE_PATH_DATA`, and the second Run reads what the first wrote.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_restarted_process_finds_the_state_the_previous_run_wrote() {
        const APP: &str = r#"
import json, os, sys
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

root = os.environ["ATO_STATE_PATH_DATA"]
path = os.path.join(root, "counter")
value = int(open(path).read()) + 1 if os.path.exists(path) else 1
open(path, "w").write(str(value))

class Server(ThreadingHTTPServer):
    # http.server resolves the bind address with a reverse DNS lookup
    # (`getfqdn`), which can block for the resolver's timeout on a CI host with
    # slow DNS - before the socket is listening, and with no output. A test
    # fixture does not need the FQDN.
    def server_bind(self):
        import socketserver
        socketserver.TCPServer.server_bind(self)
        self.server_name = "localhost"
        self.server_port = self.server_address[1]

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"counter": value}).encode()
        self.send_response(HTTPStatus.OK)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass

Server(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), Handler).serve_forever()
"#;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("app.py"), APP).unwrap();
        std::fs::write(
            source.join("capsule.toml"),
            AUTHORED_SOURCE.replace("MOUNT_REQUIRED", "mount_required = false"),
        )
        .unwrap();
        let (_, bundle) = crate::build_authored_bundle_v2(&source).unwrap();
        let route_ref = bundle.index.derivations[0].to_string();
        let validated = crate::validate_bundle_for_derivation(&bundle, &route_ref).unwrap();

        // A shim that forwards the workload instead of applying a policy. It
        // keeps the workload's output so a failed start has a cause in the
        // test's own failure message.
        let shim = root.path().join("shim.sh");
        std::fs::write(
            &shim,
            "#!/bin/sh\nwhile [ \"$1\" != \"--\" ]; do shift; done\nshift\n\
             exec \"$@\" >> \"$ATO_RUNTIME_DIR/workload.log\" 2>&1\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The Instance owns this directory; neither runtime scratch above does.
        let state = root.path().join("instance/state/data");
        std::fs::create_dir_all(&state).unwrap();
        let state = state.canonicalize().unwrap();
        let state_mounts = vec![PortableStateMount {
            id: "data".to_owned(),
            host_path: state.clone(),
            guest_path: "/data".to_owned(),
        }];

        let run = |name: &str| {
            let runtime_root = root.path().join(name);
            std::fs::create_dir_all(&runtime_root).unwrap();
            let workspace = runtime_root.join("workspace");
            std::fs::create_dir_all(&workspace).unwrap();
            std::fs::copy(source.join("app.py"), workspace.join("app.py")).unwrap();
            let context = PortableRunContext {
                cache_root: root.path(),
                sandbox_shim: &shim,
                sandbox_subcommand: "__portable-sandbox-exec",
                host: None,
            };
            let executor = PortableBundleExecutor {
                bundle: &bundle,
                validated: &validated,
                runtime_root: &runtime_root,
                static_state: None,
                filesystem_state: None,
                binding_environment: &BTreeMap::new(),
                context: &context,
            };
            let realized = match executor.start_process(workspace, &state_mounts, &[]) {
                Ok(realized) => realized,
                Err(RealizeFailure::Execution(error))
                | Err(RealizeFailure::Launch { error, .. })
                | Err(RealizeFailure::Stopped { error, .. })
                | Err(RealizeFailure::Abandoned { error, .. }) => {
                    panic!("the process should start: {error:#}")
                }
            };
            let url = realized
                .candidate
                .endpoints()
                .get("app.http")
                .cloned()
                .expect("the serving endpoint");
            let counter = wait_for_counter(&url, &runtime_root);
            realized
                .candidate
                .stop()
                .unwrap_or_else(|error| panic!("the process should stop: {error}"));
            counter
        };

        assert_eq!(run("run-one"), 1, "the first Run starts from empty state");
        assert_eq!(
            run("run-two"),
            2,
            "the second Run opens the state the first Run left behind"
        );
    }

    /// The workload's own output, when the shim kept it.
    #[cfg(target_os = "macos")]
    fn workload_log(runtime_root: &std::path::Path) -> String {
        std::fs::read_to_string(runtime_root.join("process/workload.log"))
            .unwrap_or_else(|_| "(no workload output)".to_owned())
    }

    /// Poll the workload until it answers; stop is the caller's business.
    #[cfg(target_os = "macos")]
    fn wait_for_counter(base: &str, runtime_root: &std::path::Path) -> u64 {
        use std::io::{Read, Write};
        use std::net::TcpStream;
        let authority = base.strip_prefix("http://").expect("loopback URL");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let attempt = (|| -> std::io::Result<String> {
                let mut stream = TcpStream::connect(authority)?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
                write!(
                    stream,
                    "GET /counter HTTP/1.0\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
                )?;
                let mut response = String::new();
                stream.read_to_string(&mut response)?;
                Ok(response)
            })();
            if let Ok(response) = attempt
                && let Some((_, body)) = response.split_once("\r\n\r\n")
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(body)
            {
                return value["counter"].as_u64().expect("counter value");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the workload never answered at {base}; workload log:\n{}",
                workload_log(runtime_root)
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_declared_fixed_mount_is_still_refused_where_there_is_no_mount_namespace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let runtime = root.path().join("run");
        let toolchain = root.path().join("toolchain/bin");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&toolchain).unwrap();
        fs::write(toolchain.join("python3"), b"#!/bin/sh\n").unwrap();
        let state = root.path().join("state/data");
        fs::create_dir_all(&state).unwrap();
        let state_mounts = vec![PortableStateMount {
            id: "data".to_owned(),
            host_path: state,
            guest_path: "/data".to_owned(),
        }];
        let error = portable_process_sandbox_command(
            &workspace,
            &runtime,
            toolchain.join("python3").to_str().unwrap(),
            &["python3".to_owned()],
            8700,
            ".",
            PortableProcessMounts {
                state: &state_mounts,
                inputs: &[],
                state_by_path: false,
            },
            Path::new("/usr/bin/true"),
            "__portable-sandbox-exec",
        )
        .expect_err("a plain state slot keeps the Linux-only refusal");
        assert!(
            error
                .to_string()
                .contains("requires a Linux bubblewrap sandbox"),
            "{error}"
        );
    }
}

#[cfg(all(test, unix))]
mod process_group_policy_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let runtime = root.path().join("runtime");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&runtime).unwrap();
        let shim = root.path().join("ato");
        fs::write(&shim, b"shim").unwrap();
        let broker = root.path().join("peer").join("broker.sock");
        (root, workspace, runtime, shim, broker)
    }

    fn policy(path: &Path) -> SandboxPolicy {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_service_policy_grants_only_its_ports_and_its_declared_peer_shims() {
        let (_root, workspace, runtime, shim, broker) = fixture();
        let policy_path = runtime.join("policy.json");
        let command = portable_process_group_sandbox_command(
            &workspace,
            &runtime,
            "/bin/sh",
            &[
                "python3".to_owned(),
                "proxy.py".to_owned(),
                "8080".to_owned(),
            ],
            &[43101],
            &[43102],
            Some(&broker),
            &shim,
            "__portable-sandbox-exec",
            &policy_path,
        )
        .unwrap();
        // The workload argv crosses unchanged: this path never infers or
        // rewrites a Port from it.
        assert_eq!(
            &command[command.len() - 3..],
            ["python3", "proxy.py", "8080"]
        );
        let policy = policy(&policy_path);
        assert_eq!(policy.allowed_bind_tcp_ports, [43101]);
        assert_eq!(policy.allowed_connect_tcp_ports, [43102]);
        assert!(!policy.allow_network);
        assert_eq!(policy.ipc_socket_paths, [broker]);
        // A peer's real host port never appears in a consumer's authority.
        assert!(!policy.allowed_connect_tcp_ports.contains(&43199));
    }

    #[test]
    fn a_shim_policy_binds_its_loopback_port_and_opens_only_the_broker() {
        let (_root, workspace, runtime, shim, broker) = fixture();
        let policy_path = runtime.join("shim.json");
        let command = portable_peer_shim_command(
            &workspace,
            &runtime,
            &broker,
            43102,
            "0-web-backend.http",
            &shim,
            "__portable-sandbox-exec",
            &policy_path,
        )
        .unwrap();
        assert!(command.iter().any(|arg| arg == "__portable-peer-shim"));
        let policy = policy(&policy_path);
        assert_eq!(policy.allowed_bind_tcp_ports, [43102]);
        assert!(policy.allowed_connect_tcp_ports.is_empty());
        assert_eq!(
            policy.ipc_socket_paths.as_slice(),
            std::slice::from_ref(&broker)
        );
        assert!(!policy.allow_network);
        // A broker outside the service runtime (the short-path fallback under
        // the platform temp directory) still has to be traversable: path
        // resolution needs metadata on its ancestors, and without them
        // connect() fails with EPERM.
        let broker_root = broker.parent().unwrap().to_path_buf();
        assert!(
            policy.read_only_paths.contains(&broker_root),
            "the shim policy must let the shim reach the broker's directory: {:?}",
            policy.read_only_paths
        );
    }
}
