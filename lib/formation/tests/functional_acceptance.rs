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
            ..Default::default()
        },
        accepted_statuses: vec![201],
        when: None,
        ..Default::default()
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

#[test]
fn logical_state_ids_are_rebound_as_physical_keys_without_aliasing() {
    use ato_formation::retained::state_service_key;
    assert_eq!(state_service_key("app_data").unwrap(), "app_data");
    let key = state_service_key("app.data").unwrap();
    assert_eq!(key.len(), 64);
    assert!(key.starts_with("slot_"));
    assert_ne!(key, state_service_key("app-data").unwrap());
    assert_ne!(key, state_service_key(&key).unwrap());
    assert!(state_service_key("../host").is_err());
}

#[test]
fn saved_functional_observations_require_exact_scope_status_and_checks() {
    use ato_formation::port_operations::{Observation, PortOperationObservation, ResponseCheck};
    let (_, mut plan, _) = fixture();
    let check = ResponseCheck {
        json_pointer: "/input".into(),
        header_name: None,
        binding: "FUNCTIONAL_INPUT".into(),
        scalar: None,
    };
    plan.operations[0].response_checks.push(check.clone());
    let observation = |value| PortOperationObservation {
        operation_index: 0,
        port: "app.http".into(),
        guard: false,
        observation: value,
    };
    let evidence = vec![
        observation(Observation::RequestTemplate {
            template: plan.operations[0].request.clone(),
        }),
        observation(Observation::ResponseStatus { status: 201 }),
        observation(Observation::ResponseCheck {
            check,
            matched: true,
        }),
    ];
    assert!(plan.validate_observations(&evidence).is_ok());
    for mutation in 0..6 {
        let mut broken = evidence.clone();
        match mutation {
            0 => {
                broken.pop();
            }
            1 => broken[0].port = "other".into(),
            2 => broken[0].guard = true,
            3 => broken[1].observation = Observation::ResponseStatus { status: 500 },
            4 => {
                if let Observation::ResponseCheck { matched, .. } = &mut broken[2].observation {
                    *matched = false;
                }
            }
            _ => broken.push(broken[2].clone()),
        };
        assert!(
            plan.validate_observations(&broken).is_err(),
            "mutation {mutation}"
        );
    }
}
