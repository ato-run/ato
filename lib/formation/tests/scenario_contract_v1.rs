//! Scenario requirements in the Contract: authoring, binding and rendering.
//!
//! A Scenario is a content-addressed Contract requirement. The steps live in an
//! `ato.scenario/1` object; the Contract carries only its `sha256:` reference in
//! the existing `digest` field. These tests pin that shape and the fact that
//! existing Contracts are untouched.

use ato_formation::authoring::{
    AuthoringDraft, BindingContext, BoundContract, ObservationDraft, Observed, RequirementDraft,
    SCENARIO_CONTRACT_VERIFIER, ScenarioRequirement, bind,
};
use ato_formation::capsule_toml::{parse_capsule_toml, render_capsule_toml};
use ato_formation::capsule_toml_v2::parse_capsule_toml_v2;

const SCENARIO_REF: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn ctx() -> BindingContext<'static> {
    BindingContext {
        source_closure_ref: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    }
}

fn route(contract: &str) -> String {
    format!(
        r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "python"
version = "3.12.7"

[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["python3", "-B", "app.py"]
cwd = "."

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

{contract}
"#
    )
}

fn scenario_contract() -> String {
    format!(
        r#"[[contract.require]]
id = "crud"
use = "ato.contract.scenario@1"
port = "app.http"
scenario = "{SCENARIO_REF}"
"#
    )
}

#[test]
fn a_scenario_requirement_binds_the_content_reference_into_k() {
    let draft = parse_capsule_toml(&route(&scenario_contract())).expect("parses");
    let (contract, derivation) = bind(&draft, &ctx()).expect("binds");
    assert_eq!(contract.requirements.len(), 1);
    let requirement = &contract.requirements[0];
    assert_eq!(requirement.id, "crud");
    assert_eq!(requirement.verifier, SCENARIO_CONTRACT_VERIFIER);
    assert_eq!(requirement.port.as_deref(), Some("app.http"));
    assert_eq!(requirement.digest.as_deref(), Some(SCENARIO_REF));
    assert!(requirement.method.is_none());
    assert!(requirement.path.is_none());
    assert!(requirement.status.is_none());
    assert!(requirement.body_digest.is_none());
    assert!(requirement.input.is_none());
    // The requirement is inside K, not D.
    let json = String::from_utf8(serde_jcs::to_vec(&contract).unwrap()).unwrap();
    assert!(json.contains(SCENARIO_REF), "{json}");
    assert!(!json.contains("python"), "{json}");
    assert!(derivation.derivation_ref().is_ok());
}

#[test]
fn a_scenario_contract_is_a_different_capsule_than_an_http_only_one() {
    let http_only = route(
        r#"[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"

[contract.require.expect]
status = 200
"#,
    );
    let http_contract = bind(&parse_capsule_toml(&http_only).unwrap(), &ctx())
        .unwrap()
        .0;
    let scenario = bind(
        &parse_capsule_toml(&route(&scenario_contract())).unwrap(),
        &ctx(),
    )
    .unwrap()
    .0;
    assert_ne!(
        http_contract.contract_ref().unwrap(),
        scenario.contract_ref().unwrap(),
        "a scenario observation is part of K"
    );
    // Re-binding the same scenario document is byte-identical.
    let again = bind(
        &parse_capsule_toml(&route(&scenario_contract())).unwrap(),
        &ctx(),
    )
    .unwrap()
    .0;
    assert_eq!(again, scenario);
}

#[test]
fn the_v1_grammar_round_trips_a_scenario_requirement() {
    let draft = parse_capsule_toml(&route(&scenario_contract())).expect("parses");
    let rendered = render_capsule_toml(&draft).expect("renders");
    assert!(
        rendered.contains(r#"use = "ato.contract.scenario@1""#),
        "{rendered}"
    );
    assert!(rendered.contains(SCENARIO_REF), "{rendered}");
    let reparsed = parse_capsule_toml(&rendered).expect("reparses");
    assert_eq!(reparsed.contract, draft.contract);
    assert_eq!(reparsed.derivation, draft.derivation);
    assert_eq!(
        bind(&reparsed, &ctx()).unwrap().0.contract_ref().unwrap(),
        bind(&draft, &ctx()).unwrap().0.contract_ref().unwrap()
    );
}

#[test]
fn a_scenario_requirement_refuses_an_undeclared_port() {
    let contract = scenario_contract().replace(r#"port = "app.http""#, r#"port = "missing""#);
    let draft = parse_capsule_toml(&route(&contract)).expect("grammar accepts");
    let error = bind(&draft, &ctx()).expect_err("must refuse");
    assert_eq!(error.code(), "authoring_unresolved_reference");
    assert!(error.to_string().contains("missing"), "{error}");
}

#[test]
fn a_scenario_reference_must_be_a_content_address() {
    // The grammar accepts the string; binding refuses a non-sha256 reference.
    let contract = scenario_contract().replace(SCENARIO_REF, "sha256:ABC");
    let draft = parse_capsule_toml(&route(&contract)).expect("grammar accepts");
    let error = bind(&draft, &ctx()).expect_err("must refuse");
    assert_eq!(error.code(), "authoring_malformed");
}

#[test]
fn a_captured_scenario_is_refused_by_grammar_and_by_binding() {
    let contract = scenario_contract().replace(SCENARIO_REF, "capture");
    let error = parse_capsule_toml(&route(&contract)).expect_err("grammar refuses");
    assert_eq!(error.code(), "authoring_malformed");

    // A draft built without the grammar still cannot bind a capture.
    let mut draft = parse_capsule_toml(&route(&scenario_contract())).unwrap();
    draft.contract.requirements.push(ObservationDraft {
        id: "captured".to_owned(),
        requirement: RequirementDraft::Scenario(ScenarioRequirement {
            port: "app.http".to_owned(),
            scenario: Observed::Capture,
        }),
    });
    let error = bind(&draft, &ctx()).expect_err("binding refuses");
    assert_eq!(error.code(), "authoring_capture_unresolvable");
}

#[test]
fn a_scenario_requirement_rejects_http_only_keys() {
    for extra in ["method = \"GET\"", "path = \"/\"", "input = \"workspace\""] {
        let contract = format!("{}{extra}\n", scenario_contract());
        let error = parse_capsule_toml(&route(&contract)).expect_err("grammar refuses");
        assert_eq!(error.code(), "authoring_malformed", "{extra}");
    }
}

#[test]
fn behavioral_identity_is_compatible_with_a_scenario_observation() {
    let contract = format!(
        "[contract]\nidentity = \"behavioral\"\n\n{}",
        scenario_contract()
    );
    let draft = parse_capsule_toml(&route(&contract)).expect("parses");
    bind(&draft, &ctx()).expect("a behavioral Contract may observe a scenario");
}

#[test]
fn a_source_bound_identity_still_requires_the_workspace_observation() {
    let contract = format!(
        "[contract]\nidentity = \"source-bound\"\n\n{}",
        scenario_contract()
    );
    let draft = parse_capsule_toml(&route(&contract)).expect("parses");
    let error = bind(&draft, &ctx()).expect_err("must refuse");
    assert_eq!(
        error.code(),
        "source_bound_contract_has_no_source_requirement"
    );
}

#[test]
fn a_scenario_is_not_representable_by_the_v2_grammar_as_an_http_observation() {
    // The v2 frontend carries the scenario as its own observation with a
    // source-relative file; an HTTP observation cannot smuggle a scenario key.
    let base = include_str!("../../../samples/portable-model-set-probe/capsule.toml");
    let smuggled = base.replace(
        "status = 200",
        "status = 200\nscenario = \"scenarios/crud.json\"",
    );
    assert!(parse_capsule_toml_v2(&smuggled).is_err());
}

#[test]
fn a_bound_scenario_contract_serializes_only_the_existing_fields() {
    let contract: BoundContract = bind(
        &parse_capsule_toml(&route(&scenario_contract())).unwrap(),
        &ctx(),
    )
    .unwrap()
    .0;
    let value = serde_json::to_value(&contract).unwrap();
    let requirement = &value["requirements"][0];
    assert_eq!(requirement["verifier"], SCENARIO_CONTRACT_VERIFIER);
    assert_eq!(requirement["digest"], SCENARIO_REF);
    assert_eq!(requirement["port"], "app.http");
    for absent in ["method", "path", "status", "body_digest", "input"] {
        assert!(
            requirement.get(absent).is_none(),
            "{absent} must stay absent: {requirement}"
        );
    }
    // Authoring drafts are not the bound Contract; binding is deterministic.
    let draft: AuthoringDraft = parse_capsule_toml(&route(&scenario_contract())).unwrap();
    assert_eq!(
        bind(&draft, &ctx()).unwrap().0.contract_ref().unwrap(),
        contract.contract_ref().unwrap()
    );
}
