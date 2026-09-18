//! The Runner's `runtime_launch` lease handler.
//!
//! One Dynamic Compute Run, from a claimed lease to a terminal report:
//!
//! ```text
//! lease command
//!   -> parse RuntimeLaunchSpecV1 or V2 (an OCI service group), by protocol
//!   -> recompute the canonical digest and compare to the command's
//!   -> materialize the workspace from its content address
//!   -> allocate a real host port per endpoint
//!   -> redeem the state grant the control plane recorded
//!   -> restore the working copy from the granted revision
//!   -> ResolvedRuntimeLaunchContext
//!   -> sandboxed process
//!   -> readiness
//!   -> ACTIVE, until the control plane asks it to stop
//!   -> stop, pack, commit, release
//! ```
//!
//! There is no fallback for an unrecognized command. A Runner that guessed
//! would run a workload under a contract nobody agreed to.

use std::collections::{BTreeMap, BTreeSet};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_oci::{
    DockerOciAdapter, OciEndpoint, OciHandle, OciMount, OciResourceLimits, OciServiceGroup, OciSpec,
};
use ato_ipc::runtime_launch::{
    LaunchRealizationV1, ReadinessV1, RuntimeLaunchSpecV1, StateAccessV1,
};
use ato_ipc::runtime_launch_v2::{EndpointExposureV2, RuntimeLaunchSpec};

use super::process_executor::{ReadinessProbe, state_path_env_name, state_working_copy};
use super::resolved::{
    ResolvedRuntimeLaunchContext, ResolvedSecret, ResolvedStateAttachment, allocate_endpoint,
};
use super::session::{PreparedRun, RunStateOutcome, abort_run, commit_run};
use super::state_artifact::StateArtifactTransport;
use super::workspace::{WorkspaceTransport, materialize_workspace};

/// The lease kind this handler answers to.
///
/// The control plane selects a Runner by this exact string
/// (`selectRunnerForLeaseKind`), and the Runner advertises it. A contract test
/// pins the two together, because a silent mismatch does not fail loudly — it
/// looks like "no runner available" forever.
pub const RUNTIME_LAUNCH_LEASE_KIND: &str = "runtime_launch";
/// Absolute upper bound accepted from the control plane for one workload.
/// Public previews may request a shorter lease-owned deadline; omitting the
/// field preserves the ordinary one-hour safety cap.
pub const RUNTIME_LAUNCH_MAX_DURATION_SECS: u64 = 60 * 60;

/// Whether this Runner may take `runtime_launch` leases at all.
///
/// Containment is the condition. Advertising the kind on a host that cannot
/// contain a workload would make the Runner look available to the scheduler
/// and fail every Run it won — and the failure would arrive after a lease had
/// already been issued and a state slot taken.
#[allow(non_snake_case)]
pub fn RUNTIME_LAUNCH_SUPPORTED() -> bool {
    super::sandbox::containment_available()
}

/// What the control plane puts in `runner_leases.command_json`.
#[derive(Debug, serde::Deserialize)]
pub struct RuntimeLaunchLeaseCommand {
    pub run_id: String,
    pub compute_instance_id: String,
    /// The canonical digest of `launch_spec`, recomputed and compared here.
    pub launch_spec_digest: String,
    pub launch_spec: serde_json::Value,
    /// Present when the control plane attached a CPU entitlement. Ignored by
    /// this handler; declared so the envelope still parses.
    #[serde(default)]
    pub runtime_cpu_request: Option<serde_json::Value>,
    /// Runtime orchestration policy, outside the digested launch spec/K/D.
    /// Used by bounded public previews so cleanup does not depend on a browser
    /// request or a coarse control-plane cron sweep.
    #[serde(default)]
    pub max_duration_secs: Option<u64>,
}

/// Parse the command's spec and prove it is the one that was dispatched.
///
/// Recomputing rather than trusting matters because the spec is what decides
/// what runs: if the bytes that reached the Runner differ from the ones the
/// control plane digested onto the Run, the Runner would execute something the
/// receipt does not describe.
pub fn verified_spec(command: &RuntimeLaunchLeaseCommand) -> Result<RuntimeLaunchSpec> {
    if let Some(seconds) = command.max_duration_secs {
        ensure!(
            (1..=RUNTIME_LAUNCH_MAX_DURATION_SECS).contains(&seconds),
            "runtime launch max_duration_secs must be between 1 and {RUNTIME_LAUNCH_MAX_DURATION_SECS}"
        );
    }
    let encoded = serde_json::to_string(&command.launch_spec)
        .context("lease command launch_spec is not encodable")?;
    // Dispatch on `protocol` before interpreting the body: a version this
    // build does not know is refused, never read as the nearest known shape.
    let spec = RuntimeLaunchSpec::parse(&encoded)
        .map_err(|error| anyhow::anyhow!("lease command launch_spec is invalid: {error}"))?;
    let digest = spec
        .canonical_digest()
        .map_err(|error| anyhow::anyhow!("cannot digest the launch spec: {error}"))?;
    ensure!(
        digest == command.launch_spec_digest,
        "launch spec digest mismatch: lease says {}, spec digests to {digest}",
        command.launch_spec_digest
    );
    ensure!(
        spec.context().run_id == command.run_id
            && spec.context().compute_instance_id == command.compute_instance_id,
        "launch spec identity does not match its lease command"
    );
    verify_group_cpu_reservation(&spec, command.runtime_cpu_request.as_ref())?;
    Ok(spec)
}

/// A group reserves its summed CPU as ONE lease. When the control plane sent
/// the reservation, it must be exactly that sum; a mismatch means the lease
/// was sized for a different workload than the one about to run.
fn verify_group_cpu_reservation(
    spec: &RuntimeLaunchSpec,
    request: Option<&serde_json::Value>,
) -> Result<()> {
    let (RuntimeLaunchSpec::V2(group), Some(request)) = (spec, request) else {
        return Ok(());
    };
    let total = group.service_group().total_limits.cpu_limit_millis;
    let millis = |field: &str| request.get(field).and_then(serde_json::Value::as_u64);
    ensure!(
        millis("min_millis") == Some(total) && millis("max_millis") == Some(total),
        "the lease's CPU reservation does not equal the service group's total of {total} millis"
    );
    Ok(())
}

/// The one Endpoint the Runner's ingress slot publishes: a v1 spec's only
/// Endpoint, or a group's Surface Endpoint.
pub fn published_endpoint_name(spec: &RuntimeLaunchSpec) -> Result<String> {
    match spec {
        RuntimeLaunchSpec::V1(spec) => {
            ensure!(
                spec.endpoints.len() == 1,
                "runtime-launch ingress currently supports exactly one declared endpoint"
            );
            Ok(spec.endpoints[0].name.clone())
        }
        RuntimeLaunchSpec::V2(spec) => spec
            .service_group()
            .surface()
            .map(|(_, endpoint)| endpoint.name.clone())
            .context("service group declares no Surface Endpoint"),
    }
}

impl ActiveWorkload {
    /// The container, if any, whose forwarder serves the published Endpoint.
    pub fn surface_container(&self) -> Option<&OciHandle> {
        match self {
            ActiveWorkload::Process(_) => None,
            ActiveWorkload::Oci(container) => Some(container),
            ActiveWorkload::OciServiceGroup(group) => group
                .services()
                .map(|(_, container)| container)
                .find(|container| !container.port_mapping().1.is_empty()),
        }
    }
}

pub fn maximum_lifetime(command: &RuntimeLaunchLeaseCommand) -> Duration {
    Duration::from_secs(
        command
            .max_duration_secs
            .unwrap_or(RUNTIME_LAUNCH_MAX_DURATION_SECS),
    )
}

/// Bind an ephemeral port and keep it only long enough to learn its number.
///
/// A real bind rather than a guess: asking the OS for port 0 is the only way
/// to get a port nothing else holds. The listener is dropped immediately, so
/// there is a window in which another process could take it — accepted here
/// because the alternative, a fixed port, collides with certainty rather than
/// by chance.
fn allocate_host_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("failed to allocate a host port")?;
    let port = listener
        .local_addr()
        .context("allocated socket has no address")?
        .port();
    drop(listener);
    Ok(port)
}

/// Everything a launch needs, resolved on this Runner.
pub struct ResolvedRun {
    pub context: ResolvedRuntimeLaunchContext,
    pub prepared: PreparedRun,
    /// Endpoint name -> the host port actually allocated.
    pub endpoint_ports: BTreeMap<String, u16>,
}

/// Materialize the workspace and the state, and resolve the launch.
/// `assigned_ports` names the host port an endpoint MUST bind, by endpoint name.
///
/// The Runner still owns port selection — this is the Runner telling itself
/// which of its own ingress slots this endpoint is being published on, not the
/// control plane picking a port it cannot know is free. An endpoint with no
/// assignment keeps the ephemeral allocation below.
pub fn resolve_run(
    spec: &RuntimeLaunchSpec,
    lease_root: &Path,
    workspace: &dyn WorkspaceTransport,
    state: &dyn StateArtifactTransport,
    secrets: Vec<ResolvedSecret>,
    assigned_ports: &BTreeMap<String, u16>,
) -> Result<ResolvedRun> {
    let (launch_workspace, cwd_relative, public_env, declared_endpoints) = match spec {
        RuntimeLaunchSpec::V1(spec) => (
            &spec.workspace,
            spec.workspace.cwd_relative.as_str(),
            spec.public_env
                .iter()
                .map(|entry| (entry.name.clone(), entry.value.clone()))
                .collect(),
            spec.endpoints.clone(),
        ),
        // A group's environment is per service and applied at each container;
        // only its one Surface Endpoint is allocated a host port.
        RuntimeLaunchSpec::V2(spec) => (
            &spec.workspace,
            "",
            BTreeMap::new(),
            spec.service_group()
                .services
                .iter()
                .flat_map(|service| &service.endpoints)
                .filter(|endpoint| endpoint.exposure == EndpointExposureV2::Surface)
                .map(|endpoint| ato_ipc::runtime_launch::EndpointV1 {
                    name: endpoint.name.clone(),
                    protocol: endpoint.protocol.clone(),
                    guest_port: Some(endpoint.guest_port),
                    allocation: ato_ipc::runtime_launch::EndpointAllocationV1::Automatic,
                    preferred_port: None,
                })
                .collect(),
        ),
    };
    let workspace_root =
        materialize_workspace(workspace, &launch_workspace.materialization_ref, lease_root)?;

    let mut endpoint_ports = BTreeMap::new();
    let mut endpoints = Vec::new();
    for endpoint in &declared_endpoints {
        let port = match assigned_ports.get(&endpoint.name) {
            // A slot port is bound because that is where the ingress already
            // sends traffic. If it is taken, failing here is right: binding
            // somewhere else would produce a Run nothing can reach while
            // reporting a URL that says otherwise.
            Some(assigned) => *assigned,
            None => allocate_host_port()?,
        };
        endpoint_ports.insert(endpoint.name.clone(), port);
        endpoints.push(allocate_endpoint(endpoint, port));
    }

    let attachments = spec
        .state_attachments()
        .iter()
        .map(|attachment| {
            ResolvedStateAttachment::new(
                attachment.state_key.clone(),
                attachment.revision_ref.clone(),
                state_working_copy(&workspace_root, &attachment.state_key),
                attachment.mount_target.clone(),
                attachment.access,
            )
        })
        .collect::<Vec<_>>();

    let grants = spec.secret_grants();
    let expected_secret_names = grants
        .iter()
        .map(|grant| grant.name.as_str())
        .collect::<BTreeSet<_>>();
    let resolved_secret_names = secrets
        .iter()
        .map(ResolvedSecret::name)
        .collect::<BTreeSet<_>>();
    ensure!(
        expected_secret_names == resolved_secret_names
            && resolved_secret_names.len() == secrets.len(),
        "redeemed runtime Bindings do not match the launch spec"
    );

    let context = ResolvedRuntimeLaunchContext::new(
        workspace_root,
        cwd_relative,
        public_env,
        secrets,
        attachments,
        endpoints,
    )
    .map_err(|error| anyhow::anyhow!("cannot resolve the launch: {error}"))?;

    let prepared = super::session::prepare_run(spec.state_attachments(), &context, state)?;
    Ok(ResolvedRun {
        context,
        prepared,
        endpoint_ports,
    })
}

/// A Run that is up and serving.
pub enum ActiveWorkload {
    Process(super::process_executor::LaunchedProcess),
    Oci(OciHandle),
    OciServiceGroup(OciServiceGroup),
}

pub struct ActiveRun {
    pub launched: ActiveWorkload,
    pub resolved: ResolvedRun,
}

#[derive(Debug, serde::Serialize)]
pub struct RuntimeExecutionEvidence {
    pub realization: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_executable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Every container of an OCI service group, in start order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceExecutionEvidence>,
}

#[derive(Debug, serde::Serialize)]
pub struct ServiceExecutionEvidence {
    pub name: String,
    pub container_id: String,
    pub image: String,
}

impl ActiveRun {
    pub fn execution_subject(&self) -> String {
        match &self.launched {
            ActiveWorkload::Process(process) => format!("pid={}", process.pid()),
            ActiveWorkload::Oci(container) => {
                format!("container_id={}", container.container_id())
            }
            ActiveWorkload::OciServiceGroup(group) => group
                .services()
                .map(|(name, container)| format!("{name}={}", container.container_id()))
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    pub fn endpoint_port(&self, name: &str) -> Option<u16> {
        self.resolved.endpoint_ports.get(name).copied()
    }

    pub fn execution_evidence(&self) -> RuntimeExecutionEvidence {
        match &self.launched {
            ActiveWorkload::Process(process) => RuntimeExecutionEvidence {
                realization: "process",
                runtime_executable: Some(process.runtime_executable().to_owned()),
                runtime_version: process.runtime_version().map(str::to_owned),
                pid: Some(process.pid()),
                container_id: None,
                image: None,
                platform: None,
                services: Vec::new(),
            },
            ActiveWorkload::Oci(container) => RuntimeExecutionEvidence {
                realization: "oci",
                runtime_executable: Some("docker".to_owned()),
                runtime_version: None,
                pid: None,
                container_id: Some(container.container_id().to_owned()),
                image: Some(container.image().to_owned()),
                platform: Some(container.platform().to_owned()),
                services: Vec::new(),
            },
            ActiveWorkload::OciServiceGroup(group) => RuntimeExecutionEvidence {
                realization: "oci_service_group",
                runtime_executable: Some("docker".to_owned()),
                runtime_version: None,
                pid: None,
                container_id: None,
                image: None,
                platform: group
                    .services()
                    .next()
                    .map(|(_, container)| container.platform().to_owned()),
                services: group
                    .services()
                    .map(|(name, container)| ServiceExecutionEvidence {
                        name: name.to_owned(),
                        container_id: container.container_id().to_owned(),
                        image: container.image().to_owned(),
                    })
                    .collect(),
            },
        }
    }
}

/// Launch and wait for readiness. On failure, nothing is left holding a slot.
pub fn start(
    spec: &RuntimeLaunchSpec,
    resolved: ResolvedRun,
    state: &dyn StateArtifactTransport,
    probe: &dyn ReadinessProbe,
) -> Result<ActiveRun> {
    let spec = match spec {
        RuntimeLaunchSpec::V1(spec) => spec,
        RuntimeLaunchSpec::V2(spec) => {
            let group =
                match super::service_group::launch_service_group(spec, &resolved.context, probe) {
                    Ok(group) => group,
                    Err(error) => {
                        abort_run(state, &resolved.prepared);
                        return Err(error);
                    }
                };
            return Ok(ActiveRun {
                launched: ActiveWorkload::OciServiceGroup(group),
                resolved,
            });
        }
    };
    let launched = match &spec.realization {
        LaunchRealizationV1::Process(_) => {
            let mut launched =
                match super::process_executor::launch_process(spec, &resolved.context) {
                    Ok(launched) => launched,
                    Err(error) => {
                        abort_run(state, &resolved.prepared);
                        return Err(error);
                    }
                };
            if let Err(error) = super::process_executor::wait_until_ready(
                spec,
                &resolved.context,
                &mut launched,
                probe,
            ) {
                let _ = launched.stop(&spec.lifecycle);
                abort_run(state, &resolved.prepared);
                return Err(error);
            }
            ActiveWorkload::Process(launched)
        }
        LaunchRealizationV1::Oci(oci) => {
            let image = oci
                .image_reference
                .clone()
                .context("OCI execution requires a pullable image_reference")?;
            let platform = oci
                .platform
                .clone()
                .context("OCI execution requires a platform")?;
            let limits = oci
                .resource_limits
                .clone()
                .context("OCI execution requires resource_limits")?;
            let endpoints = resolved
                .context
                .endpoints()
                .iter()
                .map(|endpoint| {
                    Ok(OciEndpoint {
                        host_port: endpoint.host_port,
                        guest_port: endpoint.guest_port.context(
                            "OCI endpoint requires a guest port for container port mapping",
                        )?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let mounts = resolved
                .context
                .state_attachments()
                .iter()
                .map(|attachment| OciMount {
                    host_path: attachment.working_copy_for_mount().to_path_buf(),
                    guest_path: attachment.guest_target().to_owned(),
                    writable: attachment.access() == StateAccessV1::ReadWrite,
                })
                .collect();
            let mut environment = resolved.context.environment_for_spawn();
            for attachment in resolved.context.state_attachments() {
                environment.insert(
                    state_path_env_name(attachment.state_key()),
                    attachment.guest_target().to_owned(),
                );
            }
            let adapter = DockerOciAdapter::new(OciSpec {
                id: spec.context.run_id.clone(),
                image,
                platform,
                entrypoint: oci.entrypoint.clone(),
                argv: oci.argv.clone().unwrap_or_default(),
                working_dir: oci.working_dir.clone().unwrap_or_else(|| "/app".to_owned()),
                workspace_mount_path: oci
                    .workspace_mount_path
                    .clone()
                    .unwrap_or_else(|| "/app".to_owned()),
                environment,
                endpoints,
                mounts,
                limits: OciResourceLimits {
                    memory_bytes: limits.memory_bytes,
                    cpu_limit_millis: limits.cpu_limit_millis,
                    pids_limit: limits.pids_limit,
                },
                stop_timeout_seconds: spec.lifecycle.graceful_shutdown_ms.div_ceil(1000).max(1),
            })?;
            let runtime_root = resolved
                .context
                .workspace_root()
                .parent()
                .context("workspace has no lease root")?
                .join("oci-runtime");
            let mut launched = match adapter.spawn(resolved.context.workspace_root(), &runtime_root)
            {
                Ok(launched) => launched,
                Err(error) => {
                    abort_run(state, &resolved.prepared);
                    return Err(error);
                }
            };
            if let Err(error) = wait_until_oci_ready(spec, &resolved.context, &mut launched, probe)
            {
                let _ = launched.stop();
                abort_run(state, &resolved.prepared);
                return Err(error);
            }
            ActiveWorkload::Oci(launched)
        }
    };
    Ok(ActiveRun { launched, resolved })
}

fn wait_until_oci_ready(
    spec: &RuntimeLaunchSpecV1,
    context: &ResolvedRuntimeLaunchContext,
    launched: &mut OciHandle,
    probe: &dyn ReadinessProbe,
) -> Result<()> {
    let (timeout_ms, target) = match &spec.readiness {
        ReadinessV1::Http {
            endpoint_name,
            path,
            timeout_ms,
        } => (*timeout_ms, Some((endpoint_name, path.as_str()))),
        ReadinessV1::Tcp {
            endpoint_name,
            timeout_ms,
        } => (*timeout_ms, Some((endpoint_name, ""))),
        ReadinessV1::Process { timeout_ms } => (*timeout_ms, None),
    };
    let Some((endpoint_name, path)) = target else {
        return Ok(());
    };
    let host_port = context
        .endpoints()
        .iter()
        .find(|endpoint| endpoint.name == *endpoint_name)
        .map(|endpoint| endpoint.host_port)
        .with_context(|| format!("readiness names missing endpoint `{endpoint_name}`"))?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some(code) = launched.exit_code()? {
            bail!("OCI container exited before readiness with code {code}");
        }
        match probe.probe(host_port, path) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => {
                bail!("OCI container did not become ready within {timeout_ms}ms: {error}")
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Hold the Run ACTIVE until the control plane asks it to stop.
///
/// A Run does not end at readiness. Committing there would make the App a
/// batch job: it would come up, be told it was ready, and be torn down before
/// anyone could use it. The Run stays up, and the state it commits is the
/// state its users produced.
pub fn wait_for_stop(
    stop_requested: &dyn Fn() -> Result<bool>,
    poll: Duration,
    deadline: Option<Instant>,
) -> Result<()> {
    loop {
        if stop_requested()? {
            return Ok(());
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            bail!("the Run exceeded its maximum lifetime without a stop request");
        }
        std::thread::sleep(poll);
    }
}

/// Stop, pack and commit. The slot is released whatever happens.
pub fn finish(
    spec: &RuntimeLaunchSpec,
    active: ActiveRun,
    state: &dyn StateArtifactTransport,
    commit_request_id: &str,
) -> Result<Vec<RunStateOutcome>> {
    let ActiveRun { launched, resolved } = active;
    let stopped = match launched {
        ActiveWorkload::Process(process) => process.stop(spec.lifecycle()).map(|_| ()),
        ActiveWorkload::Oci(container) => container.stop(),
        // Reverse start order, then the network; packing waits for all of it.
        ActiveWorkload::OciServiceGroup(group) => group.stop(),
    };
    if let Err(error) = stopped {
        abort_run(state, &resolved.prepared);
        return Err(error);
    }
    commit_run(
        &resolved.context,
        state,
        &resolved.prepared,
        commit_request_id,
    )
}

/// Which attachments this Run is allowed to write back.
pub fn writable(spec: &RuntimeLaunchSpecV1) -> Vec<&str> {
    spec.state_attachments
        .iter()
        .filter(|attachment| attachment.access == StateAccessV1::ReadWrite)
        .map(|attachment| attachment.state_key.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROCESS_FIXTURE: &str = include_str!(
        "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/fastapi-process.json"
    );

    fn command_for(spec: &RuntimeLaunchSpecV1, digest: &str) -> RuntimeLaunchLeaseCommand {
        RuntimeLaunchLeaseCommand {
            run_id: spec.context.run_id.clone(),
            compute_instance_id: spec.context.compute_instance_id.clone(),
            launch_spec_digest: digest.to_owned(),
            launch_spec: serde_json::to_value(spec).expect("spec encodes"),
            runtime_cpu_request: None,
            max_duration_secs: None,
        }
    }

    #[test]
    fn the_lease_kind_matches_the_control_plane_string_exactly() {
        // The control plane selects a Runner with
        // `selectRunnerForLeaseKind(..., leaseKind: "runtime_launch")` and the
        // Runner advertises this constant. A mismatch does not fail loudly —
        // it looks like "no runner available", forever — so the literal is
        // pinned here rather than referenced.
        assert_eq!(RUNTIME_LAUNCH_LEASE_KIND, "runtime_launch");
    }

    #[test]
    fn a_spec_whose_digest_does_not_match_its_lease_is_refused() {
        let spec = RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("fixture");
        let wrong = format!("sha256:{}", "0".repeat(64));
        let error = verified_spec(&command_for(&spec, &wrong)).unwrap_err();
        // Executing it anyway would run something the Run's receipt does not
        // describe.
        assert!(error.to_string().contains("digest mismatch"), "{error}");
    }

    #[test]
    fn a_matching_spec_is_accepted() {
        let spec = RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("fixture");
        let digest = spec.canonical_digest().expect("digests");
        let verified = verified_spec(&command_for(&spec, &digest)).expect("accepted");
        assert_eq!(verified.context().run_id, spec.context.run_id);
    }

    #[test]
    fn a_bounded_preview_uses_its_shorter_runner_deadline() {
        let spec = RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("fixture");
        let digest = spec.canonical_digest().expect("digests");
        let mut command = command_for(&spec, &digest);
        command.max_duration_secs = Some(180);
        verified_spec(&command).expect("bounded command accepted");
        assert_eq!(maximum_lifetime(&command), Duration::from_secs(180));
    }

    #[test]
    fn an_unbounded_control_plane_override_is_refused() {
        let spec = RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("fixture");
        let digest = spec.canonical_digest().expect("digests");
        let mut command = command_for(&spec, &digest);
        command.max_duration_secs = Some(RUNTIME_LAUNCH_MAX_DURATION_SECS + 1);
        let error = verified_spec(&command).unwrap_err();
        assert!(error.to_string().contains("max_duration_secs"), "{error}");
    }

    #[test]
    fn a_spec_naming_a_different_run_than_its_lease_is_refused() {
        let spec = RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("fixture");
        let digest = spec.canonical_digest().expect("digests");
        let mut command = command_for(&spec, &digest);
        command.run_id = "run_someone_else".to_owned();
        let error = verified_spec(&command).unwrap_err();
        assert!(
            error.to_string().contains("does not match its lease"),
            "{error}"
        );
    }

    const GROUP_FIXTURE: &str = include_str!(
        "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v2/service-group.json"
    );

    fn group_command(cpu_request: Option<serde_json::Value>) -> RuntimeLaunchLeaseCommand {
        let spec =
            ato_ipc::runtime_launch_v2::RuntimeLaunchSpecV2::parse(GROUP_FIXTURE).expect("fixture");
        RuntimeLaunchLeaseCommand {
            run_id: spec.context.run_id.clone(),
            compute_instance_id: spec.context.compute_instance_id.clone(),
            launch_spec_digest: spec.canonical_digest().expect("digests"),
            launch_spec: serde_json::from_str(GROUP_FIXTURE).expect("json"),
            runtime_cpu_request: cpu_request,
            max_duration_secs: None,
        }
    }

    #[test]
    fn a_service_group_rides_the_same_lease_kind_and_publishes_its_surface() {
        let verified = verified_spec(&group_command(None)).expect("accepted");
        assert!(matches!(verified, RuntimeLaunchSpec::V2(_)));
        assert_eq!(published_endpoint_name(&verified).unwrap(), "app.http");
        assert_eq!(verified.secret_grants().len(), 1);
    }

    #[test]
    fn a_group_lease_must_reserve_exactly_the_summed_cpu() {
        let request = |min: u64, max: u64| {
            Some(serde_json::json!({
                "schema": "ato.runtime-cpu-request/v1",
                "class": "standard",
                "min_millis": min,
                "max_millis": max,
            }))
        };
        verified_spec(&group_command(request(1_000, 1_000))).expect("exact reservation");
        for (min, max) in [(1_000, 2_000), (500, 500), (2_000, 2_000)] {
            let error = verified_spec(&group_command(request(min, max))).unwrap_err();
            assert!(error.to_string().contains("CPU reservation"), "{error}");
        }
    }

    #[test]
    fn an_unknown_spec_protocol_is_refused_before_anything_runs() {
        let mut command = group_command(None);
        command.launch_spec["protocol"] = "ato.runtime-launch-spec.v3".into();
        let error = verified_spec(&command).unwrap_err();
        assert!(error.to_string().contains("UNSUPPORTED_VERSION"), "{error}");
    }

    #[test]
    fn an_unknown_lease_kind_has_no_handler_to_fall_back_to() {
        // Deserializing the envelope is the refusal: there is no catch-all
        // arm, so a command this Runner does not understand cannot be run
        // under a contract nobody agreed to.
        let unknown = serde_json::json!({ "kind": "some_future_kind", "run_id": "run_1" });
        assert!(serde_json::from_value::<crate::LeaseCommand>(unknown).is_err());
    }
}
