use std::collections::BTreeMap;
use std::path::PathBuf;

use super::resolved::{ResolvedRuntimeLaunchContext, ResolvedSecret, ResolvedStateAttachment};
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
    // The fixture IS the canonical form, so ato-api can be checked against the
    // same file rather than against a second hand-maintained description.
    let process = process_spec();
    assert_eq!(
        String::from_utf8(process.canonical_bytes().unwrap()).unwrap(),
        PROCESS_FIXTURE.trim_end()
    );
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

// ---------------------------------------------------------- resolved context

fn resolved_context() -> ResolvedRuntimeLaunchContext {
    let root = std::env::temp_dir();
    ResolvedRuntimeLaunchContext::new(
        root,
        "",
        BTreeMap::from([("PORT".to_owned(), "8000".to_owned())]),
        vec![ResolvedSecret::new("APP_SECRET_KEY", "hunter2")],
        vec![ResolvedStateAttachment::new(
            "app_data",
            Some("isr_1".to_owned()),
            PathBuf::from("/var/lib/ato/state/app_data/working"),
            "/data",
            StateAccessV1::ReadWrite,
        )],
        vec![],
    )
    .expect("workspace-rooted cwd resolves")
}

#[test]
fn debug_redacts_secret_values_and_host_paths() {
    // The only remaining way to leak a resolved context is to print it, so
    // printing it must be safe.
    let rendered = format!("{:?}", resolved_context());
    assert!(!rendered.contains("hunter2"));
    assert!(!rendered.contains("/var/lib/ato/state"));
    // Identity survives, so a diagnostic is still worth reading.
    assert!(rendered.contains("APP_SECRET_KEY"));
    assert!(rendered.contains("app_data"));
    assert!(rendered.contains("/data"));
}

#[test]
fn secrets_reach_only_the_spawn_boundary() {
    let context = resolved_context();
    assert_eq!(
        context.public_env().get("PORT").map(String::as_str),
        Some("8000")
    );
    // Not in the public env...
    assert!(!context.public_env().contains_key("APP_SECRET_KEY"));
    // ...and not in what a receipt may observe.
    assert_eq!(context.observed_secret_names(), vec!["APP_SECRET_KEY"]);
    // Only here.
    assert_eq!(
        context
            .environment_for_spawn()
            .get("APP_SECRET_KEY")
            .map(String::as_str),
        Some("hunter2")
    );
}

#[test]
fn observed_state_reports_identity_without_location() {
    let context = resolved_context();
    let observed = context.observed_state();
    assert_eq!(observed[0].state_key, "app_data");
    assert_eq!(observed[0].revision_ref, Some("isr_1"));
    assert_eq!(observed[0].guest_target, "/data");
    let rendered = format!("{observed:?}");
    assert!(!rendered.contains("/var/lib/ato/state"));
}

#[test]
fn a_resolved_cwd_outside_the_workspace_is_refused() {
    let error = ResolvedRuntimeLaunchContext::new(
        PathBuf::from("/tmp/ato-workspace-root"),
        "../escape",
        BTreeMap::new(),
        vec![],
        vec![],
        vec![],
    )
    .unwrap_err();
    assert_eq!(error.code(), "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_CWD");
}
