use super::*;
use crate::runtime_launch::LaunchRealizationV1;

const GROUP_FIXTURE: &str =
    include_str!("../../tests/fixtures/runtime-launch-spec-v2/service-group.json");
const V1_OCI_FIXTURE: &str =
    include_str!("../../tests/fixtures/runtime-launch-spec-v1/fastapi-oci.json");

fn group() -> RuntimeLaunchSpecV2 {
    RuntimeLaunchSpecV2::parse(GROUP_FIXTURE).expect("group fixture is valid")
}

fn refused(mutate: impl FnOnce(&mut RuntimeLaunchSpecV2)) -> RuntimeLaunchSpecError {
    let mut spec = group();
    mutate(&mut spec);
    spec.validate().expect_err("mutation must be refused")
}

fn services(spec: &mut RuntimeLaunchSpecV2) -> &mut Vec<OciServiceV2> {
    match &mut spec.realization {
        LaunchRealizationV2::OciServiceGroup(group) => &mut group.services,
    }
}

#[test]
fn canonical_bytes_are_the_fixture_bytes() {
    let spec = group();
    assert_eq!(
        String::from_utf8(spec.canonical_bytes().unwrap()).unwrap(),
        GROUP_FIXTURE
    );
    // Pinned in ato-api too: the control plane's factory must produce these
    // exact bytes for the same group.
    assert_eq!(
        spec.canonical_digest().unwrap(),
        "sha256:0b31af7926415dc9b94027c83af2bb77812fe86f960450e9d55525a8b5afbdb1"
    );
}

#[test]
fn the_versioned_parser_dispatches_on_protocol_and_refuses_the_unknown() {
    assert!(matches!(
        RuntimeLaunchSpec::parse(GROUP_FIXTURE).unwrap(),
        RuntimeLaunchSpec::V2(_)
    ));
    let v1 = RuntimeLaunchSpec::parse(V1_OCI_FIXTURE).unwrap();
    let RuntimeLaunchSpec::V1(inner) = &v1 else {
        panic!("v1 fixture parsed as another version");
    };
    assert!(matches!(inner.realization, LaunchRealizationV1::Oci(_)));
    assert_eq!(
        v1.canonical_digest().unwrap(),
        inner.canonical_digest().unwrap(),
        "dispatch must not change a v1 digest"
    );
    let unknown = GROUP_FIXTURE.replace(
        RUNTIME_LAUNCH_SPEC_V2_PROTOCOL,
        "ato.runtime-launch-spec.v999",
    );
    assert_eq!(
        RuntimeLaunchSpec::parse(&unknown).unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_UNSUPPORTED_VERSION"
    );
}

#[test]
fn a_v1_parser_fails_closed_on_a_group() {
    // What an older Runner does with a group it was never scheduled for.
    assert_eq!(
        RuntimeLaunchSpecV1::parse(GROUP_FIXTURE)
            .unwrap_err()
            .code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_FORBIDDEN_FIELD"
    );
    let mut spec = group();
    spec.protocol = RUNTIME_LAUNCH_SPEC_V1_PROTOCOL.to_owned();
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_UNSUPPORTED_VERSION"
    );
}

#[test]
fn secrets_are_redeemed_across_services_but_scoped_to_one() {
    let spec = RuntimeLaunchSpec::V2(group());
    let grants = spec.secret_grants();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].grant_ref, "portable-binding:admin_secret");
    let RuntimeLaunchSpec::V2(group) = spec else {
        unreachable!()
    };
    assert!(group.service_group().services[1].secret_grants.is_empty());
    let (service, surface) = group.service_group().surface().unwrap();
    assert_eq!(
        (service.name.as_str(), surface.name.as_str()),
        ("web", "app.http")
    );
}

#[test]
fn every_group_invariant_is_enforced() {
    type Mutation = Box<dyn FnOnce(&mut RuntimeLaunchSpecV2)>;
    let cases: Vec<(&str, &str, Mutation)> = vec![
        (
            "one service",
            "INVALID_SERVICE_GROUP",
            Box::new(|s| {
                services(s).truncate(1);
            }),
        ),
        (
            "not a DNS label",
            "INVALID_SERVICE_GROUP",
            Box::new(|s| {
                services(s)[0].name = "back.end".to_owned();
            }),
        ),
        (
            "duplicate service",
            "INVALID_SERVICE_GROUP",
            Box::new(|s| {
                services(s)[1].name = "backend".to_owned();
            }),
        ),
        (
            "tag reference",
            "INVALID_IMAGE_DIGEST",
            Box::new(|s| {
                services(s)[0].image_reference = "traefik/whoami:latest".to_owned();
            }),
        ),
        (
            "no Surface",
            "INVALID_ENDPOINT",
            Box::new(|s| {
                services(s)[1].endpoints[0].exposure = EndpointExposureV2::Internal;
            }),
        ),
        (
            "two Surfaces",
            "INVALID_ENDPOINT",
            Box::new(|s| {
                services(s)[0].endpoints[0].exposure = EndpointExposureV2::Surface;
            }),
        ),
        (
            "service without Endpoint",
            "INVALID_SERVICE_GROUP",
            Box::new(|s| {
                services(s)[0].endpoints.clear();
            }),
        ),
        (
            "readiness on a sibling's Endpoint",
            "INVALID_READINESS",
            Box::new(|s| {
                services(s)[0].readiness = ServiceReadinessV2::Tcp {
                    endpoint_name: "app.http".to_owned(),
                    timeout_ms: 1_000,
                };
            }),
        ),
        (
            "HTTP readiness on TCP",
            "INVALID_READINESS",
            Box::new(|s| {
                services(s)[0].readiness = ServiceReadinessV2::Http {
                    endpoint_name: "backend.http".to_owned(),
                    path: "/".to_owned(),
                    timeout_ms: 1_000,
                };
            }),
        ),
        (
            "state shared",
            "STATE_KEY_CONFLICT",
            Box::new(|s| {
                services(s)[1].state_keys.push("data".to_owned());
            }),
        ),
        (
            "state unread",
            "STATE_KEY_CONFLICT",
            Box::new(|s| {
                services(s)[0].state_keys.clear();
            }),
        ),
        (
            "state over workspace",
            "MOUNT_CONFLICT",
            Box::new(|s| {
                services(s)[0].workspace_mount_path = "/data".to_owned();
            }),
        ),
        (
            "secret shadowed in one service",
            "ENV_CONFLICT",
            Box::new(|s| {
                services(s)[0].public_env.push(PublicEnvV1 {
                    name: "ATO_BINDING_ADMIN_SECRET".to_owned(),
                    value: "shadow".to_owned(),
                });
            }),
        ),
        (
            "secret name reused across services",
            "ENV_CONFLICT",
            Box::new(|s| {
                services(s)[1].secret_grants.push(SecretGrantV1 {
                    name: "ATO_BINDING_ADMIN_SECRET".to_owned(),
                    grant_ref: "grant_other".to_owned(),
                });
            }),
        ),
        (
            "total disagrees with the sum",
            "INVALID_SERVICE_GROUP",
            Box::new(|s| match &mut s.realization {
                LaunchRealizationV2::OciServiceGroup(group) => {
                    group.total_limits.cpu_limit_millis = 999;
                }
            }),
        ),
        (
            "aggregate over budget",
            "INVALID_SERVICE_GROUP",
            Box::new(|s| {
                for service in services(s) {
                    service.resource_limits.cpu_limit_millis = 2_500;
                }
                match &mut s.realization {
                    LaunchRealizationV2::OciServiceGroup(group) => {
                        group.total_limits.cpu_limit_millis = 5_000;
                    }
                }
            }),
        ),
        (
            "relative working dir",
            "FORBIDDEN_FIELD",
            Box::new(|s| {
                services(s)[0].working_dir = "app".to_owned();
            }),
        ),
        (
            "route cwd",
            "INVALID_CWD",
            Box::new(|s| {
                s.workspace.cwd_relative = "sub".to_owned();
            }),
        ),
    ];
    for (name, code, mutate) in cases {
        let error = refused(mutate);
        assert_eq!(
            error.code(),
            format!("ATO_ERR_RUNTIME_LAUNCH_SPEC_{code}"),
            "{name}"
        );
    }
    // The untouched fixture is valid, so each refusal above is its own.
    group().validate().unwrap();
}

#[test]
fn a_group_payload_never_carries_a_secret_value() {
    let tampered = GROUP_FIXTURE.replace(
        r#""grant_ref":"portable-binding:admin_secret""#,
        r#""grant_ref":"portable-binding:admin_secret","value":"hunter2""#,
    );
    assert_eq!(
        RuntimeLaunchSpecV2::parse(&tampered).unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_FORBIDDEN_FIELD"
    );
}
