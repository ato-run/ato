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
use ato_adapter_oci::{OciHandle, OciOwner, SpawnCleanupUnconfirmed, StopOutcome};
use ato_ipc::runtime_launch::{LaunchRealizationV1, RuntimeLaunchSpecV1, StateAccessV1};
use ato_ipc::runtime_launch_v2::{EndpointExposureV2, RuntimeLaunchSpec};
use ato_runtime_attempt::launch::oci::{self as oci_launch, LaunchedOci, OciStartFailure};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::process_executor::{ReadinessProbe, state_working_copy};
use super::recovery::{settle_lease, stop_budget};
use super::resolved::{
    ResolvedRuntimeLaunchContext, ResolvedSecret, ResolvedStateAttachment, allocate_endpoint,
};
use super::session::{
    PreparedRun, RunStateOutcome, VolumeAttachments, abort_run, commit_run, quarantine_run,
};
use super::state_artifact::StateArtifactTransport;
use super::volume::VolumeStore;
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
const EXECUTION_AUTHORIZATION_MAX_TTL_SECS: u64 = 60 * 60;
/// How many times the first confirmation may retry a transport failure before
/// the launch is abandoned. The workload does not exist yet, so refusing costs
/// nothing; proceeding would run it on authority nobody has confirmed.
const EXECUTION_AUTHORIZATION_FIRST_CONFIRM_ATTEMPTS: u32 = 3;
const EXECUTION_AUTHORIZATION_FIRST_CONFIRM_BACKOFF: Duration = Duration::from_secs(2);
const MAX_TCP_EGRESS_GRANTS: usize = 64;
const MAX_FIXED_TCP_ALLOCATIONS: usize = 8;

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
    /// Renewable Coordinator authority for execution on delegated or managed
    /// compute. Independent of an optional maximum-duration policy.
    #[serde(default)]
    pub execution_authorization: Option<ExecutionAuthorization>,
    /// Mutable Instance network grants and stable listener allocations. Like
    /// execution authorization, these never participate in the launch-spec
    /// digest or Capsule identity.
    #[serde(default)]
    pub network_authorization: Option<NetworkAuthorization>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkAuthorization {
    pub egress: Vec<TcpEgressGrant>,
    pub fixed_tcp: Vec<FixedTcpAllocation>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcpEgressGrant {
    pub grant_id: String,
    pub binding_id: String,
    pub environment: String,
    pub service_id: String,
    pub destination_cidr: String,
    pub ports: Vec<u16>,
    pub generation: u64,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedTcpAllocation {
    pub allocation_id: String,
    pub port_id: String,
    pub service_id: String,
    pub guest_port: u16,
    pub bind_ip: std::net::IpAddr,
    pub port: u16,
    pub generation: u64,
    /// How the Runner tells the service who connected to it.
    ///
    /// A fixed TCP listener terminates the client's connection and opens its
    /// own to the service, so by default the service sees the Runner. Any
    /// protocol whose policy depends on the peer — SMTP being the reason this
    /// exists — needs that address carried across the hop explicitly.
    #[serde(default)]
    pub client_address_transport: ClientAddressTransport,
}

/// The optional L4 metadata a fixed TCP listener prepends to a connection.
///
/// This is transport metadata on the Port, not a property of any application
/// protocol: the listener does not parse or even look at the bytes it carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAddressTransport {
    /// The service sees the Runner's address, as before.
    #[default]
    None,
    /// The Runner writes one PROXY protocol v2 header before any payload.
    ProxyProtocolV2,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionAuthorization {
    pub policy_generation: u64,
    pub authorization_generation: u64,
    pub server_time: String,
    pub expires_at: String,
    pub renew_after_secs: u64,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionAuthorizationRenewal {
    pub policy_generation: u64,
    pub authorization_generation: u64,
    pub server_time: String,
    pub expires_at: String,
    pub renew_after_secs: u64,
}

/// Monotonic projection of Coordinator-issued wall-clock authority.
///
/// The Runner never compares its wall clock with `expires_at`. It derives a
/// duration from two timestamps issued by the same Coordinator response and
/// applies that duration to `Instant`, so local clock changes cannot extend a
/// workload's authority.
#[derive(Debug, Clone)]
pub struct ExecutionAuthorizationMonitor {
    policy_generation: u64,
    authorization_generation: u64,
    deadline: Instant,
    renew_at: Instant,
    /// Whether the Coordinator has confirmed this authority since the command
    /// was received. A queued command carries a window the Runner only knows
    /// second-hand, so until the first renewal succeeds there is no authority
    /// to fall back on when a renewal fails.
    confirmed: bool,
}

impl ExecutionAuthorizationMonitor {
    pub fn new(authorization: &ExecutionAuthorization, now: Instant) -> Result<Self> {
        let (deadline, _) = authorization_window(
            &authorization.server_time,
            &authorization.expires_at,
            authorization.renew_after_secs,
            now,
        )?;
        ensure!(
            authorization.policy_generation > 0 && authorization.authorization_generation > 0,
            "execution authorization generations must be positive"
        );
        Ok(Self {
            policy_generation: authorization.policy_generation,
            authorization_generation: authorization.authorization_generation,
            deadline,
            // A queued command may have spent part or all of its wall-clock
            // authority waiting for a Runner slot. Revalidate with the
            // Coordinator before materialization instead of sliding that
            // stale window forward from receipt time.
            renew_at: now,
            confirmed: false,
        })
    }

    /// Whether the Coordinator has confirmed this authority at least once.
    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    pub fn apply(
        &mut self,
        renewal: &ExecutionAuthorizationRenewal,
        request_started: Instant,
        received_at: Instant,
    ) -> Result<()> {
        ensure!(
            request_started <= received_at,
            "execution authorization renewal timing is invalid"
        );
        ensure!(
            received_at < self.deadline,
            "execution authorization renewal arrived after the previous deadline"
        );
        ensure!(
            renewal.policy_generation == self.policy_generation,
            "execution authorization policy generation changed"
        );
        ensure!(
            renewal.authorization_generation > self.authorization_generation,
            "execution authorization generation did not advance"
        );
        let (deadline, renew_at) = authorization_window(
            &renewal.server_time,
            &renewal.expires_at,
            renewal.renew_after_secs,
            request_started,
        )?;
        self.authorization_generation = renewal.authorization_generation;
        self.deadline = deadline;
        self.renew_at = renew_at;
        self.confirmed = true;
        Ok(())
    }

    pub fn should_renew(&self, now: Instant) -> bool {
        now >= self.renew_at
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    /// Retry a transient transport failure without ever sliding the deadline.
    pub fn defer_retry(&mut self, now: Instant, delay: Duration) {
        self.renew_at = now
            .checked_add(delay)
            .unwrap_or(self.deadline)
            .min(self.deadline);
    }
}

fn authorization_window(
    server_time: &str,
    expires_at: &str,
    renew_after_secs: u64,
    now: Instant,
) -> Result<(Instant, Instant)> {
    let issued = OffsetDateTime::parse(server_time, &Rfc3339)
        .context("execution authorization server_time is not RFC 3339")?;
    let expires = OffsetDateTime::parse(expires_at, &Rfc3339)
        .context("execution authorization expires_at is not RFC 3339")?;
    let ttl = Duration::try_from(expires - issued)
        .context("execution authorization expiry precedes server_time")?;
    ensure!(
        !ttl.is_zero() && ttl <= Duration::from_secs(EXECUTION_AUTHORIZATION_MAX_TTL_SECS),
        "execution authorization TTL is outside the accepted range"
    );
    let renew_after = Duration::from_secs(renew_after_secs);
    ensure!(
        !renew_after.is_zero() && renew_after < ttl,
        "execution authorization renew_after_secs must precede expiry"
    );
    let deadline = now
        .checked_add(ttl)
        .context("execution authorization deadline overflowed")?;
    let renew_at = now
        .checked_add(renew_after)
        .context("execution authorization renewal time overflowed")?;
    Ok((deadline, renew_at))
}

/// What the Coordinator said when asked to renew an execution authorization.
#[derive(Debug)]
pub enum ExecutionAuthorizationRenewalOutcome {
    Renewed(ExecutionAuthorizationRenewal),
    Refused {
        reason: String,
        stop_requested: bool,
    },
}

/// Bring `monitor` up to date with the Coordinator, and say whether the owner
/// has stopped the Run.
///
/// The first confirmation and later renewals fail differently on purpose. A
/// later renewal that cannot reach the Coordinator still has a confirmed
/// window to live inside, so it retries within that window and the workload
/// keeps running until the window closes. The first confirmation has no such
/// window: the command may have waited in a queue, and the only authority the
/// Runner holds is what that queued command asserted about itself. Treating a
/// timeout there as "carry on" would start the workload on authority nobody
/// confirmed, so it retries a bounded number of times and then refuses.
pub fn refresh_execution_authorization(
    monitor: &mut ExecutionAuthorizationMonitor,
    lease_id: &str,
    mut clock: impl FnMut() -> Instant,
    mut renew: impl FnMut() -> Result<ExecutionAuthorizationRenewalOutcome>,
    mut sleep: impl FnMut(Duration),
) -> Result<bool> {
    let now = clock();
    ensure!(!monitor.is_expired(now), "execution authorization expired");
    if monitor.is_confirmed() && !monitor.should_renew(now) {
        return Ok(false);
    }
    let attempts = if monitor.is_confirmed() {
        1
    } else {
        EXECUTION_AUTHORIZATION_FIRST_CONFIRM_ATTEMPTS
    };
    let mut unconfirmed_error = None;
    for attempt in 0..attempts {
        let request_started = clock();
        match renew() {
            Ok(ExecutionAuthorizationRenewalOutcome::Renewed(renewal)) => {
                let received_at = clock();
                monitor.apply(&renewal, request_started, received_at)?;
                unconfirmed_error = None;
                break;
            }
            Ok(ExecutionAuthorizationRenewalOutcome::Refused {
                reason,
                stop_requested,
            }) => {
                if reason == "owner_stop" && stop_requested {
                    return Ok(true);
                }
                bail!(
                    "execution authorization renewal was refused: reason={reason} stop_requested={stop_requested}"
                )
            }
            Err(error) => {
                if monitor.is_confirmed() {
                    // A transport failure grants no time. Retry quickly within
                    // the existing monotonic deadline; once it passes, the next
                    // poll tears the workload down even if the local wall clock
                    // moved.
                    monitor.defer_retry(clock(), Duration::from_secs(5));
                    eprintln!(
                        "execution authorization renewal deferred lease_id={lease_id} error={error:#}"
                    );
                    break;
                }
                unconfirmed_error = Some(error);
                if attempt + 1 < attempts {
                    sleep(EXECUTION_AUTHORIZATION_FIRST_CONFIRM_BACKOFF);
                }
            }
        }
    }
    if let Some(error) = unconfirmed_error {
        return Err(error).context(format!(
            "the Coordinator never confirmed the execution authorization for lease {lease_id}"
        ));
    }
    ensure!(
        !monitor.is_expired(clock()),
        "execution authorization expired"
    );
    Ok(false)
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
    verify_network_authorization(&spec, command.network_authorization.as_ref())?;
    Ok(spec)
}

fn verify_network_authorization(
    spec: &RuntimeLaunchSpec,
    authorization: Option<&NetworkAuthorization>,
) -> Result<()> {
    let Some(authorization) = authorization else {
        return Ok(());
    };
    ensure!(
        !authorization.egress.is_empty() || !authorization.fixed_tcp.is_empty(),
        "empty network authorization must be omitted"
    );
    ensure!(
        authorization.egress.len() <= MAX_TCP_EGRESS_GRANTS
            && authorization.fixed_tcp.len() <= MAX_FIXED_TCP_ALLOCATIONS,
        "network authorization exceeds the bounded grant count"
    );
    let group = spec
        .service_group_spec()
        .context("network authorization requires an OCI service group")?
        .service_group();
    let mut grant_ids = BTreeSet::new();
    let mut binding_ids = BTreeSet::new();
    let mut environments = BTreeSet::new();
    for grant in &authorization.egress {
        ensure!(
            grant.grant_id.starts_with("egr_")
                && grant.grant_id.len() <= 200
                && !grant.binding_id.is_empty()
                && grant.binding_id.len() <= 128
                && grant.environment.starts_with("ATO_BINDING_")
                && grant.environment.len() <= 128
                && grant
                    .environment
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
                && group
                    .services
                    .iter()
                    .any(|service| service.name == grant.service_id)
                && grant.generation > 0
                && !grant.ports.is_empty()
                && grant.ports.len() <= 32
                && grant.ports.iter().all(|port| *port > 0)
                && grant_ids.insert(&grant.grant_id)
                && binding_ids.insert(&grant.binding_id)
                && environments.insert(&grant.environment),
            "TCP egress authorization is invalid"
        );
    }
    let mut allocation_ids = BTreeSet::new();
    let mut allocation_ports = BTreeSet::new();
    for allocation in &authorization.fixed_tcp {
        let endpoint_matches = group.services.iter().any(|service| {
            service.name == allocation.service_id
                && service.endpoints.iter().any(|endpoint| {
                    endpoint.name == allocation.port_id
                        && endpoint.guest_port == allocation.guest_port
                        && endpoint.exposure == EndpointExposureV2::Internal
                })
        });
        ensure!(
            allocation.allocation_id.starts_with("tcp_")
                && allocation.allocation_id.len() <= 200
                && allocation.generation > 0
                && endpoint_matches
                && allocation_ids.insert(&allocation.allocation_id)
                && allocation_ports.insert((&allocation.bind_ip, allocation.port)),
            "fixed TCP allocation is invalid"
        );
    }
    Ok(())
}

/// A group reserves its summed CPU as ONE lease. When the control plane sent
/// the reservation, it must be exactly that sum; a mismatch means the lease
/// was sized for a different workload than the one about to run.
fn verify_group_cpu_reservation(
    spec: &RuntimeLaunchSpec,
    request: Option<&serde_json::Value>,
) -> Result<()> {
    let (Some(group), Some(request)) = (spec.service_group_spec(), request) else {
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
        RuntimeLaunchSpec::V2(spec) | RuntimeLaunchSpec::V3 { view: spec, .. } => spec
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
            ActiveWorkload::Oci(launched) => launched
                .containers()
                .into_iter()
                .map(|(_, container)| container)
                .find(|container| !container.port_mapping().1.is_empty()),
        }
    }
}

pub fn maximum_lifetime(command: &RuntimeLaunchLeaseCommand) -> Option<Duration> {
    match (
        command.max_duration_secs,
        command.execution_authorization.as_ref(),
    ) {
        (Some(seconds), _) => Some(Duration::from_secs(seconds)),
        (None, Some(_)) => None,
        (None, None) => Some(Duration::from_secs(RUNTIME_LAUNCH_MAX_DURATION_SECS)),
    }
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
    volume_store: Option<&VolumeStore>,
) -> Result<ResolvedRun> {
    // Which keys live in a Runner-local volume. Decided before anything is
    // materialized: a Runner that cannot hold volumes refuses here rather
    // than restoring an empty working copy in the volume's place.
    let mut volume_attachments: VolumeAttachments<'_> = BTreeMap::new();
    for attachment in spec.state_attachments() {
        if let Some(backing) = spec.runner_volume(&attachment.state_key) {
            let store = volume_store.context(
                "this launch attaches a Runner-local volume, and this Runner has no volume store",
            )?;
            volume_attachments.insert(attachment.state_key.clone(), (store, backing));
        }
    }
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
        RuntimeLaunchSpec::V2(spec) | RuntimeLaunchSpec::V3 { view: spec, .. } => (
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
            let host_path = match volume_attachments.get(&attachment.state_key) {
                // The volume itself, mounted in place. Never a copy.
                Some((store, backing)) => store
                    .data_dir(&backing.volume_ref)
                    .map_err(anyhow::Error::new)?,
                None => state_working_copy(&workspace_root, &attachment.state_key),
            };
            Ok(ResolvedStateAttachment::new(
                attachment.state_key.clone(),
                attachment.revision_ref.clone(),
                host_path,
                attachment.mount_target.clone(),
                attachment.access,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

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

    let prepared = super::session::prepare_run(
        spec.state_attachments(),
        &context,
        state,
        &volume_attachments,
    )?;
    Ok(ResolvedRun {
        context,
        prepared,
        endpoint_ports,
    })
}

/// A Run that is up and serving. Both handles are the Runtime's common ones;
/// the lease, state and recovery around them stay here.
pub enum ActiveWorkload {
    Process(super::process_executor::LaunchedProcess),
    Oci(LaunchedOci),
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
            ActiveWorkload::Oci(LaunchedOci::Container(container)) => {
                format!("container_id={}", container.container_id())
            }
            ActiveWorkload::Oci(LaunchedOci::Group(group)) => group
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
            ActiveWorkload::Oci(LaunchedOci::Container(container)) => RuntimeExecutionEvidence {
                realization: "oci",
                runtime_executable: Some("docker".to_owned()),
                runtime_version: None,
                pid: None,
                container_id: Some(container.container_id().to_owned()),
                image: Some(container.image().to_owned()),
                platform: Some(container.platform().to_owned()),
                services: Vec::new(),
            },
            ActiveWorkload::Oci(LaunchedOci::Group(group)) => RuntimeExecutionEvidence {
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

/// How a failed OCI start left things: the labels' own sweep of this lease,
/// and — never overridden by it — what the start itself established about
/// what it had started. A cleanup the start could not confirm keeps the Run
/// quarantined even if a later sweep finds nothing.
fn settle_failed_oci_start(
    owner: &OciOwner,
    budget: ato_adapter_oci::StopBudget,
    error: &anyhow::Error,
) -> StopOutcome {
    let swept = settle_lease(owner, budget);
    let started = error
        .downcast_ref::<OciStartFailure>()
        .map(|failure| failure.cleanup.overall())
        .or_else(|| {
            error
                .downcast_ref::<SpawnCleanupUnconfirmed>()
                .map(|left| StopOutcome::Unconfirmed {
                    reason: left.to_string(),
                })
        });
    StopOutcome::worst(std::iter::once(&swept).chain(started.as_ref())).unwrap_or(swept)
}

/// After a start failed part-way: give the slots back only if every workload
/// the start may have created is confirmed stopped; otherwise quarantine them.
fn settle_failed_start(
    state: &dyn StateArtifactTransport,
    prepared: &PreparedRun,
    stop: &StopOutcome,
    error: anyhow::Error,
) -> StartFailure {
    if stop.is_confirmed() {
        abort_run(state, prepared);
    } else {
        // The start's error names what it left (container ids, networks).
        let reason = format!("start failed; stop unconfirmed: {stop:?}; {error:#}");
        quarantine_run(
            state,
            prepared,
            &reason.chars().take(1024).collect::<String>(),
        );
    }
    StartFailure {
        error,
        stop: stop.clone(),
        process: None,
    }
}

/// A start that failed, and whether everything it may have created is
/// confirmed stopped. Unconfirmed means the slots were quarantined.
#[derive(Debug)]
pub struct StartFailure {
    pub error: anyhow::Error,
    pub stop: StopOutcome,
    pub process: Option<super::recovery::ProcessIdentity>,
}

impl std::fmt::Display for StartFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#} (stop: {:?})", self.error, self.stop)
    }
}

impl std::error::Error for StartFailure {}

impl From<anyhow::Error> for StartFailure {
    /// A failure before any workload was created: nothing can be running.
    fn from(error: anyhow::Error) -> Self {
        StartFailure {
            error,
            stop: StopOutcome::AlreadyExited { exit_code: 0 },
            process: None,
        }
    }
}

/// Launch and wait for readiness. On failure, a slot is given back only when
/// nothing the start created can still be running; otherwise it stays held
/// and quarantined.
pub fn start(
    spec: &RuntimeLaunchSpec,
    resolved: ResolvedRun,
    state: &dyn StateArtifactTransport,
    probe: &dyn ReadinessProbe,
    owner: &OciOwner,
    network_authorization: Option<&NetworkAuthorization>,
    process_host: &super::process_executor::ProcessLaunchHost,
) -> std::result::Result<ActiveRun, StartFailure> {
    let budget = stop_budget(spec.lifecycle());
    let spec = match spec {
        RuntimeLaunchSpec::V1(spec) => spec,
        RuntimeLaunchSpec::V2(spec) | RuntimeLaunchSpec::V3 { view: spec, .. } => {
            let group = match super::service_group::launch_service_group(
                spec,
                &resolved.context,
                probe,
                owner,
                network_authorization,
            ) {
                Ok(group) => group,
                Err(error) => {
                    // The group stopped what it had started on its way out;
                    // the labels confirm nothing of this lease survived.
                    let stop = settle_failed_oci_start(owner, budget, &error);
                    return Err(settle_failed_start(state, &resolved.prepared, &stop, error));
                }
            };
            return Ok(ActiveRun {
                launched: ActiveWorkload::Oci(LaunchedOci::Group(group)),
                resolved,
            });
        }
    };
    let launched = match &spec.realization {
        LaunchRealizationV1::Process(_) => {
            let mut launched = match super::process_executor::launch_process_with(
                spec,
                &resolved.context,
                process_host,
            ) {
                Ok(launched) => launched,
                Err(error) => {
                    // `launch_process` fails before spawning or kills
                    // what it spawned; nothing is left running.
                    abort_run(state, &resolved.prepared);
                    return Err(error.into());
                }
            };
            let process_identity = super::recovery::ProcessIdentity {
                pid: launched.pid(),
                start_time: super::recovery::process_start_time(launched.pid()).unwrap_or(0),
            };
            if let Err(error) = super::process_executor::wait_until_ready(
                spec,
                &resolved.context,
                &mut launched,
                probe,
            ) {
                let stop = process_stop_outcome(launched.stop(&spec.lifecycle));
                let mut failure = settle_failed_start(state, &resolved.prepared, &stop, error);
                failure.process = Some(process_identity);
                return Err(failure);
            }
            ActiveWorkload::Process(launched)
        }
        LaunchRealizationV1::Oci(oci) => {
            let adapter =
                oci_launch::container_adapter(spec, oci, &resolved.context, owner.labels(None)?)?;
            let runtime_root = oci_launch::lease_runtime_root(&resolved.context)?;
            let mut launched = match adapter.spawn(resolved.context.workspace_root(), &runtime_root)
            {
                Ok(launched) => launched,
                Err(error) => {
                    // A spawn can fail after `docker run` created the
                    // container; only the labels can tell.
                    let stop = settle_failed_oci_start(owner, budget, &error);
                    return Err(settle_failed_start(state, &resolved.prepared, &stop, error));
                }
            };
            if let Err(error) = oci_launch::wait_until_container_ready(
                spec,
                &resolved.context,
                &mut launched,
                probe,
            ) {
                let stop = launched.stop_gracefully(budget);
                return Err(settle_failed_start(state, &resolved.prepared, &stop, error));
            }
            ActiveWorkload::Oci(LaunchedOci::Container(launched))
        }
    };
    Ok(ActiveRun { launched, resolved })
}

/// Hold the Run ACTIVE until the control plane asks it to stop.
///
/// A Run does not end at readiness. Committing there would make the App a
/// batch job: it would come up, be told it was ready, and be torn down before
/// anyone could use it. The Run stays up, and the state it commits is the
/// state its users produced.
pub fn wait_for_stop(
    stop_requested: &mut dyn FnMut() -> Result<bool>,
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

/// How a finished Run stopped, per workload, before anything was committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedStop {
    /// The least favourable outcome across the Run's workloads.
    pub overall: StopOutcome,
    /// Per service for a group, in stop order; one entry otherwise.
    pub services: Vec<(String, StopOutcome)>,
}

fn process_stop_outcome(stopped: Result<super::process_executor::StopOutcome>) -> StopOutcome {
    use super::process_executor::StopKind;
    match stopped {
        Ok(outcome) => {
            let exit_code = outcome
                .exit_status
                .and_then(|status| status.code())
                .unwrap_or(-1);
            match outcome.kind {
                StopKind::AlreadyExited => StopOutcome::AlreadyExited { exit_code },
                StopKind::Graceful => StopOutcome::Graceful { exit_code },
                StopKind::Forced => StopOutcome::Forced { exit_code },
            }
        }
        Err(error) => StopOutcome::Unconfirmed {
            reason: format!("{error:#}"),
        },
    }
}

/// Stop, then commit — or, if the stop cannot be confirmed, quarantine.
///
/// Packing and committing happen only after every workload is confirmed
/// stopped: a live writer would tear the state being packed. When the stop
/// is unconfirmed, nothing is committed or released; the slots stay held and
/// quarantined until recovery or an operator confirms the stop.
pub fn finish(
    spec: &RuntimeLaunchSpec,
    active: ActiveRun,
    state: &dyn StateArtifactTransport,
    commit_request_id: &str,
) -> (FinishedStop, Result<Vec<RunStateOutcome>>) {
    let ActiveRun { launched, resolved } = active;
    let budget = stop_budget(spec.lifecycle());
    let services = match launched {
        ActiveWorkload::Process(process) => vec![(
            "process".to_owned(),
            process_stop_outcome(process.stop(spec.lifecycle())),
        )],
        // Reverse start order for a group; its networks go only after every
        // service is confirmed stopped.
        ActiveWorkload::Oci(launched) => launched.stop(budget).services,
    };
    let stop = FinishedStop {
        overall: StopOutcome::worst(services.iter().map(|(_, outcome)| outcome))
            .unwrap_or(StopOutcome::AlreadyExited { exit_code: 0 }),
        services,
    };
    settle_stopped_run(stop, resolved, state, commit_request_id)
}

/// Only this Hosted state gate may commit/release after physical cessation.
pub(super) fn settle_stopped_run(
    stop: FinishedStop,
    resolved: ResolvedRun,
    state: &dyn StateArtifactTransport,
    commit_request_id: &str,
) -> (FinishedStop, Result<Vec<RunStateOutcome>>) {
    if !stop.overall.is_confirmed() {
        quarantine_run(
            state,
            &resolved.prepared,
            &format!("stop unconfirmed: {:?}", stop.overall),
        );
        let error = anyhow::anyhow!(
            "the Run's workload could not be confirmed stopped; its state was quarantined, not committed"
        );
        return (stop, Err(error));
    }
    let committed = commit_run(
        &resolved.context,
        state,
        &resolved.prepared,
        commit_request_id,
    );
    (stop, committed)
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
            execution_authorization: None,
            network_authorization: None,
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
        assert_eq!(maximum_lifetime(&command), Some(Duration::from_secs(180)));
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
            execution_authorization: None,
            network_authorization: None,
        }
    }

    fn execution_authorization() -> ExecutionAuthorization {
        ExecutionAuthorization {
            policy_generation: 7,
            authorization_generation: 1,
            server_time: "2026-09-19T00:00:00Z".to_owned(),
            expires_at: "2026-09-19T00:15:00Z".to_owned(),
            renew_after_secs: 180,
        }
    }

    #[test]
    fn renewable_authority_uses_monotonic_windows_and_advancing_generations() {
        let started = Instant::now();
        let mut monitor =
            ExecutionAuthorizationMonitor::new(&execution_authorization(), started).unwrap();
        assert!(monitor.should_renew(started));
        assert!(!monitor.is_expired(started + Duration::from_secs(899)));
        assert!(monitor.is_expired(started + Duration::from_secs(900)));

        let requested = started + Duration::from_secs(180);
        let received = requested + Duration::from_secs(3);
        monitor
            .apply(
                &ExecutionAuthorizationRenewal {
                    policy_generation: 7,
                    authorization_generation: 2,
                    server_time: "2038-01-01T00:00:00Z".to_owned(),
                    expires_at: "2038-01-01T00:15:00Z".to_owned(),
                    renew_after_secs: 180,
                },
                requested,
                received,
            )
            .unwrap();
        assert!(!monitor.is_expired(requested + Duration::from_secs(899)));
        assert!(monitor.is_expired(requested + Duration::from_secs(900)));
    }

    fn renewal(authorization_generation: u64) -> ExecutionAuthorizationRenewalOutcome {
        ExecutionAuthorizationRenewalOutcome::Renewed(ExecutionAuthorizationRenewal {
            policy_generation: 7,
            authorization_generation,
            server_time: "2026-09-19T00:03:00Z".to_owned(),
            expires_at: "2026-09-19T00:18:00Z".to_owned(),
            renew_after_secs: 180,
        })
    }

    /// The queued command's own window is not authority to start on. If the
    /// Coordinator cannot be reached for the first confirmation, the launch
    /// fails before anything is materialized.
    #[test]
    fn an_unconfirmed_authorization_refuses_the_launch_when_the_coordinator_is_unreachable() {
        let started = Instant::now();
        let mut monitor =
            ExecutionAuthorizationMonitor::new(&execution_authorization(), started).unwrap();
        let mut attempts = 0;
        let mut slept = Vec::new();
        let error = refresh_execution_authorization(
            &mut monitor,
            "lease_test",
            || started,
            || {
                attempts += 1;
                Err(anyhow::anyhow!("503 Service Unavailable"))
            },
            |delay| slept.push(delay),
        )
        .unwrap_err();

        assert_eq!(
            attempts, EXECUTION_AUTHORIZATION_FIRST_CONFIRM_ATTEMPTS,
            "the first confirmation must retry a bounded number of times"
        );
        assert_eq!(slept.len(), attempts as usize - 1);
        assert!(!monitor.is_confirmed());
        assert!(
            format!("{error:#}").contains("never confirmed"),
            "{error:#}"
        );
    }

    /// Once confirmed, the same transport failure is survivable: the workload
    /// keeps running inside the window the Coordinator already granted.
    #[test]
    fn a_confirmed_authorization_survives_a_transient_renewal_failure() {
        let started = Instant::now();
        let mut monitor =
            ExecutionAuthorizationMonitor::new(&execution_authorization(), started).unwrap();
        let mut generation = 1;
        refresh_execution_authorization(
            &mut monitor,
            "lease_test",
            || started,
            || {
                generation += 1;
                Ok(renewal(generation))
            },
            |_| panic!("a successful first confirmation must not sleep"),
        )
        .unwrap();
        assert!(monitor.is_confirmed());

        let later = started + Duration::from_secs(180);
        let mut attempts = 0;
        let stopped = refresh_execution_authorization(
            &mut monitor,
            "lease_test",
            || later,
            || {
                attempts += 1;
                Err(anyhow::anyhow!("503 Service Unavailable"))
            },
            |_| panic!("a confirmed renewal retries on the next poll, not by sleeping"),
        )
        .unwrap();
        assert!(!stopped);
        assert_eq!(attempts, 1);
    }

    /// An owner stop is a decision, not a failure: it is reported even on the
    /// very first confirmation, so a queued command that the owner cancelled
    /// while it waited is never materialized.
    #[test]
    fn an_owner_stop_during_the_first_confirmation_stops_instead_of_starting() {
        let started = Instant::now();
        let mut monitor =
            ExecutionAuthorizationMonitor::new(&execution_authorization(), started).unwrap();
        let stopped = refresh_execution_authorization(
            &mut monitor,
            "lease_test",
            || started,
            || {
                Ok(ExecutionAuthorizationRenewalOutcome::Refused {
                    reason: "owner_stop".to_owned(),
                    stop_requested: true,
                })
            },
            |_| panic!("a decided refusal must not be retried"),
        )
        .unwrap();
        assert!(stopped);
        assert!(!monitor.is_confirmed());
    }

    #[test]
    fn renewable_authority_rejects_sliding_or_wrong_policy_generations() {
        let started = Instant::now();
        let mut monitor =
            ExecutionAuthorizationMonitor::new(&execution_authorization(), started).unwrap();
        for (policy_generation, authorization_generation) in [(8, 2), (7, 1)] {
            let error = monitor
                .apply(
                    &ExecutionAuthorizationRenewal {
                        policy_generation,
                        authorization_generation,
                        server_time: "2026-09-19T00:03:00Z".to_owned(),
                        expires_at: "2026-09-19T00:18:00Z".to_owned(),
                        renew_after_secs: 180,
                    },
                    started + Duration::from_secs(180),
                    started + Duration::from_secs(181),
                )
                .unwrap_err();
            assert!(error.to_string().contains("generation"), "{error}");
        }
    }

    #[test]
    fn a_renewal_arriving_after_the_previous_deadline_cannot_revive_authority() {
        let started = Instant::now();
        let mut monitor =
            ExecutionAuthorizationMonitor::new(&execution_authorization(), started).unwrap();
        let error = monitor
            .apply(
                &ExecutionAuthorizationRenewal {
                    policy_generation: 7,
                    authorization_generation: 2,
                    server_time: "2026-09-19T00:03:00Z".to_owned(),
                    expires_at: "2026-09-19T00:18:00Z".to_owned(),
                    renew_after_secs: 180,
                },
                started + Duration::from_secs(899),
                started + Duration::from_secs(901),
            )
            .unwrap_err();
        assert!(error.to_string().contains("previous deadline"), "{error}");
    }

    #[test]
    fn renewable_authority_and_bounded_duration_are_independent_stop_conditions() {
        let spec = RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("fixture");
        let digest = spec.canonical_digest().expect("digests");
        let mut command = command_for(&spec, &digest);
        command.max_duration_secs = Some(60);
        command.execution_authorization = Some(execution_authorization());
        verified_spec(&command).expect("the two independent limits may coexist");
        assert_eq!(maximum_lifetime(&command), Some(Duration::from_secs(60)));
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
        command.launch_spec["protocol"] = "ato.runtime-launch-spec.v999".into();
        let error = verified_spec(&command).unwrap_err();
        assert!(error.to_string().contains("UNSUPPORTED_VERSION"), "{error}");
    }

    #[test]
    fn a_volume_launch_on_a_runner_without_volumes_touches_nothing() {
        const VOLUME_FIXTURE: &str = include_str!(
            "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v3/service-group-volume.json"
        );
        struct Untouched;
        impl WorkspaceTransport for Untouched {
            fn download(&self, _: &str) -> Result<Vec<u8>> {
                panic!("the workspace was fetched for a launch this Runner cannot hold")
            }
        }
        impl StateArtifactTransport for Untouched {
            fn acquire_writer(
                &self,
                _: &str,
            ) -> Result<super::super::state_artifact::StateWriterGrant> {
                panic!("a writer was taken for a launch this Runner cannot hold")
            }
            fn download(&self, _: &str) -> Result<Vec<u8>> {
                unreachable!()
            }
            fn commit(
                &self,
                _: &str,
                _: u64,
                _: Option<&str>,
                _: &str,
                _: &super::super::state_artifact::StateArtifact,
            ) -> Result<String> {
                unreachable!()
            }
            fn release_writer(&self, _: &str, _: u64) -> Result<()> {
                unreachable!()
            }
            fn quarantine_writer(&self, _: &str, _: u64, _: &str) -> Result<()> {
                unreachable!()
            }
        }
        let spec = RuntimeLaunchSpec::parse(VOLUME_FIXTURE).expect("v3 fixture");
        let lease_root = tempfile::tempdir().unwrap();
        let error = match resolve_run(
            &spec,
            lease_root.path(),
            &Untouched,
            &Untouched,
            Vec::new(),
            &BTreeMap::new(),
            None,
        ) {
            Ok(_) => panic!("a volume launch resolved without a volume store"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("no volume store"), "{error}");
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
