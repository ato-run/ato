//! Shared conformance cases for the DataResource authorities.
//!
//! The same `conformance/cases.json` is executed by the hosted D1
//! implementation (ato-api) and by this SQLite Coordinator. The case ids and
//! order are the contract; each implementation maps the abstract expectations
//! (committed / refused / conflict) to its own error codes.

use ato_coordinator::{CommitInput, Coordinator, CoordinatorError};
use serde_json::Value;

const CASES: &str = include_str!("../conformance/cases.json");

fn cases() -> Vec<Value> {
    let document: Value = serde_json::from_str(CASES).expect("conformance cases parse");
    assert_eq!(document["schema"], "ato.data-resource-conformance/1");
    document["cases"].as_array().expect("cases array").clone()
}

fn document_of(case: &Value) -> Value {
    case.get("document")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()))
}

fn commit_input(case: &Value) -> CommitInput {
    CommitInput {
        operation_id: case["operation_id"].as_str().unwrap_or_default().to_owned(),
        expected_revision: case["expected_revision"].as_i64().unwrap_or(-1),
        expected_generation: case["expected_generation"].as_i64().unwrap_or(-1),
        document: document_of(case),
    }
}

#[test]
fn the_sqlite_coordinator_passes_the_shared_conformance_cases() {
    let mut coordinator = Coordinator::open_in_memory().expect("in-memory coordinator");
    let mut resource_id = String::new();
    let mut binding_id = String::new();

    for case in cases() {
        let id = case["id"].as_str().expect("case id");
        let action = case["action"].as_str().expect("case action");
        match action {
            "create" => {
                let created = coordinator
                    .create_resource(
                        "local",
                        case["protocol"].as_str().unwrap_or_default(),
                        None,
                        document_of(&case),
                    )
                    .unwrap_or_else(|error| panic!("{id}: create failed: {error}"));
                resource_id = created.resource_id.clone();
                assert_eq!(
                    created.revision,
                    case["expect"]["revision"].as_i64().unwrap(),
                    "{id}: revision"
                );
            }
            "commit" => {
                let outcome = coordinator.commit("local", &resource_id, commit_input(&case));
                match case["expect"].as_str() {
                    Some("conflict") => assert!(
                        matches!(outcome, Err(CoordinatorError::Conflict(_))),
                        "{id}: expected a conflict, got {outcome:?}"
                    ),
                    _ => {
                        let outcome = outcome.unwrap_or_else(|error| panic!("{id}: {error}"));
                        assert_eq!(
                            outcome.revision,
                            case["expect"]["revision"].as_i64().unwrap(),
                            "{id}: revision"
                        );
                        assert_eq!(
                            outcome.replayed,
                            case["expect"]["replayed"].as_bool().unwrap(),
                            "{id}: replayed"
                        );
                    }
                }
            }
            "register_operation" => {
                let result = coordinator.register_operation(
                    "todo.board@1",
                    case["operation"].as_str().unwrap_or_default(),
                    case["semantics"].as_str().unwrap_or_default(),
                    case["access"].as_str().unwrap_or_default(),
                    case["implementation_digest"].as_str().unwrap_or_default(),
                    case["publisher"].as_str().unwrap_or_default(),
                );
                match case["expect"].as_str() {
                    Some("conflict") => assert!(
                        matches!(result, Err(CoordinatorError::Conflict(_))),
                        "{id}: expected a conflict, got {result:?}"
                    ),
                    _ => result.unwrap_or_else(|error| panic!("{id}: {error}")),
                }
            }
            "bind" => {
                let operations = case["operations"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let binding = coordinator
                    .bind_resource(
                        &resource_id,
                        case["port"].as_str().unwrap_or_default(),
                        case["protocol"].as_str().unwrap_or_default(),
                        case["access"].as_str().unwrap_or_default(),
                        operations,
                    )
                    .unwrap_or_else(|error| panic!("{id}: bind failed: {error}"));
                binding_id = binding.binding_id.clone();
                assert_eq!(
                    binding.access,
                    case["expect"]["access"].as_str().unwrap(),
                    "{id}: access"
                );
            }
            "read_binding_document" => {
                let result = coordinator.read_binding_document(&binding_id);
                assert!(
                    matches!(result, Err(CoordinatorError::Forbidden(_))),
                    "{id}: expected a refusal, got {result:?}"
                );
            }
            "invoke" => {
                let result = coordinator.invoke_binding_operation(
                    &binding_id,
                    case["operation"].as_str().unwrap_or_default(),
                    commit_input(&case),
                );
                match case["expect"].as_str() {
                    Some("refused") => assert!(
                        matches!(result, Err(CoordinatorError::Forbidden(_))),
                        "{id}: expected a refusal, got {result:?}"
                    ),
                    _ => {
                        let outcome = result.unwrap_or_else(|error| panic!("{id}: {error}"));
                        assert_eq!(
                            outcome.revision,
                            case["expect"]["revision"].as_i64().unwrap(),
                            "{id}: revision"
                        );
                    }
                }
            }
            "revoke" => {
                let binding = coordinator
                    .revoke_binding(&binding_id)
                    .unwrap_or_else(|error| panic!("{id}: revoke failed: {error}"));
                assert_eq!(
                    binding.status,
                    case["expect"]["status"].as_str().unwrap(),
                    "{id}: status"
                );
            }
            other => panic!("{id}: unknown action {other}"),
        }
    }
}
