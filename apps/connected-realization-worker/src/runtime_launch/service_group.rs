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
use ato_adapter_oci::{OciNetwork, OciOwner};
use ato_ipc::runtime_launch_v2::RuntimeLaunchSpecV2;
use ato_runtime_attempt::launch::oci::{self as oci_launch, LaunchedServiceGroup, ServiceEgress};

use super::managed_embedding_relay::{self, ManagedEmbeddingRelay};
use super::oidc_issuer_relay::{self, OidcIssuerGrant, OidcIssuerRelay};
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
) -> Result<LaunchedServiceGroup> {
    // Validate every scoped authority and redeemed pair before creating any resource.
    let managed_credentials = network_authorization
        .map(|authorization| {
            managed_embedding_relay::validate_group_grants(spec, authorization)?;
            authorization
                .managed_embeddings
                .iter()
                .map(|grant| ManagedEmbeddingRelay::credentials(grant, context))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    if let Some(authorization) = network_authorization {
        oidc_issuer_relay::validate_group_grants(spec, authorization)?;
    }
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
                expected_secret_value: None,
            });
        }
    }
    if let Some(authorization) = network_authorization {
        for (grant, (token, original_base)) in authorization
            .managed_embeddings
            .iter()
            .zip(managed_credentials)
        {
            let network = OciNetwork::create_egress(
                &format!("{}-managed-{}", spec.context.run_id, grant.service_id),
                &owner.labels(Some(&grant.service_id))?,
            )?;
            let relay = ManagedEmbeddingRelay::start(network.gateway_address()?, grant, token)?;
            egress.push(ServiceEgress {
                service: grant.service_id.clone(),
                environment_name: "OPENAI_BASE_URL".to_owned(),
                environment_value: relay.binding_value(),
                expected_secret_value: Some(original_base),
                network,
                guard: Box::new(relay),
            });
        }
    }
    if let Some(authorization) = network_authorization {
        for grant in &authorization.oidc_issuers {
            let network = OciNetwork::create_egress(
                &format!("{}-oidc-{}", spec.context.run_id, grant.service_id),
                &owner.labels(Some(&grant.service_id))?,
            )?;
            let relay = OidcIssuerRelay::start(network.gateway_address()?, grant)?;
            let (environment_name, environment_value) = oidc_issuer_environment(grant, &relay);
            egress.push(ServiceEgress {
                service: grant.service_id.clone(),
                environment_name,
                environment_value,
                expected_secret_value: None,
                network,
                guard: Box::new(relay),
            });
        }
    }
    oci_launch::launch_service_group(
        spec,
        context,
        probe,
        owner,
        egress,
        super::recovery::stop_budget(&spec.lifecycle),
    )
}

/// The environment entry one OIDC issuer grant projects into its service: the
/// name the Coordinator authorized and the relay's back-channel issuer base.
fn oidc_issuer_environment(grant: &OidcIssuerGrant, relay: &OidcIssuerRelay) -> (String, String) {
    (grant.environment.clone(), relay.binding_value())
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
    const GROUP_FIXTURE: &str = include_str!(
        "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v2/service-group.json"
    );

    /// The v2 fixture's Surface service alone. A launch spec admits a group of
    /// one so a single-container app that declares a network Binding rides this
    /// same group launch and gets the same Runner-owned relays.
    fn one_service_group() -> RuntimeLaunchSpecV2 {
        let mut raw: serde_json::Value = serde_json::from_str(GROUP_FIXTURE).unwrap();
        let services = raw["realization"]["services"].as_array_mut().unwrap();
        services.retain(|service| service["name"] == "web");
        let limits = services[0]["resource_limits"].clone();
        raw["realization"]["total_limits"] = limits;
        raw["state_attachments"] = serde_json::json!([]);
        RuntimeLaunchSpec::parse(&serde_json::to_string(&raw).unwrap())
            .expect("a one-service group is a valid launch spec")
            .service_group_spec()
            .expect("a group")
            .clone()
    }

    /// The one service of that group receives its authorized issuer environment
    /// and the launch starts the relay that value points at.
    #[test]
    fn a_one_service_group_with_an_oidc_issuer_grant_gets_its_environment_and_relay() {
        let spec = one_service_group();
        let group = spec.service_group();
        assert_eq!(group.services.len(), 1);
        let grant = oidc_issuer_relay::OidcIssuerGrant {
            service_id: "web".to_owned(),
            binding_id: "oidc_issuer".to_owned(),
            environment: "ATO_BINDING_OIDC_ISSUER".to_owned(),
            issuer: "https://issuer.example/v1/selfhost/oidc".to_owned(),
        };
        let authorization = NetworkAuthorization {
            egress: Vec::new(),
            fixed_tcp: Vec::new(),
            managed_embeddings: Vec::new(),
            oidc_issuers: vec![grant.clone()],
        };
        // The group guard admits a grant naming the only service.
        oidc_issuer_relay::validate_group_grants(&spec, &authorization)
            .expect("a one-service group authorizes its issuer relay");
        // What the launch starts for that grant: loopback here, an ephemeral
        // port instead of the fixed egress-bridge port.
        let relay = OidcIssuerRelay::start_at("127.0.0.1:0".parse().unwrap(), &grant)
            .expect("the issuer relay starts");
        let (environment_name, environment_value) = oidc_issuer_environment(&grant, &relay);
        assert_eq!(environment_name, "ATO_BINDING_OIDC_ISSUER");
        assert!(
            environment_value.starts_with("http://127.0.0.1:"),
            "{environment_value}"
        );
        assert!(
            environment_value.ends_with("/v1/selfhost/oidc"),
            "{environment_value}"
        );

        // The container spec the group creates carries that exact entry, so the
        // app's OIDC client discovers Ato's issuer through the relay.
        let workspace = tempfile::tempdir().unwrap();
        let context = ResolvedRuntimeLaunchContext::new(
            workspace.path().to_path_buf(),
            "",
            BTreeMap::new(),
            Vec::new(),
            Vec::new(),
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
        let network_environment =
            BTreeMap::from([(environment_name.clone(), environment_value.clone())]);
        let oci = service_oci_spec(
            &spec,
            &group.services[0],
            &context,
            &owner,
            &network_environment,
        )
        .expect("service spec");
        assert_eq!(oci.environment[&environment_name], environment_value);
    }

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
        // Service groups do not model a working directory: only /app.
        let mut moved = group
            .services
            .iter()
            .find(|s| s.name == "web")
            .unwrap()
            .clone();
        moved.working_dir = "/opt/app".to_owned();
        assert!(
            service_oci_spec(spec, &moved, &context, &owner, &BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("service groups run in /app")
        );
        assert!(
            !web.environment
                .keys()
                .any(|name| name.starts_with("ATO_STATE_PATH")),
            "nor does its path"
        );
    }
}
