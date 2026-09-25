//! Realize an `ato.runtime-launch-spec.v2` OCI service group on this Runner.
//!
//! The group itself — one `--internal` network per Run, one container per
//! service in authored order, each after its own readiness, reverse-order
//! stop — is the Runtime's common OCI launch
//! (`ato_runtime_attempt::launch::oci`). What stays here is what this Runner
//! authorizes: the TCP egress each service was granted, as a Runner-owned
//! egress network and broker per grant, handed to the group to keep for
//! exactly its lifetime.

use anyhow::Result;
use ato_adapter_oci::{OciNetwork, OciOwner, OciServiceGroup};
use ato_ipc::runtime_launch_v2::RuntimeLaunchSpecV2;
use ato_runtime_attempt::launch::oci::{self as oci_launch, ServiceEgress};

use super::process_executor::ReadinessProbe;
use super::resolved::ResolvedRuntimeLaunchContext;
use super::{lease::NetworkAuthorization, network_broker::TcpEgressBroker};

/// Start every service and wait for each to become ready. On any failure the
/// services already started are stopped in reverse order and every network
/// is removed before the error is returned.
pub fn launch_service_group(
    spec: &RuntimeLaunchSpecV2,
    context: &ResolvedRuntimeLaunchContext,
    probe: &dyn ReadinessProbe,
    owner: &OciOwner,
    network_authorization: Option<&NetworkAuthorization>,
) -> Result<OciServiceGroup> {
    let mut egress = Vec::new();
    if let Some(authorization) = network_authorization {
        for grant in &authorization.egress {
            let network = OciNetwork::create_egress(
                &format!("{}-egress-{}", spec.context.run_id, grant.binding_id),
                &owner.labels(Some(&grant.service_id))?,
            )?;
            let broker = TcpEgressBroker::start(network.gateway_address()?, grant)?;
            egress.push(ServiceEgress {
                service: grant.service_id.clone(),
                environment_name: grant.environment.clone(),
                environment_value: broker.binding_value(),
                network,
                guard: Box::new(broker),
            });
        }
    }
    oci_launch::launch_service_group(spec, context, probe, owner, egress)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ato_ipc::runtime_launch::{EndpointAllocationV1, EndpointV1};
    use ato_ipc::runtime_launch_v2::RuntimeLaunchSpec;

    use ato_ipc::runtime_launch::StateAccessV1;
    use ato_runtime_attempt::launch::oci::service_oci_spec;

    use super::super::resolved::{ResolvedSecret, ResolvedStateAttachment, allocate_endpoint};
    use super::super::volume::VolumeStore;
    use super::*;

    const VOLUME_FIXTURE: &str = include_str!(
        "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v3/service-group-volume.json"
    );

    #[test]
    fn a_volume_is_mounted_into_its_one_service_and_no_other() {
        let parsed = RuntimeLaunchSpec::parse(VOLUME_FIXTURE).expect("v3 fixture");
        let spec = parsed.service_group_spec().expect("a group");
        let root = tempfile::tempdir().unwrap();
        let store = VolumeStore::open(root.path(), "runner-a").unwrap();
        let backing = parsed.runner_volume("data").unwrap();
        let volume_data = store.data_dir(&backing.volume_ref).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let context = ResolvedRuntimeLaunchContext::new(
            workspace.path().to_path_buf(),
            "",
            BTreeMap::new(),
            vec![ResolvedSecret::new("ATO_BINDING_ADMIN_SECRET", "s")],
            vec![ResolvedStateAttachment::new(
                "data",
                None,
                volume_data.clone(),
                "/data",
                StateAccessV1::ReadWrite,
            )],
            vec![allocate_endpoint(
                &EndpointV1 {
                    name: "app.http".to_owned(),
                    protocol: "http".to_owned(),
                    guest_port: Some(8080),
                    allocation: EndpointAllocationV1::Automatic,
                    preferred_port: None,
                },
                18080,
            )],
        )
        .expect("context");
        let owner = OciOwner {
            runner_id: "runner-a".to_owned(),
            slot_id: "slot-1".to_owned(),
            lease_id: "lease-1".to_owned(),
            run_id: spec.context.run_id.clone(),
            incarnation: "inc".to_owned(),
        };
        let group = spec.service_group();
        let spec_for = |name: &str| {
            let service = group.services.iter().find(|s| s.name == name).unwrap();
            service_oci_spec(spec, service, &context, &owner, &BTreeMap::new())
                .expect("service spec")
        };

        let backend = spec_for("backend");
        assert_eq!(backend.mounts.len(), 1);
        assert_eq!(backend.mounts[0].host_path, volume_data);
        assert_eq!(backend.mounts[0].guest_path, "/data");
        assert!(backend.mounts[0].writable);

        let web = spec_for("web");
        assert!(web.mounts.is_empty(), "the volume never reaches a sibling");
        assert!(
            !web.environment
                .keys()
                .any(|name| name.starts_with("ATO_STATE_PATH")),
            "nor does its path"
        );
    }
}
