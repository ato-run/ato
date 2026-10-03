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
            ..Default::default()
        },
        accepted_statuses: vec![201],
        when: Some(StatusGuard {
            path: "/".into(),
            statuses: vec![302],
        }),
        ..Default::default()
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

#[test]
fn additive_http_fields_preserve_legacy_request_bytes_and_reject_literals() {
    let legacy = r#"{"port":"app.http","request":{"method":"POST","path":"/source/owner","json_bindings":{"password":"OWNER_INPUT"}},"accepted_statuses":[201]}"#;
    let operation: RuntimePortOperation = serde_json::from_str(legacy).unwrap();
    assert_eq!(
        serde_jcs::to_vec(&operation).unwrap(),
        serde_jcs::to_vec(&serde_json::from_str::<serde_json::Value>(legacy).unwrap()).unwrap()
    );
    for method in ["PUT", "PATCH", "DELETE"] {
        let mut value = serde_json::to_value(&operation).unwrap();
        value["request"]["method"] = json!(method);
        assert!(
            serde_json::from_value::<RuntimePortOperation>(value)
                .unwrap()
                .validate()
                .is_ok()
        );
    }
    for header in [
        "host",
        "cookie",
        "content-length",
        "transfer-encoding",
        "x-forwarded-host",
        "Authorization",
    ] {
        let mut changed = operation.clone();
        changed.request.header_bindings.insert(
            header.into(),
            ato_formation::port_operations::HeaderBinding {
                binding: "OWNER_INPUT".into(),
                encoding: ato_formation::port_operations::HeaderEncoding::Direct,
            },
        );
        assert!(changed.validate().is_err());
    }
    let mut value = serde_json::to_value(&operation).unwrap();
    value["request"]["header_bindings"] = json!({"authorization":{"binding":"OWNER_INPUT","encoding":"bearer","value":"raw-private-value"}});
    assert!(serde_json::from_value::<RuntimePortOperation>(value).is_err());
    for path in ["/entities/{id}/../x", "/entities/{id}?x=y"] {
        let mut changed = operation.clone();
        changed.request.path = path.into();
        assert!(changed.validate().is_err());
    }
}

fn response_chain() -> AuthoringDraft {
    use ato_formation::port_operations::{
        HeaderBinding, HeaderEncoding, ResponseBinding, ResponseCheck, ScalarKind,
    };
    let mut d = draft();
    add_operations(&mut d);
    let first = &mut d.derivation.runtime_port_operations[0];
    first.when = None;
    first.response_bindings = vec![ResponseBinding {
        binding: "ENTITY_ID".into(),
        json_pointer: "/entity/id".into(),
        html_text_id: None,
        json_object_key: None,
        scalar: ScalarKind::String,
        max_bytes: 128,
    }];
    let second = RuntimePortOperation {
        port: first.port.clone(),
        request: RequestTemplate {
            method: Method::Patch,
            path: "/entities/{id}".into(),
            path_bindings: BTreeMap::from([("id".into(), "ENTITY_ID".into())]),
            json_bindings: BTreeMap::from([("enabled".into(), "OWNER_INPUT".into())]),
            json_types: BTreeMap::from([("enabled".into(), ScalarKind::Boolean)]),
            header_bindings: BTreeMap::from([(
                "authorization".into(),
                HeaderBinding {
                    binding: "OWNER_INPUT".into(),
                    encoding: HeaderEncoding::Bearer,
                },
            )]),
            cookies: true,
            ..Default::default()
        },
        accepted_statuses: vec![200],
        response_checks: vec![ResponseCheck {
            json_pointer: "/id".into(),
            header_name: None,
            binding: "ENTITY_ID".into(),
            scalar: None,
        }],
        ..Default::default()
    };
    d.derivation.runtime_port_operations.push(second);
    d
}

#[test]
fn private_response_bindings_require_prior_same_port_capture_and_exact_authority() {
    use ato_formation::port_operations::ResponseCheck;
    assert!(validate_functional_chain(&response_chain()).is_ok());
    for mutation in 0..11 {
        let mut d = response_chain();
        match mutation {
            0 => d.derivation.runtime_port_operations.reverse(),
            1 => {
                d.derivation.runtime_port_operations[0].response_bindings[0].binding =
                    "OWNER_INPUT".into()
            }
            2 => {
                let duplicate =
                    d.derivation.runtime_port_operations[0].response_bindings[0].clone();
                d.derivation.runtime_port_operations[1]
                    .response_bindings
                    .push(duplicate);
            }
            3 => d.derivation.runtime_port_operations[0].response_bindings[0].max_bytes = 4097,
            4 => {
                d.derivation.runtime_port_operations[0].response_bindings[0].json_pointer =
                    "/credential/~1host".into()
            }
            5 => {
                d.derivation.runtime_port_operations[1].request.path = "/entities/prefix{id}".into()
            }
            6 => d.derivation.runtime_port_operations[1]
                .request
                .json_types
                .insert(
                    "undeclared".into(),
                    ato_formation::port_operations::ScalarKind::Boolean,
                )
                .map(|_| ())
                .unwrap_or(()),
            7 => d.derivation.variable_bindings[0].resource = "other.http".into(),
            8 => d.derivation.variable_bindings[0].operation = ResourceOperation::Read,
            9 => {
                let mut port = d.derivation.ports[0].clone();
                port.id = "other.http".into();
                d.derivation.ports.push(port);
                d.derivation.runtime_port_operations[1].port = "other.http".into();
                d.derivation.runtime_port_operations[1]
                    .request
                    .json_bindings
                    .clear();
                d.derivation.runtime_port_operations[1]
                    .request
                    .json_types
                    .clear();
                d.derivation.runtime_port_operations[1]
                    .request
                    .header_bindings
                    .clear();
                d.derivation
                    .requirements
                    .authority
                    .push(d.derivation.runtime_port_operations[1].authority());
            }
            _ => d.derivation.runtime_port_operations[1]
                .response_checks
                .push(ResponseCheck {
                    json_pointer: "/input".into(),
                    header_name: None,
                    binding: "UNKNOWN_INPUT".into(),
                    scalar: None,
                }),
        }
        assert!(
            validate_functional_chain(&d).is_err(),
            "mutation {mutation}"
        );
    }
}

fn validate_functional_chain(d: &AuthoringDraft) -> Result<(), &'static str> {
    let (_, baseline) = bind(&draft(), &context()).unwrap();
    let ports = d
        .derivation
        .ports
        .iter()
        .map(|port| {
            let mut bound = baseline
                .ports
                .iter()
                .find(|p| p.from == port.from)
                .expect("fixture declared process")
                .clone();
            bound.id = port.id.clone();
            bound.protocol = port.protocol.clone();
            bound
        })
        .collect::<Vec<_>>();
    ato_formation::port_operations::validate_bound(
        &d.derivation.runtime_port_operations,
        &ports,
        &baseline.steps,
        &d.derivation.variable_bindings,
        &d.derivation.requirements,
    )
}

#[test]
fn advanced_operations_are_not_implicitly_advertised_by_legacy_exploration() {
    let mut d = draft();
    add_operations(&mut d);
    assert!(d.derivation.runtime_port_operations[0].legacy_exploration_supported());
    for mut operation in response_chain().derivation.runtime_port_operations {
        assert!(!operation.legacy_exploration_supported());
        operation.response_bindings.clear();
        operation.response_checks.clear();
        operation.request.header_bindings.clear();
        operation.request.path_bindings.clear();
        operation.request.cookie_bindings.clear();
        operation.request.json_types.clear();
        operation.request.cookies = false;
        operation.request.method = Method::Put;
        assert!(!operation.legacy_exploration_supported());
    }
}

#[test]
fn legacy_percent_encoded_static_path_and_bytes_are_preserved() {
    let legacy = r#"{"method":"GET","path":"/literal%20path","json_bindings":{}}"#;
    let request: RequestTemplate = serde_json::from_str(legacy).unwrap();
    request.validate().unwrap();
    assert_eq!(
        serde_jcs::to_vec(&request).unwrap(),
        serde_jcs::to_vec(&serde_json::from_str::<serde_json::Value>(legacy).unwrap()).unwrap()
    );
    let mut dynamic = request;
    dynamic.path = "/literal%20path/{id}".into();
    dynamic
        .path_bindings
        .insert("id".into(), "ENTITY_ID".into());
    assert!(dynamic.validate().is_err());
}

#[test]
fn response_selectors_are_exclusive_typed_and_private_predicates_are_scoped() {
    use ato_formation::port_operations::{ObjectKeySelector, ScalarKind};
    let mut d = response_chain();
    d.derivation.runtime_port_operations[0].response_bindings[0]
        .json_pointer
        .clear();
    d.derivation.runtime_port_operations[0].response_bindings[0].json_object_key =
        Some(ObjectKeySelector {
            pointer: String::new(),
            where_pointer: "/url".into(),
            binding: "OWNER_INPUT".into(),
        });
    assert!(validate_functional_chain(&d).is_ok());
    for mutation in 0..6 {
        let mut altered = d.clone();
        let capture = &mut altered.derivation.runtime_port_operations[0].response_bindings[0];
        match mutation {
            0 => capture.json_pointer = "/id".into(),
            1 => capture.html_text_id = Some("key".into()),
            2 => capture.scalar = ScalarKind::Boolean,
            3 => capture.json_object_key.as_mut().unwrap().binding = "ENTITY_ID".into(),
            4 => altered.derivation.variable_bindings[0].artifact_embedding = true,
            _ => capture.json_object_key.as_mut().unwrap().where_pointer = "".into(),
        }
        assert!(
            validate_functional_chain(&altered).is_err(),
            "mutation {mutation}"
        );
    }
    let mut altered = d;
    altered.derivation.runtime_port_operations[1]
        .request
        .cookie_bindings
        .insert("token".into(), "OWNER_INPUT".into());
    altered.derivation.variable_bindings[0].phase = ExecutionPhase::Build;
    assert!(validate_functional_chain(&altered).is_err());
}

#[test]
fn captured_scalar_types_cannot_be_reinterpreted_by_later_requests_or_checks() {
    let mut d = response_chain();
    d.derivation.runtime_port_operations[1]
        .request
        .json_bindings
        .insert("enabled".into(), "ENTITY_ID".into());
    assert!(validate_functional_chain(&d).is_err());
    let mut d = response_chain();
    d.derivation.runtime_port_operations[1].response_checks[0].scalar =
        Some(ato_formation::port_operations::ScalarKind::Boolean);
    assert!(validate_functional_chain(&d).is_err());
}

#[test]
fn source_authoring_and_bound_d_identity_gate_advanced_functional_fields() {
    let mut legacy = draft();
    add_operations(&mut legacy);
    let (_, baseline) = bind(&legacy, &context()).unwrap();
    let advanced = response_chain().derivation.runtime_port_operations;
    for operation in advanced {
        let mut authoring = legacy.clone();
        authoring.derivation.runtime_port_operations = vec![operation.clone()];
        assert!(bind(&authoring, &context()).is_err());
        let mut bound = baseline.clone();
        bound.runtime_port_operations = vec![operation];
        assert!(bound.derivation_ref().is_err());
    }
    assert!(baseline.derivation_ref().is_ok());
}
