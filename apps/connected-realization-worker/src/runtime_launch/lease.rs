//! The Runner's `runtime_launch` lease handler.
//!
//! One Dynamic Compute Run, from a claimed lease to a terminal report:
//!
//! ```text
//! lease command
//!   -> parse RuntimeLaunchSpecV1
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

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_ipc::runtime_launch::{RuntimeLaunchSpecV1, StateAccessV1};

use super::process_executor::{ReadinessProbe, state_working_copy};
use super::resolved::{ResolvedRuntimeLaunchContext, ResolvedStateAttachment, allocate_endpoint};
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
}

/// Parse the command's spec and prove it is the one that was dispatched.
///
/// Recomputing rather than trusting matters because the spec is what decides
/// what runs: if the bytes that reached the Runner differ from the ones the
/// control plane digested onto the Run, the Runner would execute something the
/// receipt does not describe.
pub fn verified_spec(command: &RuntimeLaunchLeaseCommand) -> Result<RuntimeLaunchSpecV1> {
    let encoded = serde_json::to_string(&command.launch_spec)
        .context("lease command launch_spec is not encodable")?;
    let spec = RuntimeLaunchSpecV1::parse(&encoded)
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
        spec.context.run_id == command.run_id
            && spec.context.compute_instance_id == command.compute_instance_id,
        "launch spec identity does not match its lease command"
    );
    Ok(spec)
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
    spec: &RuntimeLaunchSpecV1,
    lease_root: &Path,
    workspace: &dyn WorkspaceTransport,
    state: &dyn StateArtifactTransport,
    assigned_ports: &BTreeMap<String, u16>,
) -> Result<ResolvedRun> {
    let workspace_root =
        materialize_workspace(workspace, &spec.workspace.materialization_ref, lease_root)?;

    let mut endpoint_ports = BTreeMap::new();
    let mut endpoints = Vec::new();
    for endpoint in &spec.endpoints {
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
        .state_attachments
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

    let context = ResolvedRuntimeLaunchContext::new(
        workspace_root,
        &spec.workspace.cwd_relative,
        spec.public_env
            .iter()
            .map(|entry| (entry.name.clone(), entry.value.clone()))
            .collect(),
        // Secret grants are references. Redeeming them is a separate concern
        // and this handler holds none, so a spec that asks for one is refused
        // rather than launched without it.
        Vec::new(),
        attachments,
        endpoints,
    )
    .map_err(|error| anyhow::anyhow!("cannot resolve the launch: {error}"))?;

    ensure!(
        spec.secret_grants.is_empty(),
        "this Runner cannot redeem secret grants; refusing to launch a workload without the \
         secrets its spec requires"
    );

    let prepared = super::session::prepare_run(spec, &context, state)?;
    Ok(ResolvedRun {
        context,
        prepared,
        endpoint_ports,
    })
}

/// A Run that is up and serving.
pub struct ActiveRun {
    pub launched: super::process_executor::LaunchedProcess,
    pub resolved: ResolvedRun,
}

impl ActiveRun {
    pub fn pid(&self) -> u32 {
        self.launched.pid()
    }

    pub fn endpoint_port(&self, name: &str) -> Option<u16> {
        self.resolved.endpoint_ports.get(name).copied()
    }
}

/// Launch and wait for readiness. On failure, nothing is left holding a slot.
pub fn start(
    spec: &RuntimeLaunchSpecV1,
    resolved: ResolvedRun,
    state: &dyn StateArtifactTransport,
    probe: &dyn ReadinessProbe,
) -> Result<ActiveRun> {
    let mut launched = match super::process_executor::launch_process(spec, &resolved.context) {
        Ok(launched) => launched,
        Err(error) => {
            abort_run(state, &resolved.prepared);
            return Err(error);
        }
    };
    if let Err(error) =
        super::process_executor::wait_until_ready(spec, &resolved.context, &mut launched, probe)
    {
        let _ = launched.stop(&spec.lifecycle);
        abort_run(state, &resolved.prepared);
        return Err(error);
    }
    Ok(ActiveRun { launched, resolved })
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
    spec: &RuntimeLaunchSpecV1,
    active: ActiveRun,
    state: &dyn StateArtifactTransport,
    commit_request_id: &str,
) -> Result<Vec<RunStateOutcome>> {
    let ActiveRun { launched, resolved } = active;
    if let Err(error) = launched.stop(&spec.lifecycle) {
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
        assert_eq!(verified.context.run_id, spec.context.run_id);
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

    #[test]
    fn an_unknown_lease_kind_has_no_handler_to_fall_back_to() {
        // Deserializing the envelope is the refusal: there is no catch-all
        // arm, so a command this Runner does not understand cannot be run
        // under a contract nobody agreed to.
        let unknown = serde_json::json!({ "kind": "some_future_kind", "run_id": "run_1" });
        assert!(serde_json::from_value::<crate::LeaseCommand>(unknown).is_err());
    }
}
