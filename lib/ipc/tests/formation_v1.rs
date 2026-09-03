//! The Formation contract, pinned by fixtures shared with TypeScript.
//!
//! The fixtures are the point. `ato-api` produces these payloads and this crate
//! consumes them, so a field that canonicalizes differently on the two sides is
//! a digest that disagrees — and a digest that disagrees is a build the control
//! plane and the worker each believe is a different build.

use ato_ipc::formation::*;

const PYTHON_JOB: &str = include_str!("fixtures/formation-v1/python-process-job.json");
const PYTHON_RESULT: &str = include_str!("fixtures/formation-v1/python-process-result.json");
const STATIC_RESULT: &str = include_str!("fixtures/formation-v1/static-web-result.json");

#[test]
fn the_fixtures_parse_and_validate() {
    let job = FormationJobV1::parse(PYTHON_JOB).expect("python job");
    assert_eq!(job.protocol, FORMATION_JOB_V1_PROTOCOL);
    assert_eq!(
        job.requested_outputs,
        vec![RequestedOutputV1::ProcessWorkspace]
    );

    let python = FormationResultV1::parse(PYTHON_RESULT).expect("python result");
    let static_web = FormationResultV1::parse(STATIC_RESULT).expect("static result");

    // The same Result contract carries both lanes. If it could not, the control
    // plane would need two registries and two comparators.
    assert_eq!(
        python.materializations[0].kind,
        MaterializationKindV1::ProcessWorkspace
    );
    assert_eq!(
        static_web.materializations[0].kind,
        MaterializationKindV1::StaticWeb
    );
}

#[test]
fn canonical_bytes_are_stable_and_digest_to_a_content_address() {
    for fixture in [PYTHON_RESULT, STATIC_RESULT] {
        let result = FormationResultV1::parse(fixture).expect("fixture");
        let once = result.canonical_bytes().expect("canonicalizes");
        let twice = result.canonical_bytes().expect("canonicalizes");
        assert_eq!(once, twice);
        let digest = result.canonical_digest().expect("digests");
        assert!(
            digest.starts_with("sha256:") && digest.len() == 71,
            "{digest}"
        );
    }
}

#[test]
fn a_forbidden_field_is_refused_by_name_at_any_depth() {
    // Each of these belongs to an execution. A Formation service that could
    // see one is a Formation service that could be made to act on one.
    for (field, payload) in [
        (
            "run_id",
            serde_json::json!({ "protocol": "ato.formation-job.v1", "run_id": "run_1" }),
        ),
        (
            "writer_fence",
            serde_json::json!({ "policy": { "writer_fence": 3 } }),
        ),
        (
            "secret_value",
            // Buried inside a free-form map — exactly where `deny_unknown_fields`
            // would not have looked.
            serde_json::json!({ "authoring": { "overrides": { "secret_value": "hunter2" } } }),
        ),
        (
            "host_port",
            serde_json::json!({ "materializations": [{ "compatibility": { "host_port": "8000" } }] }),
        ),
        (
            "compute_instance_id",
            serde_json::json!({ "a": [{ "b": { "compute_instance_id": "cinst_1" } }] }),
        ),
    ] {
        let error = reject_forbidden_fields(&payload).unwrap_err();
        assert_eq!(error.code(), "ATO_ERR_FORMATION_FORBIDDEN_FIELD");
        assert!(format!("{error}").contains(field), "{field}: {error}");
    }
}

#[test]
fn an_unknown_protocol_fails_closed() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_JOB).expect("json");
    value["protocol"] = serde_json::json!("ato.formation-job.v99");
    let error = FormationJobV1::parse(&value.to_string()).unwrap_err();
    assert_eq!(error.code(), "ATO_ERR_FORMATION_UNSUPPORTED_VERSION");
}

#[test]
fn a_mutable_ref_cannot_stand_in_for_a_pinned_commit() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_JOB).expect("json");
    for candidate in ["main", "922b112", "v1.0.0"] {
        value["source"]["resolved_commit_sha"] = serde_json::json!(candidate);
        let error = FormationJobV1::parse(&value.to_string()).unwrap_err();
        // A short SHA is ambiguous, and ambiguity in an identity is a collision
        // waiting to be found.
        assert_eq!(
            error.code(),
            "ATO_ERR_FORMATION_SOURCE_NOT_PINNED",
            "{candidate}"
        );
    }
}

#[test]
fn a_subdirectory_cannot_escape_its_repository() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_JOB).expect("json");
    for candidate in ["../elsewhere", "/etc", "app/../../secrets", ".."] {
        value["source"]["subdirectory"] = serde_json::json!(candidate);
        let error = FormationJobV1::parse(&value.to_string()).unwrap_err();
        assert_eq!(
            error.code(),
            "ATO_ERR_FORMATION_SOURCE_INVALID",
            "{candidate}"
        );
    }
}

#[test]
fn publishing_a_networked_build_is_refused() {
    // ADR-018: this build cannot confine a networked untrusted source, so the
    // two settings together are refused rather than quietly allowed.
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_JOB).expect("json");
    value["policy"]["publish_enabled"] = serde_json::json!(true);
    let error = FormationJobV1::parse(&value.to_string()).unwrap_err();
    assert_eq!(error.code(), "ATO_ERR_FORMATION_POLICY_UNSAFE");

    // The same job with the network denied is fine.
    value["policy"]["network"] = serde_json::json!("denied");
    FormationJobV1::parse(&value.to_string()).expect("a no-network publish is allowed");
}

#[test]
fn oci_is_reserved_but_refused() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_JOB).expect("json");
    value["requested_outputs"] = serde_json::json!(["oci_image"]);
    let error = FormationJobV1::parse(&value.to_string()).unwrap_err();
    // Accepting it and producing nothing would be worse than saying so.
    assert_eq!(error.code(), "ATO_ERR_FORMATION_OUTPUT_UNSUPPORTED");
}

#[test]
fn a_candidate_must_name_an_artifact_this_result_produced() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_RESULT).expect("json");
    value["realization_candidates"][0]["workspace_materialization_ref"] =
        serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    let error = FormationResultV1::parse(&value.to_string()).unwrap_err();
    // Otherwise the control plane would register a schema whose artifact
    // nothing in this result vouches for.
    assert_eq!(error.code(), "ATO_ERR_FORMATION_RESULT_INVALID");
    assert!(format!("{error}").contains("did not produce"), "{error}");
}

#[test]
fn readiness_must_name_a_port_that_exists() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_RESULT).expect("json");
    value["readiness_contracts"][0]["port_name"] = serde_json::json!("nonexistent");
    let error = FormationResultV1::parse(&value.to_string()).unwrap_err();
    // A Runner would otherwise have nothing to probe and the Run would hang
    // until its timeout.
    assert!(format!("{error}").contains("not exported"), "{error}");
}

#[test]
fn a_state_slot_cannot_mount_outside_the_guest_root() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_RESULT).expect("json");
    for candidate in ["data", "/data/../../etc", "/data/.."] {
        value["state_slot_declarations"][0]["mount_target"] = serde_json::json!(candidate);
        let error = FormationResultV1::parse(&value.to_string()).unwrap_err();
        assert_eq!(
            error.code(),
            "ATO_ERR_FORMATION_RESULT_INVALID",
            "{candidate}"
        );
    }
}

#[test]
fn a_succeeded_result_must_have_produced_something() {
    let mut value: serde_json::Value = serde_json::from_str(PYTHON_RESULT).expect("json");
    value["materializations"] = serde_json::json!([]);
    value["realization_candidates"] = serde_json::json!([]);
    let error = FormationResultV1::parse(&value.to_string()).unwrap_err();
    assert!(
        format!("{error}").contains("produced no materialization"),
        "{error}"
    );
}

#[test]
fn the_result_carries_no_execution_identity() {
    // The whole separation, stated as a test: a Result describes CODE, and
    // everything about a tenant's execution is absent from it.
    let rendered = format!(
        "{:?}",
        FormationResultV1::parse(PYTHON_RESULT).expect("fixture")
    );
    for leaked in [
        "cinst_",
        "run_",
        "lease_",
        "writer_fence",
        "hunter2",
        "/home/",
    ] {
        assert!(!rendered.contains(leaked), "result leaked {leaked}");
    }
}

/// The digests TypeScript must reproduce.
///
/// Written into a fixture rather than asserted only here: `ato-api` reads the
/// same file, so a canonicalization change on either side fails on both.
#[test]
fn cross_language_digests_match_the_recorded_fixture() {
    let recorded: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/formation-v1/expected-digests.json"))
            .expect("recorded digests");

    for (name, fixture) in [
        ("python-process-job", PYTHON_JOB),
        ("python-process-result", PYTHON_RESULT),
        ("static-web-result", STATIC_RESULT),
    ] {
        let digest = if name.ends_with("-job") {
            FormationJobV1::parse(fixture)
                .expect(name)
                .canonical_digest()
        } else {
            FormationResultV1::parse(fixture)
                .expect(name)
                .canonical_digest()
        }
        .expect("digests");
        assert_eq!(
            recorded[name].as_str(),
            Some(digest.as_str()),
            "{name} canonicalizes differently than recorded"
        );
    }
}

#[test]
#[ignore = "generator: run with --ignored to refresh expected-digests.json"]
fn emit_expected_digests() {
    let job = FormationJobV1::parse(PYTHON_JOB).expect("job");
    let python = FormationResultV1::parse(PYTHON_RESULT).expect("python");
    let static_web = FormationResultV1::parse(STATIC_RESULT).expect("static");
    println!(
        "{{\n  \"python-process-job\": \"{}\",\n  \"python-process-result\": \"{}\",\n  \"static-web-result\": \"{}\"\n}}",
        job.canonical_digest().unwrap(),
        python.canonical_digest().unwrap(),
        static_web.canonical_digest().unwrap()
    );
}
