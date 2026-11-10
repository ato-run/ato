//! `ato data` against a stub Coordinator: the CLI must print what the
//! authority returned and surface its error codes, never invent a revision.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

fn ato() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("ato"))
}

fn stub(responses: Vec<(u16, &'static str)>) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().expect("stub addr").port();
    let handle = thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (port, handle)
}

#[test]
fn health_prints_the_authority_status() {
    let (port, handle) = stub(vec![(
        200,
        r#"{"status":"ok","schema_version":1,"resources":0}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "health",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
            "--json",
        ])
        .output()
        .expect("run ato data health");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"status\": \"ok\""), "{stdout}");
}

#[test]
fn create_prints_the_resource_the_authority_created() {
    let (port, handle) = stub(vec![(
        201,
        r#"{"resource_id":"dres_1","protocol":"todo.board@1","revision":1,"generation":0,"content_digest":"sha256:abc","byte_size":2,"document":{}}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "resource",
            "create",
            "--protocol",
            "todo.board@1",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource create");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("created dres_1 (revision 1)"), "{stdout}");
}

#[test]
fn invoke_prints_the_committed_revision() {
    let (port, handle) = stub(vec![(
        200,
        r#"{"resource_id":"dres_1","revision":2,"generation":0,"content_digest":"sha256:def","replayed":false,"receipt":{"revision":2}}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "operation",
            "invoke",
            "dres_1",
            "--operation-id",
            "op-1",
            "--expected-revision",
            "1",
            "--document",
            r#"{"tasks":{}}"#,
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data operation invoke");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("committed revision 2"), "{stdout}");
}

#[test]
fn surfaces_the_authority_error_code() {
    let (port, handle) = stub(vec![(409, r#"{"error":"operation_id_conflict"}"#)]);
    let output = ato()
        .args([
            "data",
            "operation",
            "invoke",
            "dres_1",
            "--operation-id",
            "op-1",
            "--expected-revision",
            "2",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data operation invoke");
    handle.join().expect("stub");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("operation_id_conflict"), "{stderr}");
}
#[test]
fn protocol_register_prints_the_registered_operation() {
    let (port, handle) = stub(vec![(201, r#"{"status":"registered"}"#)]);
    let output = ato()
        .args([
            "data",
            "protocol",
            "register",
            "todo.board@1",
            "--operation",
            "todo_list_due_dates",
            "--semantics",
            "observation",
            "--access",
            "read",
            "--implementation-digest",
            "sha256:due",
            "--publisher",
            "alice/app",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data protocol register");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("registered todo_list_due_dates on todo.board@1"),
        "{stdout}"
    );
}

#[test]
fn binding_create_prints_the_operation_scope() {
    let (port, handle) = stub(vec![(
        201,
        r#"{"binding_id":"dbind_1","resource_id":"dres_1","port_name":"tasks","protocol":"todo.board@1","access":"operations","operations":["todo_list_due_dates"],"status":"active","binding_epoch":0}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "binding",
            "create",
            "dres_1",
            "--port",
            "tasks",
            "--protocol",
            "todo.board@1",
            "--access",
            "operations",
            "--operations",
            "todo_list_due_dates",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data binding create");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bound dbind_1 (access operations, operations: todo_list_due_dates)"),
        "{stdout}"
    );
}

#[test]
fn binding_document_refusal_surfaces_the_code() {
    let (port, handle) = stub(vec![(403, r#"{"error":"operation_only_binding"}"#)]);
    let output = ato()
        .args([
            "data",
            "binding",
            "document",
            "dbind_1",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data binding document");
    handle.join().expect("stub");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("operation_only_binding"), "{stderr}");
}

#[test]
fn binding_invoke_and_revoke_print_the_authority_result() {
    let (port, handle) = stub(vec![
        (
            200,
            r#"{"resource_id":"dres_1","revision":3,"generation":0,"content_digest":"sha256:ghi","replayed":false,"receipt":{"revision":3}}"#,
        ),
        (200, r#"{"binding_id":"dbind_1","status":"revoked"}"#),
    ]);
    let invoke = ato()
        .args([
            "data",
            "binding",
            "invoke",
            "dbind_1",
            "--operation",
            "todo_list_due_dates",
            "--operation-id",
            "op-due",
            "--expected-revision",
            "2",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data binding invoke");
    assert!(invoke.status.success(), "{invoke:?}");
    let stdout = String::from_utf8_lossy(&invoke.stdout);
    assert!(stdout.contains("committed revision 3"), "{stdout}");

    let revoke = ato()
        .args([
            "data",
            "binding",
            "revoke",
            "dbind_1",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data binding revoke");
    handle.join().expect("stub");
    assert!(revoke.status.success(), "{revoke:?}");
    let stdout = String::from_utf8_lossy(&revoke.stdout);
    assert!(stdout.contains("binding dbind_1: revoked"), "{stdout}");
}

fn capturing_stub(
    responses: Vec<(u16, &'static str)>,
) -> (
    u16,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    thread::JoinHandle<()>,
) {
    use std::sync::{Arc, Mutex};
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().expect("stub addr").port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 16384];
            let read = stream.read(&mut buffer).expect("read");
            record
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buffer[..read]).into_owned());
            let response = format!(
                "HTTP/1.1 {status} OK
content-type: application/json
content-length: {}
connection: close

{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (port, requests, handle)
}

#[test]
fn binding_invoke_with_arguments_sends_them_instead_of_a_document() {
    let (port, requests, handle) = capturing_stub(vec![(
        200,
        r#"{"resource_id":"dres_1","revision":3,"generation":0,"content_digest":"sha256:x","replayed":false,"result":{"task_id":"t"},"receipt":{"revision":3}}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "binding",
            "invoke",
            "dbind_1",
            "--operation",
            "todo_create",
            "--operation-id",
            "op-cli",
            "--expected-revision",
            "2",
            "--arguments",
            r#"{"title":"From the CLI","at":"2026-11-10T00:00:00.000Z"}"#,
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data binding invoke");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("committed revision 3"), "{stdout}");
    let request = requests.lock().unwrap()[0].clone();
    assert!(request.contains(r#""arguments""#), "{request}");
    assert!(!request.contains(r#""document""#), "{request}");
}

#[test]
fn uses_the_stored_login_token_when_none_is_passed() {
    use std::fs;
    let (port, requests, handle) = capturing_stub(vec![(
        200,
        r#"{"status":"ok","schema_version":1,"resources":0}"#,
    )]);
    let home = tempfile::tempdir().unwrap();
    let origin = format!("http://127.0.0.1:{port}");
    fs::write(
        home.path().join("credentials.json"),
        serde_json::json!({
            "credentials": {
                origin.clone(): {
                    "token": "stored-login-token",
                    "handle": "alice",
                    "user_id": "u1",
                    "registry_url": origin,
                    "expires_at": 9999999999u64
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let output = Command::new(assert_cmd::cargo::cargo_bin!("ato"))
        .env("ATO_HOME", home.path())
        .args(["data", "health", "--coordinator", &origin])
        .output()
        .expect("run ato data health with stored token");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let request = requests.lock().unwrap()[0].clone();
    assert!(request.contains("Bearer stored-login-token"), "{request}");
}

#[test]
fn binding_list_prints_scopes_and_filters_by_instance() {
    let binding = r#"{"bindings":[{"binding_id":"dbind_1","resource_id":"dres_1","port_name":"tasks","protocol":"todo.board@1","access":"operations","operations":["todo_list_due_dates"],"status":"active","binding_epoch":0,"instance_id":"calendar-ui","last_used_at":null},{"binding_id":"dbind_2","resource_id":"dres_1","port_name":"tasks","protocol":"todo.board@1","access":"read","operations":[],"status":"active","binding_epoch":0,"instance_id":"todo-ui","last_used_at":"2026-11-10T01:00:00Z"}]}"#;
    let (port, handle) = stub(vec![(200, binding)]);
    let output = ato()
        .args([
            "data",
            "binding",
            "list",
            "--instance",
            "calendar-ui",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data binding list");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("dbind_1"), "{stdout}");
    assert!(stdout.contains("todo_list_due_dates"), "{stdout}");
    assert!(stdout.contains("calendar-ui"), "{stdout}");
    assert!(!stdout.contains("dbind_2"), "{stdout}");
}

#[test]
fn protocol_list_prints_the_registry_view() {
    let body = r#"{"operations":[{"protocol":"todo.board@1","operation":"todo_create","semantics":"evolution","access":"read_write","publisher":"alice/app","implementation_digest":"sha256:abcdef0123456789abcdef","expose_mcp":true}]}"#;
    let (port, requests, handle) = capturing_stub(vec![(200, body)]);
    let output = ato()
        .args([
            "data",
            "protocol",
            "list",
            "--protocol",
            "todo.board@1",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data protocol list");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("todo_create"), "{stdout}");
    assert!(stdout.contains("evolution"), "{stdout}");
    assert!(stdout.contains("mcp"), "{stdout}");
    let request = requests.lock().unwrap()[0].clone();
    assert!(
        request.contains("/v1/protocols/todo.board@1/operations"),
        "{request}"
    );
}

#[test]
fn saved_state_export_and_import_round_trip() {
    let set = r#"{"schema":"ato.data.export-set/v1","resources":[{"schema":"ato.data.export/v1","resource_id":"dres_1","protocol":"todo.board@1","revision":1,"content_digest":"sha256:c69a77cccb6699e6265e3d74409ad48bb3ac76bbbba1bdcce10fdad8bf8c36a1","document":{"tasks":{}}},{"schema":"ato.data.export/v1","resource_id":"dres_2","protocol":"todo.board@1","revision":2,"content_digest":"sha256:7bc5dfb0f7c8e326912b6c10a55446463d90d87b80719f7ebe74c194e1387c0c","document":{"tasks":{"t1":{"id":"t1"}}}}]}"#;
    let (port, requests, handle) = capturing_stub(vec![(200, set)]);
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("saved-state.json");
    let exported = ato()
        .args([
            "data",
            "resource",
            "export-set",
            "--out",
            out.to_str().unwrap(),
            "--format",
            "json",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource export-set");
    handle.join().expect("stub");
    assert!(exported.status.success(), "{exported:?}");
    let request = requests.lock().unwrap()[0].clone();
    assert!(request.contains("/v1/data-resources/export"), "{request}");
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(written["schema"], "ato.data.export-set/v1");
    assert_eq!(written["resources"].as_array().unwrap().len(), 2);

    let (port, handle) = stub(vec![
        (
            201,
            r#"{"resource_id":"dres_new_1","revision":1,"content_digest":"sha256:a"}"#,
        ),
        (
            201,
            r#"{"resource_id":"dres_new_2","revision":1,"content_digest":"sha256:b"}"#,
        ),
    ]);
    let imported = ato()
        .args([
            "data",
            "resource",
            "import-set",
            "--file",
            out.to_str().unwrap(),
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource import-set");
    handle.join().expect("stub");
    assert!(imported.status.success(), "{imported:?}");
    let stdout = String::from_utf8_lossy(&imported.stdout);
    assert!(stdout.contains("dres_new_1"), "{stdout}");
    assert!(stdout.contains("dres_new_2"), "{stdout}");
}

#[test]
fn saved_state_import_refuses_a_tampered_entry_before_creating_anything() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("saved-state.json");
    std::fs::write(
        &file,
        serde_json::json!({
            "schema": "ato.data.export-set/v1",
            "resources": [
                {
                    "schema": "ato.data.export/v1",
                    "resource_id": "dres_1",
                    "protocol": "todo.board@1",
                    "revision": 1,
                    "content_digest": "sha256:c69a77cccb6699e6265e3d74409ad48bb3ac76bbbba1bdcce10fdad8bf8c36a1",
                    "document": {"tasks": {}}
                },
                {
                    "schema": "ato.data.export/v1",
                    "resource_id": "dres_2",
                    "protocol": "todo.board@1",
                    "revision": 2,
                    "content_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                    "document": {"tasks": {"t1": {"id": "t1"}}}
                }
            ]
        })
        .to_string(),
    )
    .unwrap();
    let output = ato()
        .args([
            "data",
            "resource",
            "import-set",
            "--file",
            file.to_str().unwrap(),
            "--coordinator",
            "http://127.0.0.1:1",
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource import-set");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("export_digest_mismatch"), "{stderr}");
}

#[test]
fn resource_export_can_write_canonical_bytes_for_a_snapshot() {
    let envelope = r#"{"schema":"ato.data.export/v1","resource_id":"dres_1","protocol":"todo.board@1","revision":2,"generation":0,"content_digest":"sha256:abc123","byte_size":17,"document":{"tasks":{"t1":{"id":"t1"}}}}"#;
    let (port, handle) = stub(vec![(200, envelope)]);
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("export.jcs");
    let output = ato()
        .args([
            "data",
            "resource",
            "export",
            "dres_1",
            "--out",
            out.to_str().unwrap(),
            "--canonical",
            "--format",
            "json",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource export --canonical");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let bytes = std::fs::read(&out).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_jcs::to_vec(&value).unwrap(), bytes);
}

#[test]
fn resource_export_writes_the_document_with_its_digest() {
    let envelope = r#"{"schema":"ato.data.export/v1","resource_id":"dres_1","protocol":"todo.board@1","revision":2,"generation":0,"content_digest":"sha256:abc123","byte_size":17,"document":{"tasks":{"t1":{"id":"t1"}}}}"#;
    let (port, requests, handle) = capturing_stub(vec![(200, envelope)]);
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("export.json");
    let output = ato()
        .args([
            "data",
            "resource",
            "export",
            "dres_1",
            "--out",
            out.to_str().unwrap(),
            "--format",
            "json",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource export");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let export: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(export["schema"], "ato.data.export/v1");
    assert_eq!(export["resource_id"], "dres_1");
    assert_eq!(export["protocol"], "todo.board@1");
    assert_eq!(export["revision"], 2);
    assert_eq!(export["content_digest"], "sha256:abc123");
    assert_eq!(export["document"]["tasks"]["t1"]["id"], "t1");
    let request = requests.lock().unwrap()[0].clone();
    assert!(request.contains("/export"), "{request}");
}

#[test]
fn resource_import_refuses_a_tampered_digest_before_contacting_the_authority() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("tampered.json");
    std::fs::write(
        &file,
        serde_json::json!({
            "schema": "ato.data.export/v1",
            "resource_id": "dres_1",
            "protocol": "todo.board@1",
            "revision": 2,
            "content_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "document": {"tasks": {"t1": {"id": "t1"}}}
        })
        .to_string(),
    )
    .unwrap();
    // Port 1 is closed: reaching the network at all would be a different error.
    let output = ato()
        .args([
            "data",
            "resource",
            "import",
            "--file",
            file.to_str().unwrap(),
            "--coordinator",
            "http://127.0.0.1:1",
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource import");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("export_digest_mismatch"), "{stderr}");
}

#[test]
fn resource_import_creates_a_new_resource_from_the_export() {
    let (port, requests, handle) = capturing_stub(vec![(
        201,
        r#"{"resource_id":"dres_2","revision":1,"content_digest":"sha256:def"}"#,
    )]);
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("export.json");
    std::fs::write(
        &file,
        serde_json::json!({
            "schema": "ato.data.export/v1",
            "resource_id": "dres_1",
            "protocol": "todo.board@1",
            "revision": 2,
            "content_digest": "sha256:7bc5dfb0f7c8e326912b6c10a55446463d90d87b80719f7ebe74c194e1387c0c",
            "document": {"tasks": {"t1": {"id": "t1"}}}
        })
        .to_string(),
    )
    .unwrap();
    let output = ato()
        .args([
            "data",
            "resource",
            "import",
            "--file",
            file.to_str().unwrap(),
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource import");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("dres_2"), "{stdout}");
    let request = requests.lock().unwrap()[0].clone();
    assert!(request.contains("todo.board@1"), "{request}");
    assert!(request.contains("\"t1\""), "{request}");
}

#[test]
fn quota_prints_usage_and_limit() {
    let (port, handle) = stub(vec![(
        200,
        r#"{"used_bytes":12,"limit_bytes":67108864,"resources":1}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "quota",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data quota");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("used_bytes: 12"), "{stdout}");
    assert!(stdout.contains("limit_bytes: 67108864"), "{stdout}");
    assert!(stdout.contains("resources: 1"), "{stdout}");
}

#[test]
fn gc_prints_the_deleted_result_count() {
    let (port, handle) = stub(vec![(200, r#"{"deleted_results":2}"#)]);
    let output = ato()
        .args([
            "data",
            "gc",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data gc");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("deleted_results: 2"), "{stdout}");
}

#[test]
fn resource_export_writes_a_hosted_compatible_archive() {
    let envelope = r#"{"schema":"ato.data.export/v1","resource_id":"dres_1","protocol":"todo.board@1","revision":2,"generation":0,"content_digest":"sha256:7bc5dfb0f7c8e326912b6c10a55446463d90d87b80719f7ebe74c194e1387c0c","byte_size":17,"document":{"tasks":{"t1":{"id":"t1"}}}}"#;
    let (port, handle) = stub(vec![(200, envelope)]);
    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("saved-state.zip");
    let output = ato()
        .args([
            "data",
            "resource",
            "export",
            "dres_1",
            "--out",
            out.to_str().unwrap(),
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource export");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04"), "not a zip");
    let entries = ato_cli::zip_store::read_zip(&bytes).unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&entries["metadata.json"]).unwrap();
    assert_eq!(metadata["protocol"], "todo.board@1");
    assert_eq!(
        metadata["content_digest"],
        ato_cli::zip_store::sha256_hex(&entries["data.json"])
    );
}

#[test]
fn resource_import_creates_a_new_resource_from_an_archive() {
    let document = br#"{"tasks":{"t1":{"id":"t1"}}}"#;
    let digest = ato_cli::zip_store::sha256_hex(document);
    let metadata = serde_jcs::to_vec(&serde_json::json!({
        "resource_id": "dres_1",
        "display_name": null,
        "protocol": "todo.board@1",
        "revision": 1,
        "generation": 0,
        "content_digest": digest,
        "schema_description_version": 0
    }))
    .unwrap();
    let archive = ato_cli::zip_store::write_zip(&[
        ("metadata.json", metadata.as_slice()),
        ("data.json", document.as_slice()),
    ]);
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("saved-state.zip");
    std::fs::write(&file, archive).unwrap();
    let (port, requests, handle) = capturing_stub(vec![(
        201,
        r#"{"resource_id":"dres_2","revision":1,"content_digest":"sha256:def"}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "resource",
            "import",
            "--file",
            file.to_str().unwrap(),
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource import");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("dres_2"), "{stdout}");
    let request = requests.lock().unwrap()[0].clone();
    assert!(request.contains("todo.board@1"), "{request}");
}

#[test]
fn resource_import_refuses_a_tampered_archive_before_contacting_the_authority() {
    let document = br#"{"tasks":{}}"#;
    let metadata = serde_jcs::to_vec(&serde_json::json!({
        "resource_id": "dres_1",
        "display_name": null,
        "protocol": "todo.board@1",
        "revision": 1,
        "generation": 0,
        "content_digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "schema_description_version": 0
    }))
    .unwrap();
    let archive = ato_cli::zip_store::write_zip(&[
        ("metadata.json", metadata.as_slice()),
        ("data.json", document.as_slice()),
    ]);
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("tampered.zip");
    std::fs::write(&file, archive).unwrap();
    let output = ato()
        .args([
            "data",
            "resource",
            "import",
            "--file",
            file.to_str().unwrap(),
            "--coordinator",
            "http://127.0.0.1:1",
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource import");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("export_digest_mismatch"), "{stderr}");
}
