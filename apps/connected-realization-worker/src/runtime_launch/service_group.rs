//! Realize an `ato.runtime-launch-spec.v2` OCI service group.
//!
//! One `--internal` network per Run, one container per service, started in
//! authored order — each only after the previous one is ready — and stopped
//! in reverse. Only the Surface Endpoint is forwarded to the host; internal
//! Endpoints get no forwarder and no host listener, and are probed for
//! readiness directly on the Run network from the Runner, which is the trusted
//! execution substrate.
//!
//! Visibility is per service: a container receives only its own public
//! environment, its own secrets and its own state mounts.

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_oci::{
    DockerOciAdapter, OciEndpoint, OciMount, OciNetwork, OciResourceLimits, OciServiceGroup,
    OciSpec,
};
use ato_ipc::runtime_launch::StateAccessV1;
use ato_ipc::runtime_launch_v2::{
    EndpointExposureV2, OciServiceV2, RuntimeLaunchSpecV2, ServiceReadinessV2,
};

use super::process_executor::{ReadinessProbe, state_path_env_name};
use super::resolved::ResolvedRuntimeLaunchContext;

const INTERNAL_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Start every service and wait for each to become ready. On any failure the
/// services already started are stopped in reverse order and the network is
/// removed before the error is returned.
pub fn launch_service_group(
    spec: &RuntimeLaunchSpecV2,
    context: &ResolvedRuntimeLaunchContext,
    probe: &dyn ReadinessProbe,
) -> Result<OciServiceGroup> {
    ensure!(
        cfg!(target_os = "linux"),
        "OCI service groups require a native Linux Runner"
    );
    let group_spec = spec.service_group();
    let runtime_root = context
        .workspace_root()
        .parent()
        .context("workspace has no lease root")?
        .join("oci-runtime");
    let network = OciNetwork::create(&spec.context.run_id)?;
    // From here on, dropping `group` stops whatever started and removes the
    // network, so every early return below cleans up.
    let mut group = OciServiceGroup::new(network);
    for service in &group_spec.services {
        let adapter = DockerOciAdapter::new(service_oci_spec(spec, service, context)?)?;
        let handle = adapter.spawn_in_network(
            context.workspace_root(),
            &runtime_root.join(&service.name),
            group.network(),
            Some(&service.name),
        )?;
        group.push(service.name.clone(), handle);
        wait_until_service_ready(service, context, &group, probe)?;
    }
    Ok(group)
}

fn service_oci_spec(
    spec: &RuntimeLaunchSpecV2,
    service: &OciServiceV2,
    context: &ResolvedRuntimeLaunchContext,
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
        .collect::<std::collections::BTreeMap<_, _>>();
    environment.extend(context.secret_environment_for(&secret_names)?);
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
