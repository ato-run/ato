//! Trusted Protocol handler execution (G3).
//!
//! A Protocol operation names a handler module and an export. The Coordinator
//! runs that export with the current document and the arguments and uses the
//! returned `{ operations, result }` to compute the next document. Handlers
//! never touch storage; they are pure functions of (state, arguments).
//!
//! This slice executes handlers in an isolated QuickJS runtime per call. It is
//! not a sandbox claim: the module text comes from the registered Protocol, not
//! from a caller, and the runtime has no filesystem, network or clock bindings.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerError {
    /// The module could not be evaluated or the export is missing.
    InvalidModule(&'static str),
    /// The handler threw.
    Failed(String),
}

impl std::fmt::Display for HandlerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandlerError::InvalidModule(code) => write!(formatter, "{code}"),
            HandlerError::Failed(message) => write!(formatter, "handler_failed: {message}"),
        }
    }
}

/// Turn an ES module of plain function declarations into a script. The
/// supported shape is the one Protocol handlers use: top-level
/// `export function` / `export const`. Anything else is left for the engine to
/// refuse rather than silently rewritten.
fn as_script(source: &str) -> String {
    source
        .replace("export function", "function")
        .replace("export const", "const")
        .replace("export let", "let")
}

/// Run `export` from `source` with the given state and arguments.
///
/// The handler receives one object: `{ state, arguments }`. The result is the
/// JSON value the handler returned; the caller decides whether it is a valid
/// `{ operations, result }`.
pub fn call_export(
    source: &str,
    export: &str,
    state: &Value,
    arguments: &Value,
) -> Result<Value, HandlerError> {
    if export.is_empty()
        || !export
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
    {
        return Err(HandlerError::InvalidModule("invalid_export"));
    }
    let state_json =
        serde_json::to_string(state).map_err(|_| HandlerError::InvalidModule("invalid_state"))?;
    let arguments_json = serde_json::to_string(arguments)
        .map_err(|_| HandlerError::InvalidModule("invalid_arguments"))?;
    let definition = as_script(source);
    let runtime =
        rquickjs::Runtime::new().map_err(|_| HandlerError::InvalidModule("runtime_unavailable"))?;
    let context = rquickjs::Context::full(&runtime)
        .map_err(|_| HandlerError::InvalidModule("context_unavailable"))?;
    let result: String = context.with(|ctx| {
        // Define the module once; a second definition would redeclare its
        // top-level consts. The probe reports a missing export as a module
        // error instead of a handler failure.
        let kind: String = ctx
            .eval::<String, _>(
                format!(
                    "{definition}
typeof {export}"
                )
                .as_bytes(),
            )
            .map_err(|_| HandlerError::InvalidModule("invalid_module"))?;
        if kind != "function" {
            return Err(HandlerError::InvalidModule("invalid_module"));
        }
        ctx.eval::<String, _>(
            format!(
                "JSON.stringify(({export})({{state: {state_json}, arguments: {arguments_json}}}))"
            )
            .as_bytes(),
        )
        .map_err(|error| {
            let caught = ctx.catch();
            HandlerError::Failed(format!("{error} / {caught:?}"))
        })
    })?;
    if result == "undefined" || result.is_empty() {
        return Err(HandlerError::Failed("handler returned no value".into()));
    }
    serde_json::from_str(&result)
        .map_err(|_| HandlerError::Failed("handler result is not JSON".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MODULE: &str = r#"
const KEY = "board";
function boardOf(state) {
  return state && state.document && typeof state.document === "object" ? state.document : {};
}
export function listDueDates({ state }) {
  const tasks = Object.values(boardOf(state).tasks || {})
    .filter((task) => task && task.dueAt && !task.archived)
    .map((task) => ({
      task_id: String(task.id),
      title: String(task.title || ""),
      due_at: String(task.dueAt),
      completed: task.completed === true,
    }));
  return { operations: [], result: { tasks } };
}
export function createTask({ arguments: args, state }) {
  const board = boardOf(state);
  const tasks = Object.assign({}, board.tasks || {});
  const id = "task-" + (Object.keys(tasks).length + 1);
  tasks[id] = { id, title: String(args.title), dueAt: args.due_at ?? null, completed: false };
  return { operations: [{ kind: "replace", value: { tasks } }], result: { task_id: id } };
}
"#;

    #[test]
    fn runs_an_observation_export() {
        let state = json!({"document": {"tasks": {
            "t1": {"id": "t1", "title": "Due", "dueAt": "2026-11-20", "completed": false},
            "t2": {"id": "t2", "title": "Done", "dueAt": null, "archived": true}
        }}});
        let value = call_export(MODULE, "listDueDates", &state, &json!({})).unwrap();
        assert_eq!(
            value,
            json!({"operations": [], "result": {"tasks": [
                {"task_id": "t1", "title": "Due", "due_at": "2026-11-20", "completed": false}
            ]}})
        );
    }

    #[test]
    fn runs_an_evolution_export_and_returns_the_next_document() {
        let state = json!({"document": {"tasks": {}}});
        let value = call_export(
            MODULE,
            "createTask",
            &state,
            &json!({"title": "Write the migration", "due_at": "2026-12-01"}),
        )
        .unwrap();
        assert_eq!(value["operations"][0]["kind"], "replace", "{value}");
        assert_eq!(
            value["operations"][0]["value"]["tasks"]["task-1"]["title"],
            "Write the migration"
        );
        assert_eq!(value["result"]["task_id"], "task-1");
    }

    #[test]
    fn refuses_a_missing_export_and_a_throwing_handler() {
        let state = json!({"document": {}});
        assert_eq!(
            call_export(MODULE, "missing", &state, &json!({})),
            Err(HandlerError::InvalidModule("invalid_module"))
        );
        let throwing = "export function boom() { throw new Error('no'); }";
        assert!(matches!(
            call_export(throwing, "boom", &state, &json!({})),
            Err(HandlerError::Failed(_))
        ));
        assert_eq!(
            call_export(MODULE, "bad name", &state, &json!({})),
            Err(HandlerError::InvalidModule("invalid_export"))
        );
    }

    /// If the real sample is checked out beside this repo, run its handler too.
    #[test]
    fn runs_the_shared_json_todo_handler_when_present() {
        let path = std::path::Path::new(
            "../../../../ato-samples/samples/shared-json-todo/src/operations/todo.js",
        );
        let Ok(source) = std::fs::read_to_string(path) else {
            eprintln!("sample handler not checked out; skipping");
            return;
        };
        let state = json!({"document": {
            "schemaVersion": 3,
            "columns": [{"id": "todo", "title": "Todo", "order": 1000}],
            "tasks": {
                "a": {"id": "a", "title": "Due soon", "dueAt": "2026-11-20", "completed": false},
                "b": {"id": "b", "title": "No date", "dueAt": null}
            }
        }});
        let value = call_export(&source, "listDueDates", &state, &json!({})).unwrap();
        let tasks = value["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["task_id"], "a");
        assert_eq!(tasks[0]["due_at"], "2026-11-20");
        assert_eq!(tasks[0]["title"], "Due soon");
    }
}
