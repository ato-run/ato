//! OCI execution the Runtime owns for its caller: one container, or a
//! service group started in order on its own internal network.
//!
//! ```text
//! caller                          here
//! ──────                          ────
//! lease, fence, slot              container spec a Hosted launch spec asks for
//! execution authorization         ordered group start, each service ready first
//! TCP egress grants → networks ─▶ networks and guards owned by the group
//! state acquire / commit          stop with a confirmed outcome per container
//! quarantine / recovery           an OCI candidate for the common attempt
//! ```
//!
//! How a candidate comes to be running and how it stops is here, for a
//! Hosted Run and a CLI Run alike. Which egress a service may reach is the
//! caller's: it creates those networks and their brokers and hands them over,
//! and the group keeps them exactly as long as its services. Nothing here
//! acquires, commits, releases or quarantines state; a caller does that only
//! after [`OciStop::overall`] is confirmed.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_oci::{
    DockerOciAdapter, OciEndpoint, OciHandle, OciMount, OciNetwork, OciOwner, OciResourceLimits,
    OciServiceGroup, OciSpec, StopBudget, StopOutcome,
};
use ato_ipc::runtime_launch::{OciRealizationV1, ReadinessV1, RuntimeLaunchSpecV1, StateAccessV1};
use ato_ipc::runtime_launch_v2::{
    EndpointExposureV2, OciServiceV2, RuntimeLaunchSpecV2, ServiceReadinessV2,
};

use super::process_executor::{ReadinessProbe, state_path_env_name};
use super::resolved::ResolvedRuntimeLaunchContext;
use crate::realize::RunningCandidate;

const INTERNAL_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// A running OCI workload.
pub enum LaunchedOci {
    Container(OciHandle),
    Group(OciServiceGroup),
}

/// How an OCI workload stopped, per container, in stop order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciStop {
    /// `container` for a single container; each service, in reverse start
    /// order, for a group.
    pub services: Vec<(String, StopOutcome)>,
}

impl OciStop {
    /// The least favourable outcome. Only a confirmed one lets a caller treat
    /// the workload's writes as finished.
    pub fn overall(&self) -> StopOutcome {
        StopOutcome::worst(self.services.iter().map(|(_, outcome)| outcome))
            .unwrap_or(StopOutcome::AlreadyExited { exit_code: 0 })
    }
}

impl LaunchedOci {
    /// Every container, by the name its stop outcome is reported under.
    pub fn containers(&self) -> Vec<(&str, &OciHandle)> {
        match self {
            Self::Container(handle) => vec![("container", handle)],
            Self::Group(group) => group.services().collect(),
        }
    }

    /// `Some(description)` once a container is no longer running. A group is
    /// one workload: any service exiting ends it.
    pub fn exited(&self) -> Result<Option<String>> {
        Ok(match self {
            Self::Container(handle) => handle
                .exit_code()?
                .map(|code| format!("the OCI container exited with code {code}")),
            Self::Group(group) => group
                .exited_service()?
                .map(|(name, code)| format!("OCI service `{name}` exited with code {code}")),
        })
    }

    /// Stop signal, grace, SIGKILL and confirmation for every container; a
    /// group stops in reverse start order and removes its networks only when
    /// every service is confirmed stopped.
    pub fn stop(self, budget: StopBudget) -> OciStop {
        OciStop {
            services: match self {
                Self::Container(handle) => {
                    vec![("container".to_owned(), handle.stop_gracefully(budget))]
                }
                Self::Group(group) => group.stop_gracefully(budget).services,
            },
        }
    }
}

// ── a Hosted single container ───────────────────────────────────────────────

/// The container a v1 launch spec's OCI realization asks for, in `context`,
/// carrying the caller's ownership `labels`.
pub fn container_adapter(
    spec: &RuntimeLaunchSpecV1,
    oci: &OciRealizationV1,
    context: &ResolvedRuntimeLaunchContext,
    labels: BTreeMap<String, String>,
) -> Result<DockerOciAdapter> {
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
    let endpoints = context
        .endpoints()
        .iter()
        .map(|endpoint| {
            Ok(OciEndpoint {
                host_port: endpoint.host_port,
                guest_port: endpoint
                    .guest_port
                    .context("OCI endpoint requires a guest port for container port mapping")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mounts = context
        .state_attachments()
        .iter()
        .map(|attachment| OciMount {
            host_path: attachment.working_copy_for_mount().to_path_buf(),
            guest_path: attachment.guest_target().to_owned(),
            writable: attachment.access() == StateAccessV1::ReadWrite,
        })
        .collect();
    let mut environment = context.environment_for_spawn();
    for attachment in context.state_attachments() {
        environment.insert(
            state_path_env_name(attachment.state_key()),
            attachment.guest_target().to_owned(),
        );
    }
    DockerOciAdapter::new(OciSpec {
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
        labels,
    })
}

/// Where a Hosted Run's container runtime scratch lives: beside its workspace.
pub fn lease_runtime_root(context: &ResolvedRuntimeLaunchContext) -> Result<PathBuf> {
    Ok(context
        .workspace_root()
        .parent()
        .context("workspace has no lease root")?
        .join("oci-runtime"))
}

/// Wait for the readiness a v1 launch spec declares, failing as soon as the
/// container exits.
pub fn wait_until_container_ready(
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

// ── a service group ─────────────────────────────────────────────────────────

/// One service to start in a group, prepared by the caller.
pub struct ServiceStart {
    pub name: String,
    pub adapter: DockerOciAdapter,
    /// This service's container runtime scratch.
    pub runtime_root: PathBuf,
    /// Networks the caller authorized for this service alone (a TCP egress
    /// bridge), connected once it starts and owned by the group from then.
    pub networks: Vec<OciNetwork>,
    /// What keeps those networks working (a broker), kept for exactly the
    /// group's lifetime and dropped after its services stop.
    pub guards: Vec<Box<dyn Send>>,
}

/// Start `services` in order on `network`, each only after `ready` accepts
/// it. On any failure every service already started is stopped in reverse
/// order and every network removed before the error is returned; services
/// not yet started are never started, and their networks and guards go too.
pub fn start_service_group(
    network: OciNetwork,
    workspace: &Path,
    services: Vec<ServiceStart>,
    ready: &mut dyn FnMut(&str, &OciServiceGroup) -> Result<()>,
) -> Result<OciServiceGroup> {
    // From here on, dropping `group` stops whatever started and removes the
    // networks, so every early return cleans up.
    let mut group = OciServiceGroup::new(network);
    for service in services {
        let handle = service.adapter.spawn_in_network(
            workspace,
            &service.runtime_root,
            group.network(),
            Some(&service.name),
        )?;
        let container = handle.container_id().to_owned();
        group.push(service.name.clone(), handle);
        for network in service.networks {
            network.connect_container(&container)?;
            group.push_auxiliary_network(network);
        }
        for guard in service.guards {
            group.retain_resource(guard);
        }
        ready(&service.name, &group)?;
    }
    Ok(group)
}

/// A TCP egress the caller authorized for one service of a v2 group: the
/// environment entry that reaches it, the network it is on, and what keeps
/// it working.
pub struct ServiceEgress {
    pub service: String,
    pub environment_name: String,
    pub environment_value: String,
    pub network: OciNetwork,
    pub guard: Box<dyn Send>,
}

/// Start an `ato.runtime-launch-spec.v2` OCI service group: one `--internal`
/// network per Run, one container per service in authored order, each after
/// its own readiness; only the Surface Endpoint is forwarded to the host.
/// Each service receives only its own public environment, secrets, state
/// mounts and the `egress` authorized for it.
pub fn launch_service_group(
    spec: &RuntimeLaunchSpecV2,
    context: &ResolvedRuntimeLaunchContext,
    probe: &dyn ReadinessProbe,
    owner: &OciOwner,
    egress: Vec<ServiceEgress>,
) -> Result<OciServiceGroup> {
    ensure!(
        cfg!(target_os = "linux"),
        "OCI service groups require a native Linux Runner"
    );
    let group_spec = spec.service_group();
    let names = group_spec
        .services
        .iter()
        .map(|service| service.name.as_str())
        .collect::<BTreeSet<_>>();
    ensure!(
        egress
            .iter()
            .all(|grant| names.contains(grant.service.as_str())),
        "TCP egress grant names an absent service"
    );
    let runtime_root = lease_runtime_root(context)?;
    let mut egress = egress;
    let mut starts = Vec::with_capacity(group_spec.services.len());
    for service in &group_spec.services {
        let (mine, others): (Vec<_>, Vec<_>) = egress
            .into_iter()
            .partition(|grant| grant.service == service.name);
        egress = others;
        let network_environment = mine
            .iter()
            .map(|grant| {
                (
                    grant.environment_name.clone(),
                    grant.environment_value.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let adapter = DockerOciAdapter::new(service_oci_spec(
            spec,
            service,
            context,
            owner,
            &network_environment,
        )?)?;
        let (networks, guards) = mine
            .into_iter()
            .map(|grant| (grant.network, grant.guard))
            .unzip();
        starts.push(ServiceStart {
            name: service.name.clone(),
            adapter,
            runtime_root: runtime_root.join(&service.name),
            networks,
            guards,
        });
    }
    let network = OciNetwork::create(&spec.context.run_id, &owner.labels(None)?)?;
    start_service_group(
        network,
        context.workspace_root(),
        starts,
        &mut |name, group| {
            let service = group_spec
                .services
                .iter()
                .find(|service| service.name == name)
                .context("service was not declared")?;
            wait_until_service_ready(service, context, group, probe)
        },
    )
}

/// The container one service of a v2 group asks for.
pub fn service_oci_spec(
    spec: &RuntimeLaunchSpecV2,
    service: &OciServiceV2,
    context: &ResolvedRuntimeLaunchContext,
    owner: &OciOwner,
    network_environment: &BTreeMap<String, String>,
) -> Result<OciSpec> {
    let group = spec.service_group();
    let endpoints = service
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.exposure == EndpointExposureV2::Surface)
        .map(|endpoint| {
            let resolved = context
                .endpoints()
                .iter()
                .find(|resolved| resolved.name == endpoint.name)
                .with_context(|| {
                    format!("Surface Endpoint `{}` was not allocated", endpoint.name)
                })?;
            Ok(OciEndpoint {
                host_port: resolved.host_port,
                guest_port: endpoint.guest_port,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let state_keys = service
        .state_keys
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let attachments = context
        .state_attachments()
        .iter()
        .filter(|attachment| state_keys.contains(attachment.state_key()))
        .collect::<Vec<_>>();
    ensure!(
        attachments.len() == state_keys.len(),
        "service `{}` names state that this Run did not prepare",
        service.name
    );
    let secret_names = service
        .secret_grants
        .iter()
        .map(|grant| grant.name.as_str())
        .collect::<BTreeSet<_>>();

    let mut environment = service
        .public_env
        .iter()
        .map(|entry| (entry.name.clone(), entry.value.clone()))
        .collect::<BTreeMap<_, _>>();
    environment.extend(context.secret_environment_for(&secret_names)?);
    for (name, value) in network_environment {
        ensure!(
            environment.insert(name.clone(), value.clone()).is_none(),
            "network Binding `{name}` conflicts with another environment value"
        );
    }
    for attachment in &attachments {
        environment.insert(
            state_path_env_name(attachment.state_key()),
            attachment.guest_target().to_owned(),
        );
    }

    Ok(OciSpec {
        id: format!("{}-{}", spec.context.run_id, service.name),
        image: service.image_reference.clone(),
        platform: group.platform.clone(),
        entrypoint: service.entrypoint.clone(),
        argv: service.argv.clone(),
        working_dir: service.working_dir.clone(),
        workspace_mount_path: service.workspace_mount_path.clone(),
        environment,
        endpoints,
        mounts: attachments
            .iter()
            .map(|attachment| OciMount {
                host_path: attachment.working_copy_for_mount().to_path_buf(),
                guest_path: attachment.guest_target().to_owned(),
                writable: attachment.access() == StateAccessV1::ReadWrite,
            })
            .collect(),
        limits: OciResourceLimits {
            memory_bytes: service.resource_limits.memory_bytes,
            cpu_limit_millis: service.resource_limits.cpu_limit_millis,
            pids_limit: service.resource_limits.pids_limit,
        },
        stop_timeout_seconds: spec.lifecycle.graceful_shutdown_ms.div_ceil(1000).max(1),
        labels: owner.labels(Some(&service.name))?,
    })
}

fn wait_until_service_ready(
    service: &OciServiceV2,
    context: &ResolvedRuntimeLaunchContext,
    group: &OciServiceGroup,
    probe: &dyn ReadinessProbe,
) -> Result<()> {
    let endpoint = service
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == service.readiness.endpoint_name())
        .context("readiness names an Endpoint the service does not serve")?;
    let (_, handle) = group
        .services()
        .find(|(name, _)| *name == service.name)
        .context("service was not started")?;
    let timeout_ms = service.readiness.timeout_ms();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        // A sibling that died while this one was starting fails the group as
        // surely as this service dying does.
        if let Some((name, code)) = group.exited_service()? {
            bail!("OCI service `{name}` exited before the group was ready with code {code}");
        }
        let outcome = match endpoint.exposure {
            EndpointExposureV2::Surface => {
                let host_port = context
                    .endpoints()
                    .iter()
                    .find(|resolved| resolved.name == endpoint.name)
                    .map(|resolved| resolved.host_port)
                    .context("Surface Endpoint was not allocated")?;
                let path = match &service.readiness {
                    ServiceReadinessV2::Http { path, .. } => path.as_str(),
                    ServiceReadinessV2::Tcp { .. } => "",
                };
                probe.probe(host_port, path)
            }
            EndpointExposureV2::Internal => {
                let target = SocketAddr::new(handle.container_address(), endpoint.guest_port);
                TcpStream::connect_timeout(&target, INTERNAL_PROBE_TIMEOUT)
                    .map(drop)
                    .map_err(|error| error.to_string())
            }
        };
        match outcome {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => bail!(
                "OCI service `{}` did not become ready within {timeout_ms}ms: {error}",
                service.name
            ),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

// ── an OCI candidate for the common attempt ────────────────────────────────

/// A running OCI workload as a [`RunningCandidate`], with the runtime scratch
/// its realization owns. It is stopped with `budget`; its scratch is removed
/// only once every container is confirmed stopped, because an unconfirmed
/// container may still be writing through its mounts.
pub struct OciCandidate {
    launched: Option<LaunchedOci>,
    endpoints: BTreeMap<String, String>,
    owned: Vec<PathBuf>,
    budget: StopBudget,
}

impl OciCandidate {
    pub fn new(
        launched: LaunchedOci,
        endpoints: BTreeMap<String, String>,
        owned: Vec<PathBuf>,
        budget: StopBudget,
    ) -> Self {
        Self {
            launched: Some(launched),
            endpoints,
            owned,
            budget,
        }
    }

    /// The running workload, until it is stopped.
    pub fn launched(&self) -> Option<&LaunchedOci> {
        self.launched.as_ref()
    }

    fn stop_owned(&mut self) -> Result<()> {
        let Some(launched) = self.launched.take() else {
            return Ok(());
        };
        let stop = launched.stop(self.budget);
        if !stop.overall().is_confirmed() {
            bail!(
                "the OCI workload could not be confirmed stopped ({:?}); its runtime scratch \
                 is kept",
                stop.services
            );
        }
        remove_owned(&std::mem::take(&mut self.owned))
    }
}

impl RunningCandidate for OciCandidate {
    fn endpoints(&self) -> &BTreeMap<String, String> {
        &self.endpoints
    }

    fn exited(&mut self) -> Result<Option<String>> {
        match &self.launched {
            Some(launched) => launched.exited(),
            None => Ok(Some("stopped".to_owned())),
        }
    }

    fn stop(mut self: Box<Self>) -> Result<()> {
        self.stop_owned()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for OciCandidate {
    fn drop(&mut self) {
        if let Err(error) = self.stop_owned() {
            eprintln!("[runtime] {error:#}");
        }
    }
}

/// Remove what a realization unpacked or created for itself.
fn remove_owned(paths: &[PathBuf]) -> Result<()> {
    let mut failed = Vec::new();
    for path in paths {
        if path.exists()
            && let Err(error) = std::fs::remove_dir_all(path)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_is_confirmed_only_when_every_container_is() {
        let stop = OciStop {
            services: vec![
                ("web".to_owned(), StopOutcome::Graceful { exit_code: 0 }),
                ("backend".to_owned(), StopOutcome::Forced { exit_code: 137 }),
            ],
        };
        assert_eq!(stop.overall(), StopOutcome::Forced { exit_code: 137 });
        assert!(stop.overall().is_confirmed());
        let stop = OciStop {
            services: vec![
                ("web".to_owned(), StopOutcome::Graceful { exit_code: 0 }),
                (
                    "backend".to_owned(),
                    StopOutcome::Unconfirmed {
                        reason: "docker did not answer".to_owned(),
                    },
                ),
            ],
        };
        assert!(!stop.overall().is_confirmed());
        assert!(OciStop { services: vec![] }.overall().is_confirmed());
    }
}
