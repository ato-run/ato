use super::*;

const PROCESS_FIXTURE: &str =
    include_str!("../../tests/fixtures/runtime-launch-spec-v1/fastapi-process.json");
const OCI_FIXTURE: &str =
    include_str!("../../tests/fixtures/runtime-launch-spec-v1/fastapi-oci.json");

fn process_spec() -> RuntimeLaunchSpecV1 {
    RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("process fixture is valid")
}

#[test]
fn process_and_oci_fixtures_parse_and_validate() {
    let process = process_spec();
    assert!(matches!(
        process.realization,
        LaunchRealizationV1::Process(_)
    ));
    let oci = RuntimeLaunchSpecV1::parse(OCI_FIXTURE).expect("oci fixture is valid");
    assert!(matches!(oci.realization, LaunchRealizationV1::Oci(_)));
}

#[test]
fn the_two_realizations_differ_only_in_the_realization_arm() {
    // The contract's central claim. If anything else drifts, Process and OCI
    // have begun to mean different things and P5 cannot reuse P3's model.
    let process = process_spec();
    let oci = RuntimeLaunchSpecV1::parse(OCI_FIXTURE).unwrap();
    assert_eq!(process.endpoints, oci.endpoints);
    assert_eq!(process.readiness, oci.readiness);
    assert_eq!(process.lifecycle, oci.lifecycle);
    assert_eq!(
        process.state_attachments[0].mount_target,
        oci.state_attachments[0].mount_target
    );
    assert_eq!(
        process.state_attachments[0].access,
        oci.state_attachments[0].access
    );
}

#[test]
fn canonical_bytes_are_the_fixture_bytes() {
    // BOTH fixtures, not just one. The process fixture alone hid a real
    // asymmetry: `revision_ref` is null in the OCI case, and a
    // `skip_serializing_if` on the Rust side omitted it while TypeScript
    // emitted `null` — the same spec canonicalizing to different bytes in the
    // two languages these fixtures exist to keep aligned.
    for fixture in [PROCESS_FIXTURE, OCI_FIXTURE] {
        let spec = RuntimeLaunchSpecV1::parse(fixture).expect("fixture is valid");
        assert_eq!(
            String::from_utf8(spec.canonical_bytes().unwrap()).unwrap(),
            fixture.trim_end()
        );
    }
}

#[test]
fn an_oci_reference_must_be_content_addressed() {
    // A mutable tag would launch different code on different days while the
    // spec digest claimed reproducibility.
    for reference in [
        "python:latest",
        "sha256:tooshort",
        "",
        "sha256:ZZZZ5c6ff1b2c1cbb2f8d9a4e5f60718293a4b5c6d7e8f90112233445566778899",
        &format!("sha256:{}", "A".repeat(64)),
    ] {
        let mut spec = RuntimeLaunchSpecV1::parse(OCI_FIXTURE).unwrap();
        spec.realization = LaunchRealizationV1::Oci(OciRealizationV1 {
            image_digest_ref: reference.to_owned(),
            argv: None,
            working_dir: None,
        });
        assert_eq!(
            spec.validate().unwrap_err().code(),
            "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_IMAGE_DIGEST",
            "reference {reference:?} should be refused"
        );
    }
}

#[test]
fn an_empty_identity_is_refused() {
    // An unattributable launch: no receipt, no correlation, no way to say
    // whose state was touched.
    type Blank = fn(&mut RuntimeLaunchSpecV1);
    let cases: [(&str, Blank); 5] = [
        ("run_id", |spec| spec.context.run_id.clear()),
        ("compute_id", |spec| spec.context.compute_id.clear()),
        ("compute_schema_id", |spec| {
            spec.context.compute_schema_id.clear()
        }),
        ("compute_instance_id", |spec| {
            spec.context.compute_instance_id.clear()
        }),
        ("materialization_ref", |spec| {
            spec.workspace.materialization_ref.clear()
        }),
    ];
    for (label, mutate) in cases {
        let mut spec = process_spec();
        mutate(&mut spec);
        assert_eq!(
            spec.validate().unwrap_err().code(),
            "ATO_ERR_RUNTIME_LAUNCH_SPEC_EMPTY_IDENTITY",
            "{label} should be refused"
        );
    }
}

#[test]
fn endpoint_allocation_and_ports_must_agree() {
    // `preferred` with no port prefers nothing; `automatic` WITH one reads as
    // a request the Runner may ignore, and a caller that believed it was
    // honoured would build a URL against a port nobody bound.
    let mut spec = process_spec();
    spec.endpoints[0].allocation = EndpointAllocationV1::Preferred;
    spec.endpoints[0].preferred_port = None;
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_ENDPOINT"
    );

    let mut spec = process_spec();
    spec.endpoints[0].allocation = EndpointAllocationV1::Preferred;
    spec.endpoints[0].preferred_port = Some(0);
    assert!(spec.validate().is_err());

    let mut spec = process_spec();
    spec.endpoints[0].preferred_port = Some(9000);
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_ENDPOINT"
    );

    let mut spec = process_spec();
    spec.endpoints[0].guest_port = Some(0);
    assert!(spec.validate().is_err());
}

#[test]
fn probed_readiness_requires_a_reachable_endpoint() {
    // Without a guest port there is nowhere to connect, so readiness would
    // silently never fire.
    let mut spec = process_spec();
    spec.endpoints[0].guest_port = None;
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_READINESS"
    );
}

#[test]
fn zero_and_inconsistent_timeouts_are_refused() {
    let mut spec = process_spec();
    spec.readiness = ReadinessV1::Process { timeout_ms: 0 };
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_LIFECYCLE"
    );

    let mut spec = process_spec();
    spec.lifecycle.graceful_shutdown_ms = 0;
    assert!(spec.validate().is_err());

    // Killed before being asked to stop.
    let mut spec = process_spec();
    spec.lifecycle.force_kill_after_ms = spec.lifecycle.graceful_shutdown_ms;
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_LIFECYCLE"
    );
}

#[test]
fn the_writer_fence_is_a_number_not_a_capability() {
    // The spec is persisted and digested onto a Run receipt, so a bearer token
    // here would publish an authorization secret. The fence only lets a stale
    // commit be refused; authorization is the authenticated Runner + Run.
    let spec = process_spec();
    assert_eq!(spec.state_attachments[0].writer_fence, Some(12));
    let rendered = serde_json::to_string(&spec).unwrap();
    assert!(rendered.contains("\"writer_fence\":12"));
    assert!(!rendered.contains("writer_fencing_token"));
}

#[test]
fn canonical_digest_is_stable_across_field_order() {
    let spec = process_spec();
    let reordered: RuntimeLaunchSpecV1 =
        serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
    assert_eq!(
        spec.canonical_digest().unwrap(),
        reordered.canonical_digest().unwrap()
    );
    assert!(spec.canonical_digest().unwrap().starts_with("sha256:"));
}

#[test]
fn an_unsupported_protocol_is_refused_rather_than_guessed() {
    let mut spec = process_spec();
    spec.protocol = "ato.runtime-launch-spec.v2".into();
    let error = spec.validate().unwrap_err();
    assert_eq!(
        error.code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_UNSUPPORTED_VERSION"
    );
    // A digest must not exist for a spec no executor would accept.
    assert!(spec.canonical_digest().is_err());
}

#[test]
fn empty_argv_is_refused() {
    let mut spec = process_spec();
    spec.realization = LaunchRealizationV1::Process(ProcessRealizationV1 { argv: vec![] });
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_EMPTY_ARGV"
    );
}

#[test]
fn a_cwd_that_leaves_the_workspace_is_refused() {
    for cwd in [
        "../etc",
        "/etc",
        "app/../..",
        "C:\\app",
        "app//sub",
        "app/.",
    ] {
        let mut spec = process_spec();
        spec.workspace.cwd_relative = cwd.into();
        assert_eq!(
            spec.validate().unwrap_err().code(),
            "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_CWD",
            "cwd {cwd:?} should be refused"
        );
    }
}

#[test]
fn a_duplicate_environment_name_is_refused() {
    let mut spec = process_spec();
    spec.public_env.push(PublicEnvV1 {
        name: "PORT".into(),
        value: "9000".into(),
    });
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_ENV_CONFLICT"
    );
}

#[test]
fn a_secret_may_not_share_a_name_with_a_public_variable() {
    // Otherwise the executor applies whichever came last, and a secret can be
    // silently replaced by a public value.
    let mut spec = process_spec();
    spec.public_env.push(PublicEnvV1 {
        name: "APP_SECRET_KEY".into(),
        value: "not-a-secret".into(),
    });
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_ENV_CONFLICT"
    );
}

#[test]
fn duplicate_state_keys_and_mount_targets_are_refused() {
    let mut spec = process_spec();
    let mut second = spec.state_attachments[0].clone();
    second.mount_target = "/other".into();
    spec.state_attachments.push(second);
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_STATE_KEY_CONFLICT"
    );

    let mut spec = process_spec();
    let mut second = spec.state_attachments[0].clone();
    second.state_key = "other".into();
    spec.state_attachments.push(second);
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_MOUNT_CONFLICT"
    );
}

#[test]
fn a_mount_target_must_be_an_absolute_normal_guest_path() {
    for target in [
        "data",
        "/data/../etc",
        "/",
        "/data/.",
        "C:\\data",
        "/data\\x",
    ] {
        let mut spec = process_spec();
        spec.state_attachments[0].mount_target = target.into();
        assert_eq!(
            spec.validate().unwrap_err().code(),
            "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_MOUNT_TARGET",
            "target {target:?} should be refused"
        );
    }
}

#[test]
fn readiness_must_name_a_declared_endpoint() {
    let mut spec = process_spec();
    spec.readiness = ReadinessV1::Http {
        endpoint_name: "absent".into(),
        path: "/health".into(),
        timeout_ms: 1000,
    };
    assert_eq!(
        spec.validate().unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_READINESS"
    );
}

#[test]
fn a_secret_value_on_the_wire_is_refused_by_the_shape_itself() {
    // `deny_unknown_fields` is the enforcement: a payload carrying a value is
    // rejected at parse, so it can never be accepted-and-then-logged.
    let with_value = PROCESS_FIXTURE.replace(
        r#"{"grant_ref":"grant_01M1J0SECRET000000000000","name":"APP_SECRET_KEY"}"#,
        r#"{"grant_ref":"g","name":"APP_SECRET_KEY","value":"hunter2"}"#,
    );
    assert!(RuntimeLaunchSpecV1::parse(&with_value).is_err());
}

#[test]
fn a_host_path_field_is_refused_by_the_shape_itself() {
    let with_path = PROCESS_FIXTURE.replace(
        r#""state_key":"app_data""#,
        r#""state_key":"app_data","working_copy_path":"/var/lib/ato/state/x""#,
    );
    assert!(RuntimeLaunchSpecV1::parse(&with_path).is_err());
}

#[test]
fn the_logical_spec_carries_no_secret_and_no_host_path() {
    let rendered = format!("{:?}", process_spec());
    assert!(!rendered.contains("hunter2"));
    assert!(!rendered.contains("/var/lib"));
    // What it DOES carry is references, which are safe to persist.
    assert!(rendered.contains("grant_01M1J0SECRET000000000000"));
}
