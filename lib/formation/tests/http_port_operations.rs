//! Authoring/compiler boundaries. Actual Coordinator/Runtime gates are separate.
use ato_formation::{
    authoring::{AuthoringDraft, BindingContext, bind},
    capsule_toml::{parse_capsule_toml, render_capsule_toml},
    port_operations::{Method, RequestTemplate, RuntimePortOperation, StatusGuard},
    requirements::{AuthorityRequirement, ExecutionPhase, ResourceOperation},
};
use serde_json::json;
use std::collections::BTreeMap;

fn context() -> BindingContext<'static> {
    BindingContext {
        source_closure_ref: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    }
}
fn draft() -> AuthoringDraft {
    parse_capsule_toml(include_str!("fixtures/proposal-python.toml")).unwrap()
}
fn add_operations(d: &mut AuthoringDraft) {
    d.derivation.variable_bindings = serde_json::from_value(json!([{"name":"OWNER_INPUT","kind":"configuration",
        "purpose":"Source-declared local input","resource":"app.http","operation":"execute","phase":"runtime",
        "secret":false,"artifact_embedding":false}])).unwrap();
    let operation = RuntimePortOperation {
        port: "app.http".into(),
        request: RequestTemplate {
            method: Method::Post,
            path: "/source/owner".into(),
            json_bindings: BTreeMap::from([("password".into(), "OWNER_INPUT".into())]),
        },
        accepted_statuses: vec![201],
        when: Some(StatusGuard {
            path: "/".into(),
            statuses: vec![302],
        }),
    };
    d.derivation.requirements.authority = vec![
        operation.authority(),
        AuthorityRequirement {
            phase: ExecutionPhase::Runtime,
            protocol: "ato.http@1".into(),
            resource: "app.http".into(),
            operation: ResourceOperation::Bind,
        },
    ];
    d.derivation.runtime_port_operations.push(operation);
}
#[test]
fn old_bytes_stay_unchanged_operations_change_only_d_and_round_trip_in_authored_order() {
    let mut d = draft();
    let (old_k, old_d) = bind(&d, &context()).unwrap();
    let mut old_json = serde_json::to_value(&old_d).unwrap();
    assert!(old_json.get("runtime_port_operations").is_none());
    old_json["runtime_port_operations"] = json!([]);
    let decoded: ato_formation::authoring::BoundDerivation =
        serde_json::from_value(old_json).unwrap();
    assert_eq!(
        decoded.derivation_ref().unwrap(),
        old_d.derivation_ref().unwrap()
    );
    add_operations(&mut d);
    let (k, new_d) = bind(&d, &context()).unwrap();
    assert_eq!(k, old_k);
    assert_ne!(
        new_d.derivation_ref().unwrap(),
        old_d.derivation_ref().unwrap()
    );
    let rendered = render_capsule_toml(&d).unwrap();
    let parsed = parse_capsule_toml(&rendered).unwrap();
    assert_eq!(bind(&parsed, &context()).unwrap(), (k.clone(), new_d));
    let mut second = d.derivation.runtime_port_operations[0].clone();
    second.request.path = "/source/other".into();
    d.derivation.runtime_port_operations.push(second);
    let (k2, d2) = bind(&d, &context()).unwrap();
    d.derivation.runtime_port_operations.reverse();
    let (k3, d3) = bind(&d, &context()).unwrap();
    assert_eq!(k2, k3);
    assert_ne!(d2.derivation_ref().unwrap(), d3.derivation_ref().unwrap());
}
#[test]
fn unbound_input_authority_port_phase_and_embedding_are_rejected() {
    for mutation in 0..6 {
        let mut d = draft();
        add_operations(&mut d);
        match mutation {
            0 => d.derivation.variable_bindings.clear(),
            1 => d.derivation.variable_bindings[0].artifact_embedding = true,
            2 => d.derivation.variable_bindings[0].phase = ExecutionPhase::Build,
            3 => d.derivation.requirements.authority.clear(),
            4 => d.derivation.runtime_port_operations[0].port = "undeclared".into(),
            _ => d.derivation.steps[0].protocol = "ato.browser@1".into(),
        }
        assert!(bind(&d, &context()).is_err(), "mutation {mutation}");
    }
}
#[test]
fn raw_values_hosts_query_headers_and_unbounded_guards_cannot_enter_the_schema() {
    let mut d = draft();
    add_operations(&mut d);
    let operation = &d.derivation.runtime_port_operations[0];
    for field in ["json_values", "headers", "host", "response_capture"] {
        let mut value = serde_json::to_value(operation).unwrap();
        value["request"][field] = json!("private-canary");
        assert!(serde_json::from_value::<RuntimePortOperation>(value).is_err());
    }
    for path in [
        "https://example.com",
        "//example.com",
        "/?token=private",
        "/a/../b",
        "/a\r\nInjected",
    ] {
        let mut changed = operation.clone();
        changed.request.path = path.into();
        assert!(changed.validate().is_err());
    }
    let mut changed = operation.clone();
    changed.when.as_mut().unwrap().statuses = vec![302, 302];
    assert!(changed.validate().is_err());
}
