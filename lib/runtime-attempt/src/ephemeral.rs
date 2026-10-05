//! Temporary realization: bring a candidate up on the local Runtime, measure
//! it, then destroy it (ADR-019).
//!
//! A Formation is not done until the candidate is observed satisfying the
//! Contract. For a process lane that means a real process on a real port, so
//! the candidate is launched — but NOT by a Formation-specific spawn. It goes
//! through the Runtime's own process executor:
//!
//! ```text
//! RuntimeLaunchSpecV1 + ResolvedRuntimeLaunchContext
//!     -> launch_process_with   (bwrap namespaces, Landlock shim, cleared env,
//!                               allocated host port, isolated process group)
//!     -> wait_until_ready      (the Runtime's loopback readiness probe)
//!     -> LaunchedProcess::stop (TERM -> KILL on the group, proven gone)
//! ```
//!
//! The default lifecycle has no lease, durable state attachment, stable
//! endpoint or record pipeline. Explicit functional verification may borrow
//! already assigned state through `VerificationStateBindings`; its caller owns
//! the writer lifecycle and the state survives candidate cleanup. The realization
//! exists to be measured inside one attempt, and [`TemporaryRealization`] is
//! destroyed before the attempt returns — on every path, because `Drop` does
//! it when nothing else did.
//!
//! The workspace the candidate sees is a DISPOSABLE COPY of the build output.
//! The Runtime mounts it read-only at `/app` and gives the candidate a tmpfs
//! `/tmp`; whatever the candidate writes goes with the realization, and the
//! artifact that is kept is packed from the untouched build output instead.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::control::{AttemptPhase, ExecutionControl};
use crate::launch::process_executor::{
    LaunchedProcess, LoopbackReadinessProbe, ProcessLaunchHost, ReadinessProbe,
    launch_process_with, launch_process_with_scoped_network, wait_until_ready,
};
use crate::launch::resolved::{
    ResolvedEndpoint, ResolvedRuntimeLaunchContext, ResolvedStateAttachment,
};
use crate::launch::sandbox::endpoint_port_env_name;
use anyhow::{Context, Result, bail};
use ato_formation::verify::RuntimeHttpObservation;
use ato_formation::{authoring::BoundDerivation, execution::ExecutionPlan};
use ato_ipc::runtime_launch::{
    EndpointAllocationV1, EndpointV1, LaunchContextV1, LaunchRealizationV1, LaunchWorkspaceV1,
    LifecycleV1, ProcessRealizationV1, PublicEnvV1, RUNTIME_LAUNCH_SPEC_V1_PROTOCOL, ReadinessV1,
    RuntimeLaunchSpecV1, SecretGrantV1, StateAccessV1, StateAttachmentV1,
};

/// How long a candidate gets to come up before the attempt is failed.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-request budget once the candidate is up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bodies larger than this are not hashed into evidence.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// How much of the candidate's output is kept while it runs. An untrusted
/// candidate cannot fill the disk by talking.
const OUTPUT_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
/// How much of the candidate's output a failure message carries.
const OUTPUT_TAIL_BYTES: usize = 4 * 1024;
/// The teardown bounds handed to the Runtime's stop.
const LIFECYCLE: LifecycleV1 = LifecycleV1 {
    graceful_shutdown_ms: 2_000,
    force_kill_after_ms: 4_000,
};

/// A port the Contract observes, as the Derivation declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredPort {
    /// The Contract's port id (`app.http`).
    pub port_id: String,
    /// The port the Derivation says the workload listens on.
    pub guest_port: u16,
}

/// An HTTP observation the Contract needs, by logical port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredObservation {
    pub port_id: String,
    pub path: String,
}

/// Ports a realization in this worker has chosen and its candidate has not
/// bound yet.
///
/// A process realization releases the port it chose before the candidate
/// binds it. Another realization, or a verifier's own loopback server, could
/// otherwise be handed that port in the window. Every port chooser in this
/// worker skips the ports listed here; nothing waits on anybody. Other
/// processes on the host are not covered — a collision with them still fails
/// readiness visibly.
static PENDING_PORTS: std::sync::Mutex<std::collections::BTreeSet<u16>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

fn pending_ports() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<u16>> {
    PENDING_PORTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Is `port` chosen by a realization that has not bound it yet?
pub(crate) fn port_is_pending(port: u16) -> bool {
    pending_ports().contains(&port)
}

/// The ports one realization chose, released from the list when it is
/// dropped — after readiness, or on any failure path.
struct PendingClaim(Vec<u16>);

impl PendingClaim {
    /// Claim `port` unless another realization already has.
    fn claim(&mut self, port: u16) -> bool {
        let claimed = pending_ports().insert(port);
        if claimed {
            self.0.push(port);
        }
        claimed
    }
}

impl Drop for PendingClaim {
    fn drop(&mut self) {
        let mut pending = pending_ports();
        for port in &self.0 {
            pending.remove(port);
        }
    }
}

/// What to realize, temporarily.
pub struct TemporaryRealizationRequest<'a> {
    /// The immutable build output. Copied, never mounted: the copy is what
    /// the candidate sees, so nothing it does can reach this tree.
    pub workspace: &'a Path,
    /// Scratch owned by this realization and removed with it.
    pub scratch: &'a Path,
    pub derivation: &'a BoundDerivation,
    pub plan: &'a ExecutionPlan,
    pub ports: &'a [RequiredPort],
    /// The binary bwrap re-enters as `sandbox-exec`.
    pub shim: &'a Path,
    /// Correlates the realization with its attempt.
    pub attempt_id: &'a str,
}

/// Where the verifier reaches a logical port.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RealizedEndpoint {
    pub port_id: String,
    pub guest_port: u16,
    pub host_port: u16,
}

/// A running candidate, alive only while this value is.
pub struct TemporaryRealization {
    launched: Option<LaunchedProcess>,
    endpoints: Vec<RealizedEndpoint>,
    scratch: PathBuf,
    output: PathBuf,
    variables: Vec<crate::variables::ResolvedVariable>,
    #[cfg(unix)]
    ingress: Vec<crate::network_bridge::IngressBridge>,
}

impl TemporaryRealization {
    /// Launch the candidate through the Runtime's process executor and wait
    /// until every required port accepts connections.
    pub fn launch(request: &TemporaryRealizationRequest<'_>) -> Result<Self> {
        Self::launch_inner(request, None, &[], None, None)
    }

    pub fn launch_scoped(
        request: &TemporaryRealizationRequest<'_>,
        runtime_gate: &Path,
    ) -> Result<Self> {
        Self::launch_inner(request, Some(runtime_gate), &[], None, None)
    }

    pub fn launch_scoped_with_variables(
        request: &TemporaryRealizationRequest<'_>,
        runtime_gate: &Path,
        variables: &[crate::variables::ResolvedVariable],
    ) -> Result<Self> {
        Self::launch_inner(request, Some(runtime_gate), variables, None, None)
    }

    pub fn launch_controlled(
        request: &TemporaryRealizationRequest<'_>,
        runtime_gate: Option<&Path>,
        variables: &[crate::variables::ResolvedVariable],
        control: Option<&crate::control::ExecutionControl>,
    ) -> Result<Self> {
        Self::launch_inner(request, runtime_gate, variables, None, control)
    }

    /// Launch with state already assigned by the authenticated control plane.
    /// Cleanup stops the process and removes candidate scratch, never these
    /// caller-owned working copies. This does not acquire or release a writer.
    pub fn launch_controlled_with_state(
        request: &TemporaryRealizationRequest<'_>,
        runtime_gate: Option<&Path>,
        variables: &[crate::variables::ResolvedVariable],
        state: Option<&crate::state_bindings::VerificationStateBindings<'_>>,
        control: Option<&crate::control::ExecutionControl>,
    ) -> Result<Self> {
        Self::launch_inner(request, runtime_gate, variables, state, control)
    }
    fn launch_inner(
        request: &TemporaryRealizationRequest<'_>,
        runtime_gate: Option<&Path>,
        variables: &[crate::variables::ResolvedVariable],
        state: Option<&crate::state_bindings::VerificationStateBindings<'_>>,
        control: Option<&crate::control::ExecutionControl>,
    ) -> Result<Self> {
        let check = || control.map_or(Ok(()), |c| c.remaining(AttemptPhase::Launch).map(|_| ()));
        check()?;
        if let Some(state) = state {
            if runtime_gate.is_none() {
                bail!("verification_state_scoped_grant_required");
            }
            // Check ownership boundaries before this realization takes scratch
            // ownership, so an invalid binding cannot be deleted on rejection.
            state.validate(
                &request.derivation.state,
                request.workspace,
                request.scratch,
            )?;
        }
        #[cfg(not(unix))]
        if runtime_gate.is_some() {
            bail!("scoped runtime requires Unix");
        }
        let serve = request.plan.serving(request.derivation);
        if serve.argv.is_empty() {
            bail!("the intent declares no launch argv");
        }

        // Owned from the first byte written: any error below drops it, and
        // dropping it removes the scratch and stops anything launched.
        // Absolute: bwrap binds these from a working directory of its own.
        let scratch = std::path::absolute(request.scratch)
            .context("cannot resolve the realization scratch")?;
        let mut realization = Self {
            variables: variables
                .iter()
                .map(|v| crate::variables::ResolvedVariable {
                    phase: v.phase,
                    grant_ref: v.grant_ref.clone(),
                    value: crate::launch::resolved::ResolvedSecret::new(
                        v.value.name(),
                        v.value.expose_for_spawn(),
                    ),
                })
                .collect(),
            launched: None,
            endpoints: Vec::new(),
            scratch: scratch.to_path_buf(),
            output: scratch.join("candidate.log"),
            #[cfg(unix)]
            ingress: Vec::new(),
        };
        let workspace_root = scratch.join("workspace");
        std::fs::create_dir_all(&workspace_root)
            .context("cannot create the realization workspace")?;
        crate::plan::copy_tree_with_guard(request.workspace, &workspace_root, &check)
            .context("cannot copy the build output into the realization")?;
        let runtime_root = scratch.join("runtime");
        std::fs::create_dir_all(&runtime_root)
            .context("cannot create the realization runtime root")?;

        // ── ports ───────────────────────────────────────────────────────────
        //
        // The Derivation names a guest port. A process realization has no NAT,
        // so the port the workload binds IS a host port, and the Runtime
        // tells the workload which one through its endpoint ABI:
        // ATO_ENDPOINT_<NAME>_PORT. The guest port is preferred when it is
        // free, otherwise the kernel picks one.
        //
        // The argv and environment are executed EXACTLY as the Derivation
        // states them. Nothing here reads a string for a port number: a
        // Derivation that reads the endpoint variable runs either way; one
        // that binds its guest port literally runs when that port is free and
        // fails, visibly, when it is not.
        // Listed until every port the candidate needs accepts connections.
        let mut claim = PendingClaim(Vec::new());
        let mut endpoints = Vec::new();
        let mut resolved = Vec::new();
        let mut declared = Vec::new();
        for port in request.ports {
            if endpoints
                .iter()
                .any(|endpoint: &RealizedEndpoint| endpoint.port_id == port.port_id)
            {
                continue;
            }
            let preferred =
                reserve_port(port.guest_port).filter(|&host_port| claim.claim(host_port));
            let (host_port, allocation, preferred_port) = match preferred {
                Some(host_port) => (
                    host_port,
                    EndpointAllocationV1::Preferred,
                    Some(port.guest_port),
                ),
                None => (
                    allocate_unclaimed_host_port(&mut claim)?,
                    EndpointAllocationV1::Automatic,
                    None,
                ),
            };
            let name = endpoint_name(&port.port_id);
            declared.push(EndpointV1 {
                name: name.clone(),
                protocol: "http".to_owned(),
                guest_port: Some(port.guest_port),
                allocation,
                preferred_port,
            });
            resolved.push(ResolvedEndpoint {
                name,
                guest_port: Some(port.guest_port),
                host_port,
            });
            endpoints.push(RealizedEndpoint {
                port_id: port.port_id.clone(),
                guest_port: port.guest_port,
                host_port,
            });
        }
        realization.endpoints = endpoints;

        // Authoring uses `.` for the root; the launch wire uses an empty
        // relative path. Reuse the same checked projection as build steps.
        let cwd_relative = ato_formation::projection::workspace_relative_cwd(&serve.cwd)
            .map_err(anyhow::Error::msg)?;
        let argv = serve.argv.clone();
        let mut public_env = request.plan.process_environment(request.derivation);
        let mut state_attachments = Vec::new();
        let mut resolved_state = Vec::new();
        if let Some(state) = state {
            (state_attachments, resolved_state) = state.project();
        } else if runtime_gate.is_some() {
            for slot in &request.derivation.state {
                if !ato_formation::proposal::isolated_state_id(&slot.id)
                    || !ato_formation::proposal::isolated_state_mount(&slot.mount)
                {
                    bail!("unsupported_state_requirement");
                }
                let access = match slot.access {
                    ato_formation::authoring::StateAccess::ReadOnly => StateAccessV1::ReadOnly,
                    ato_formation::authoring::StateAccess::ReadWrite => StateAccessV1::ReadWrite,
                };
                // Empty, attempt-owned working copy. No normal instance state
                // or production binding is ever resolved by exploration.
                let path = scratch.join("isolated-state").join(&slot.id);
                std::fs::create_dir_all(&path)?;
                state_attachments.push(StateAttachmentV1 {
                    state_key: slot.id.clone(),
                    revision_ref: None,
                    mount_target: slot.mount.clone(),
                    access,
                    writer_fence: None,
                });
                resolved_state.push(ResolvedStateAttachment::new(
                    slot.id.clone(),
                    None,
                    path,
                    slot.mount.clone(),
                    access,
                ));
            }
        } else if !request.derivation.state.is_empty() {
            bail!("exploration_state_grant_required");
        }

        let launch_timeout = control.map_or(Ok(LAUNCH_TIMEOUT), |c| {
            c.cap(AttemptPhase::Launch, LAUNCH_TIMEOUT)
        })?;
        let spec = RuntimeLaunchSpecV1 {
            protocol: RUNTIME_LAUNCH_SPEC_V1_PROTOCOL.to_owned(),
            context: state
                .map(|s| s.context.clone())
                .unwrap_or_else(|| LaunchContextV1 {
                    run_id: format!("formation-{}", request.attempt_id),
                    compute_id: "formation".to_owned(),
                    compute_schema_id: "formation".to_owned(),
                    compute_instance_id: format!("formation-{}", request.attempt_id),
                }),
            workspace: LaunchWorkspaceV1 {
                materialization_ref: format!("formation-attempt:{}", request.attempt_id),
                cwd_relative: cwd_relative.clone(),
            },
            realization: LaunchRealizationV1::Process(ProcessRealizationV1 {
                argv,
                executable: None,
            }),
            public_env: public_env
                .iter()
                .map(|(name, value)| PublicEnvV1 {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
            secret_grants: variables
                .iter()
                .filter(|v| v.phase == ato_formation::requirements::ExecutionPhase::Runtime)
                .map(|v| SecretGrantV1 {
                    name: v.value.name().to_owned(),
                    grant_ref: v.grant_ref.clone(),
                })
                .collect(),
            state_attachments,
            endpoints: declared,
            readiness: ReadinessV1::Process {
                timeout_ms: launch_timeout.as_millis().max(1) as u64,
            },
            lifecycle: LIFECYCLE,
        };
        // The preparation wrapper receives the current frozen allowance only
        // at spawn. It is not persisted in D or the logical launch spec, and
        // cannot inherit a previous attempt's relative budget.
        crate::control::apply_launch_deadline_environment(&mut public_env, control)?;
        let context = ResolvedRuntimeLaunchContext::new(
            workspace_root,
            &cwd_relative,
            public_env,
            variables
                .iter()
                .filter(|v| v.phase == ato_formation::requirements::ExecutionPhase::Runtime)
                .map(|v| {
                    crate::launch::resolved::ResolvedSecret::new(
                        v.value.name(),
                        v.value.expose_for_spawn(),
                    )
                })
                .collect(),
            resolved_state,
            resolved,
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;

        let host = ProcessLaunchHost {
            shim: request.shim.to_path_buf(),
            runtime_root,
            output: Some((realization.output.clone(), OUTPUT_LIMIT_BYTES)),
        };
        let ingress_root = host.runtime_root.join("ingress");
        check()?;
        let launched = match runtime_gate {
            Some(socket) => {
                std::fs::create_dir_all(&ingress_root)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        &ingress_root,
                        std::fs::Permissions::from_mode(0o700),
                    )?;
                    for endpoint in &realization.endpoints {
                        realization
                            .ingress
                            .push(crate::network_bridge::IngressBridge::start(
                                ingress_root.join(format!("{}.sock", endpoint.host_port)),
                                endpoint.host_port,
                            )?);
                    }
                }
                launch_process_with_scoped_network(&spec, &context, &host, socket, &ingress_root)?
            }
            None => launch_process_with(&spec, &context, &host)?,
        };
        realization.launched = Some(launched);

        // Readiness: every observed port accepts a connection. The Runtime's
        // probe and its "exited before ready" check, one endpoint at a time.
        let probe = LoopbackReadinessProbe::new(http_client()?);
        for endpoint in &spec.endpoints {
            let remaining = control.map_or(Ok(LAUNCH_TIMEOUT), |c| {
                c.cap(AttemptPhase::Launch, LAUNCH_TIMEOUT)
            })?;
            let mut per_endpoint = spec.clone();
            per_endpoint.readiness = ReadinessV1::Tcp {
                endpoint_name: endpoint.name.clone(),
                timeout_ms: remaining.as_millis().max(1) as u64,
            };
            let launched = realization
                .launched
                .as_mut()
                .expect("launched until destroyed");
            #[cfg(unix)]
            let scoped_probe = ScopedReadiness {
                bridges: &realization.ingress,
                endpoints: &realization.endpoints,
            };
            #[cfg(unix)]
            let selected_probe: &dyn ReadinessProbe = if runtime_gate.is_some() {
                &scoped_probe
            } else {
                &probe
            };
            #[cfg(not(unix))]
            let selected_probe: &dyn ReadinessProbe = &probe;
            let controlled_probe = ControlledReadiness {
                fallback: selected_probe,
                control,
                loopback: runtime_gate.is_none(),
            };
            if let Err(error) =
                wait_until_ready(&per_endpoint, &context, launched, &controlled_probe)
            {
                // The port explanation first: the output tail is long, and
                // a bounded report must not lose the actionable part.
                let mut detail = String::new();
                if let Some(moved) = realization
                    .endpoints
                    .iter()
                    .find(|endpoint| endpoint.host_port != endpoint.guest_port)
                {
                    detail.push_str(&format!(
                        "guest port {} was unavailable on this Runtime, so {} carries {} — a \
                         Derivation that binds {} literally cannot run here; ",
                        moved.guest_port,
                        endpoint_env_name(&moved.port_id),
                        moved.host_port,
                        moved.guest_port
                    ));
                }
                let exit = realization
                    .launched
                    .as_mut()
                    .and_then(|p| p.exited().ok().flatten());
                let tail = realization.output_tail();
                if runtime_gate.is_some() {
                    let control = scratch
                        .parent()
                        .context("attempt root missing")?
                        .join("control");
                    std::fs::create_dir_all(&control)?;
                    crate::execution_facts::append(
                        &control.join("execution-facts.jsonl"),
                        "launch",
                        "serve",
                        exit,
                        &tail,
                    )?;
                }
                detail.push_str(&format!("candidate output: {tail}"));
                return Err(error.context(detail));
            }
            if let Some(control) = control {
                control.remaining(AttemptPhase::Launch)?;
            }
        }
        Ok(realization)
    }

    /// Where each logical port is reachable.
    pub fn endpoints(&self) -> &[RealizedEndpoint] {
        &self.endpoints
    }

    /// GET every required path through the endpoint its logical port maps to.
    pub fn observe(&self, required: &[RequiredObservation]) -> Result<Vec<RuntimeHttpObservation>> {
        let client = http_client()?;
        let mut observed = Vec::with_capacity(required.len());
        for observation in required {
            let endpoint = self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.port_id == observation.port_id)
                .with_context(|| format!("port {} was not realized", observation.port_id))?;
            let url = format!(
                "http://127.0.0.1:{}{}",
                endpoint.host_port, observation.path
            );
            let response = client
                .get(&url)
                .send()
                .with_context(|| format!("GET {} failed", observation.path))?;
            let status = response.status().as_u16();
            let body = response.bytes().context("cannot read the response body")?;
            if body.len() > MAX_BODY_BYTES {
                bail!(
                    "GET {} returned more than {MAX_BODY_BYTES} bytes",
                    observation.path
                );
            }
            observed.push(RuntimeHttpObservation::from_response(
                observation.port_id.clone(),
                "GET",
                observation.path.clone(),
                status,
                &body,
            ));
        }
        Ok(observed)
    }

    /// Stop the candidate through the Runtime and discard everything it
    /// touched. The error is the Runtime's own: a process group that
    /// survived termination is reported, not assumed gone.
    pub fn destroy(mut self) -> Result<()> {
        self.teardown()
    }

    fn teardown(&mut self) -> Result<()> {
        let stopped = match self.launched.take() {
            Some(launched) => launched.stop(&LIFECYCLE).map(|_| ()),
            None => Ok(()),
        };
        #[cfg(unix)]
        self.ingress.clear();
        let removed = if self.scratch.exists() {
            remove_scratch(&self.scratch)
        } else {
            Ok(())
        };
        stopped.and(removed)
    }

    fn output_tail(&self) -> String {
        let tail = ato_adapter_process::read_output_tail(&self.output, OUTPUT_TAIL_BYTES);
        let tail = tail.trim();
        if tail.is_empty() {
            "(none)".to_owned()
        } else {
            crate::variables::redact(tail, &self.variables)
        }
    }
}

#[cfg(unix)]
struct ScopedReadiness<'a> {
    bridges: &'a [crate::network_bridge::IngressBridge],
    endpoints: &'a [RealizedEndpoint],
}

struct ControlledReadiness<'a> {
    fallback: &'a dyn ReadinessProbe,
    control: Option<&'a ExecutionControl>,
    loopback: bool,
}
impl ReadinessProbe for ControlledReadiness<'_> {
    fn probe(&self, host_port: u16, path: &str) -> std::result::Result<(), String> {
        let Some(control) = self.control else {
            return self.fallback.probe(host_port, path);
        };
        let budget = control
            .cap(AttemptPhase::Launch, REQUEST_TIMEOUT)
            .map_err(|e| e.to_string())?;
        let result = if self.loopback {
            if path.is_empty() {
                let address = std::net::SocketAddr::from(([127, 0, 0, 1], host_port));
                std::net::TcpStream::connect_timeout(
                    &address,
                    budget.min(Duration::from_millis(100)),
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            } else {
                let client = reqwest::blocking::Client::builder()
                    .timeout(budget)
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| e.to_string())?;
                LoopbackReadinessProbe::new(client).probe(host_port, path)
            }
        } else {
            self.fallback.probe(host_port, path)
        };
        control
            .remaining(AttemptPhase::Launch)
            .map_err(|e| e.to_string())?;
        result
    }
}
#[cfg(unix)]
impl ReadinessProbe for ScopedReadiness<'_> {
    fn probe(&self, host_port: u16, path: &str) -> std::result::Result<(), String> {
        if !path.is_empty() {
            return Err("scoped readiness requires a declared TCP endpoint".into());
        }
        self.endpoints
            .iter()
            .position(|e| e.host_port == host_port)
            .and_then(|index| self.bridges.get(index))
            .ok_or_else(|| "scoped readiness endpoint missing".to_string())?
            .ready()
            .map_err(|e| e.to_string())
    }
}

impl Drop for TemporaryRealization {
    fn drop(&mut self) {
        // Every path out of an attempt ends here if `destroy` was not reached:
        // an error, a timeout, a verifier failure, an unwinding panic.
        if let Err(error) = self.teardown() {
            eprintln!("[formation] temporary realization teardown failed: {error:#}");
        }
    }
}

/// The Runtime's endpoint name for a Contract port id. `app.http` becomes
/// `app_http`, exported as `ATO_ENDPOINT_APP_HTTP_PORT`.
fn endpoint_name(port_id: &str) -> String {
    port_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The environment variable a candidate reads for a port's host port.
pub fn endpoint_env_name(port_id: &str) -> String {
    endpoint_port_env_name(&endpoint_name(port_id))
}

/// The guest port itself, if nothing on this host holds it.
///
/// Probed on the wildcard address: a listener on `0.0.0.0` or `127.0.0.1`
/// both make the port unusable for a workload.
fn reserve_port(guest_port: u16) -> Option<u16> {
    // One at a time: a listener held across the second probe would make the
    // port look taken by this very check.
    if TcpListener::bind(("0.0.0.0", guest_port)).is_err() {
        return None;
    }
    if TcpListener::bind(("127.0.0.1", guest_port)).is_err() {
        return None;
    }
    Some(guest_port)
}

/// A free loopback port, chosen by the kernel.
///
/// Released before the candidate binds it: a process realization binds the
/// host port itself, so the window between release and bind is the price of
/// having no NAT. A collision there fails readiness rather than observing
/// somebody else, because the candidate's own bind is what fails.
fn allocate_host_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("cannot allocate a loopback port for the candidate")?;
    Ok(listener.local_addr()?.port())
}

/// [`allocate_host_port`], skipping ports another realization in this
/// worker chose and has not bound yet.
fn allocate_unclaimed_host_port(claim: &mut PendingClaim) -> Result<u16> {
    for _ in 0..64 {
        let port = allocate_host_port()?;
        if claim.claim(port) {
            return Ok(port);
        }
    }
    bail!("cannot allocate a loopback port no other candidate is about to bind")
}

fn http_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // A candidate has no business redirecting its Contract observation
        // somewhere else; what it answers directly is what is measured.
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Remove the realization scratch. The copy is plain files the candidate
/// could only read, but a build may have produced read-only directories.
fn remove_scratch(path: &Path) -> Result<()> {
    if std::fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    make_writable(path);
    std::fs::remove_dir_all(path)
        .with_context(|| format!("cannot remove realization scratch {}", path.display()))
}

fn make_writable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if metadata.is_symlink() {
                return;
            }
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o700);
            let _ = std::fs::set_permissions(path, permissions);
            if metadata.is_dir()
                && let Ok(entries) = std::fs::read_dir(path)
            {
                for entry in entries.flatten() {
                    make_writable(&entry.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_names_follow_the_runtime_abi() {
        assert_eq!(endpoint_name("app.http"), "app_http");
        assert_eq!(endpoint_env_name("app.http"), "ATO_ENDPOINT_APP_HTTP_PORT");
    }

    #[test]
    fn invalid_borrowed_state_is_refused_before_scratch_cleanup_ownership() {
        use crate::state_bindings::{VerificationStateAttachment, VerificationStateBindings};
        use ato_formation::authoring::{BindingContext, BoundState, StateAccess, bind};
        let root = tempfile::tempdir().expect("fixture root");
        let root = root.path().canonicalize().expect("canonical root");
        let workspace = root.join("workspace");
        let scratch = root.join("candidate");
        let state = scratch.join("must-preserve");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir_all(&state).expect("overlapping state");
        std::fs::write(state.join("sentinel"), "owned state").expect("state sentinel");
        let draft = ato_formation::capsule_toml::parse_capsule_toml(include_str!(
            "../../formation/tests/fixtures/proposal-python.toml"
        ))
        .expect("draft");
        let (_, mut derivation) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: &format!("sha256:{}", "a".repeat(64)),
            },
        )
        .expect("bind");
        derivation.state = vec![BoundState {
            id: "app.data".into(),
            protocol: ato_formation::authoring::STATE_FILESYSTEM_PROTOCOL.into(),
            mount: "/data".into(),
            access: StateAccess::ReadWrite,
        }];
        let plan = ExecutionPlan {
            lane: ato_formation::intent::Lane::PythonProcess,
            serving_step: 0,
            workspace_guest_root: "/app".into(),
            toolchains: Default::default(),
            package_manager: None,
            actions: vec![],
            toolchain_path: vec![],
            environment_bindings: Default::default(),
        };
        let request = TemporaryRealizationRequest {
            workspace: &workspace,
            scratch: &scratch,
            derivation: &derivation,
            plan: &plan,
            ports: &[],
            shim: &root.join("never-executed-shim"),
            attempt_id: "no-execution",
        };
        let context = LaunchContextV1 {
            run_id: "assigned-run".into(),
            compute_id: "assigned-compute".into(),
            compute_schema_id: "assigned-schema".into(),
            compute_instance_id: "assigned-instance".into(),
        };
        let logical = StateAttachmentV1 {
            state_key: "app_data".into(),
            revision_ref: None,
            mount_target: "/data".into(),
            access: StateAccessV1::ReadWrite,
            writer_fence: Some(1),
        };
        let resolved = ResolvedStateAttachment::new(
            "app_data",
            None,
            state.clone(),
            "/data",
            StateAccessV1::ReadWrite,
        );
        let bindings = VerificationStateBindings {
            context: &context,
            attachments: &[VerificationStateAttachment {
                slot_id: "app.data",
                declaration: &logical,
                resolved: &resolved,
            }],
        };
        let error = TemporaryRealization::launch_controlled_with_state(
            &request,
            Some(&root.join("unused-gate")),
            &[],
            Some(&bindings),
            None,
        )
        .err()
        .expect("rejected before launch");
        assert!(error.to_string().contains("cleanup_overlap"));
        assert_eq!(
            std::fs::read_to_string(state.join("sentinel")).expect("state retained"),
            "owned state"
        );
        let error = TemporaryRealization::launch_controlled_with_state(
            &request,
            None,
            &[],
            Some(&bindings),
            None,
        )
        .err()
        .expect("unscoped request refused");
        assert!(error.to_string().contains("scoped_grant_required"));
        assert!(state.join("sentinel").exists());
    }
}
