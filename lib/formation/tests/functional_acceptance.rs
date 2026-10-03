use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    functional_acceptance::FunctionalAcceptanceV1,
    port_operations::{Method, RequestTemplate, RuntimePortOperation},
    requirements::{ExecutionPhase, ExecutionRequirements},
    variables::VariableRequirement,
};
use std::collections::BTreeMap;
fn fixture() -> (
    ato_formation::authoring::BoundDerivation,
    FunctionalAcceptanceV1,
    ExecutionRequirements,
) {
    let draft = parse_capsule_toml(include_str!("fixtures/proposal-python.toml")).unwrap();
    let (_,d)=bind(&draft,&BindingContext{source_closure_ref:"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}).unwrap();
    let variable:VariableRequirement=serde_json::from_value(serde_json::json!({"name":"FUNCTIONAL_INPUT","kind":"configuration","purpose":"Separately approved local action","resource":"app.http","operation":"execute","phase":"runtime","secret":false,"artifact_embedding":false})).unwrap();
    let operation = RuntimePortOperation {
        port: "app.http".into(),
        request: RequestTemplate {
            method: Method::Post,
            path: "/action".into(),
            json_bindings: BTreeMap::from([("input".into(), variable.name.clone())]),
        },
        accepted_statuses: vec![201],
        when: None,
    };
    let mut ceiling = d.requirements.clone();
    ceiling.authority.push(operation.authority());
    (
        d,
        FunctionalAcceptanceV1 {
            schema: "ato.functional-http-acceptance/1".into(),
            variables: vec![variable],
            operations: vec![operation],
        },
        ceiling,
    )
}
#[test]
fn additional_actions_have_explicit_authority_without_changing_d() {
    let (d, p, c) = fixture();
    let before = d.derivation_ref().unwrap();
    let variables = p.validate(&d, &c).unwrap();
    assert_eq!(variables.len(), 1);
    assert_eq!(before, d.derivation_ref().unwrap());
    assert!(d.variable_bindings.is_empty());
}
#[test]
fn private_value_fields_undeclared_authority_bad_phase_and_binding_replacement_fail_closed() {
    let (d, p, c) = fixture();
    for mutation in 0..5 {
        let mut d = d.clone();
        let mut p = p.clone();
        let mut c = c.clone();
        match mutation {
            0 => c.authority.clear(),
            1 => p.variables[0].artifact_embedding = true,
            2 => p.variables[0].phase = ExecutionPhase::Build,
            3 => d.variable_bindings.push(p.variables[0].clone()),
            _ => p.operations[0].port = "undeclared".into(),
        };
        assert!(p.validate(&d, &c).is_err(), "mutation {mutation}");
    }
    let mut value = serde_json::to_value(&p).unwrap();
    value["operations"][0]["request"]["json_values"] = serde_json::json!({"input":"forbidden"});
    assert!(serde_json::from_value::<FunctionalAcceptanceV1>(value).is_err());
}
