use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use ato_portable_application::bundle_sha256;
use serde_json::{Value, json};

struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    id: usize,
}

impl Client {
    fn connect(home: &Path, workspace: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ato"))
            .env("ATO_HOME", home)
            .env_remove("ATO_PUBLICATION_BROWSER_VERIFIER")
            .args(["publish", "mcp", "--workspace"])
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            input,
            output,
            id: 0,
        };
        let initialized = client.rpc("initialize", json!({"protocolVersion":"2025-11-25"}));
        assert!(initialized["capabilities"]["tools"].is_object());
        client
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        serde_json::to_writer(
            &mut self.input,
            &json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params}),
        )
        .unwrap();
        self.input.write_all(b"\n").unwrap();
        self.input.flush().unwrap();
        let mut line = String::new();
        assert_ne!(self.output.read_line(&mut line).unwrap(), 0);
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], self.id);
        response["result"].clone()
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({"name":tool,"arguments":arguments}))
    }

    fn ok(&mut self, tool: &str, arguments: Value) -> Value {
        let reply = self.call(tool, arguments);
        assert_eq!(reply["isError"], false, "{reply}");
        reply["structuredContent"].clone()
    }

    fn wait(&mut self, id: &str) -> Value {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let reply = self.ok(
                "publication_wait",
                json!({"preparation_id":id,"wait_ms":300}),
            );
            if !matches!(reply["phase"].as_str(), Some("queued" | "verifying")) {
                return reply;
            }
            assert!(Instant::now() < until, "{reply}");
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scope() -> Value {
    json!({
        "kind":"public","api_url":"http://127.0.0.1:9","name":"tester/app","tag":"latest","license":"MPL-2.0"
    })
}

fn site(workspace: &Path) -> PathBuf {
    let site = workspace.join("site");
    fs::create_dir(&site).unwrap();
    fs::write(
        site.join("index.html"),
        b"<!doctype html><h1>Frozen site</h1>",
    )
    .unwrap();
    fs::write(site.join("proof.txt"), b"Frozen response").unwrap();
    site
}

#[test]
fn mcp_freezes_source_before_execution_and_requires_terminal_approval() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let site = site(workspace.path());
    let initial = fs::read(site.join("proof.txt")).unwrap();
    let mut client = Client::connect(home.path(), workspace.path());
    let tools = client.rpc("tools/list", json!({}));
    assert!(
        tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"] != "publication_approve")
    );
    let prepared = client.ok("publication_prepare", json!({
        "source":"site","destination":scope(),
        "http_checks":[{"id":"proof","path":"/proof.txt","status":200,"body_sha256":bundle_sha256(&initial)}]
    }));
    let id = prepared["preparation_id"].as_str().unwrap();
    let digest = prepared["plan"]["artifact_sha256"].as_str().unwrap();
    let contract = prepared["plan"]["base_contract_ref"].as_str().unwrap();
    assert_eq!(prepared["phase"], "prepared");
    fs::write(
        site.join("proof.txt"),
        "Never included in the frozen bundle",
    )
    .unwrap();
    client.ok("publication_start", json!({"preparation_id":id}));
    drop(client);

    let mut reconnected = Client::connect(home.path(), workspace.path());
    let outcome = reconnected.wait(id);
    assert_eq!(outcome["phase"], "ready", "{outcome}");
    assert_eq!(outcome["attempts"][0]["artifact_sha256"], digest);
    assert_eq!(outcome["attempts"][0]["base_contract_ref"], contract);
    assert_eq!(outcome["attempts"][0]["verdict"], "pass");
    assert_eq!(outcome["attempts"][0]["cleanup_confirmed"], true);
    assert_eq!(outcome["attempts"][0]["mechanical"][0]["id"], "proof");
    assert_eq!(
        outcome["attempts"][0]["mechanical"][0]["observed_http"]["status"],
        200
    );
    let report = outcome["report_ref"].as_str().unwrap();
    let refused = reconnected.call(
        "publication_commit",
        json!({"preparation_id":id,"report_ref":report}),
    );
    assert_eq!(refused["isError"], true);
    assert!(
        refused["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("user-operated terminal")
    );

    let stored = home.path().join("publication").join(id).join("state.json");
    let mut document: Value = serde_json::from_slice(&fs::read(&stored).unwrap()).unwrap();
    document["record"]["plan"]["destination"]["tag"] = json!("other");
    fs::write(&stored, serde_json::to_vec(&document).unwrap()).unwrap();
    let status = reconnected.call("publication_status", json!({"preparation_id":id}));
    assert_eq!(status["isError"], true);
    assert_eq!(
        status["structuredContent"]["error"],
        "Preparation authentication failed"
    );
}

#[test]
fn required_conditions_distinguish_unavailable_from_failed_and_existing_identity_is_reused() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    site(workspace.path());
    let capsule = workspace.path().join("app.capsule");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule"),
        &capsule,
    )
    .unwrap();
    let bytes = fs::read(&capsule).unwrap();
    let (bundle, _) = ato_portable_application::validate_bytes_all(&bytes).unwrap();
    let mut client = Client::connect(home.path(), workspace.path());
    let blocked = client.ok("publication_prepare", json!({
        "source":"app.capsule","destination":scope(),"unsupported_required":["User can complete a checkout"]
    }));
    let id = blocked["preparation_id"].as_str().unwrap();
    assert_eq!(
        blocked["plan"]["base_contract_ref"],
        bundle.index.root_contract_ref
    );
    assert_eq!(
        blocked["plan"]["derivations"],
        json!(bundle.index.derivations)
    );
    assert_eq!(blocked["plan"]["artifact_sha256"], bundle_sha256(&bytes));
    assert_eq!(
        client.call("publication_start", json!({"preparation_id":id}))["isError"],
        true
    );
    assert_eq!(
        client.ok("publication_cancel", json!({"preparation_id":id}))["phase"],
        "cancelled"
    );

    let mismatch = client.ok(
        "publication_prepare",
        json!({
            "source":"site","destination":scope(),
            "http_checks":[{"id":"home","path":"/","status":201}]
        }),
    );
    let id = mismatch["preparation_id"].as_str().unwrap();
    client.ok("publication_start", json!({"preparation_id":id}));
    let outcome = client.wait(id);
    assert_eq!(outcome["phase"], "unmet", "{outcome}");
    assert_eq!(outcome["attempts"][0]["verdict"], "fail");
    assert_eq!(
        outcome["attempts"][0]["mechanical"][0]["observed_http"]["status"],
        200
    );
    assert!(outcome["report_ref"].is_null());
}

#[test]
fn cross_workspace_sources_and_receipt_substitution_are_refused() {
    let workspace = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let outside = site(elsewhere.path());
    let mut client = Client::connect(home.path(), workspace.path());
    let response = client.call(
        "publication_prepare",
        json!({"source":outside,"destination":scope()}),
    );
    assert_eq!(response["isError"], true);
    assert_eq!(
        response["structuredContent"]["error"],
        "The requested source is outside the authorized workspace"
    );

    site(workspace.path());
    let reply = client.ok("publication_prepare", json!({
        "source":"site","destination":scope(),"http_checks":[{"id":"home","path":"/","status":200}]
    }));
    let id = reply["preparation_id"].as_str().unwrap();
    let artifact = home
        .path()
        .join("publication")
        .join(id)
        .join("artifact.capsule");
    fs::write(artifact, "changed").unwrap();
    let start = client.call("publication_start", json!({"preparation_id":id}));
    assert_eq!(start["isError"], true);
    assert_eq!(
        start["structuredContent"]["error"],
        "The frozen artifact changed. Start a new preparation"
    );
}

#[test]
fn reports_declared_routes_and_scopes_passes_to_the_requested_host() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples/interop-multi-derivation.capsule"),
        workspace.path().join("app.capsule"),
    )
    .unwrap();
    let mut client = Client::connect(home.path(), workspace.path());
    let host = ato_formation_worker::local::host_triple();
    let other = if host == "x86_64-unknown-linux-gnu" {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let wrong = client.ok(
        "publication_prepare",
        json!({"source":"app.capsule","destination":scope(),"target_host":other}),
    );
    let id = wrong["preparation_id"].as_str().unwrap();
    client.ok("publication_start", json!({"preparation_id":id}));
    let outcome = client.wait(id);
    assert_eq!(outcome["phase"], "inconclusive", "{outcome}");
    assert_eq!(outcome["plan"]["target_host"], other);
    let routes = outcome["plan"]["declared_routes"].as_array().unwrap();
    assert!(routes.len() >= 2);
    let attempts = outcome["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), routes.len());
    assert!(attempts.iter().all(|attempt| {
        attempt["host"] == host
            && attempt["verdict"] == "inconclusive"
            && attempt["issue"] == "requested_verification_host_is_unavailable"
    }));

    let local = client.ok(
        "publication_prepare",
        json!({"source":"app.capsule","destination":scope(),"target_host":host}),
    );
    let id = local["preparation_id"].as_str().unwrap();
    client.ok("publication_start", json!({"preparation_id":id}));
    let outcome = client.wait(id);
    assert_eq!(outcome["phase"], "ready", "{outcome}");
    assert!(
        outcome["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|attempt| { attempt["host"] == host && attempt["verdict"] == "pass" })
    );
}

#[test]
fn forms_authored_native_service_groups_with_their_original_contract_and_peer_ports() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/native-process-group");
    let source = workspace.path().join("group");
    fs::create_dir(&source).unwrap();
    for name in ["capsule.toml", "backend.py", "proxy.py"] {
        fs::copy(sample.join(name), source.join(name)).unwrap();
    }
    let (_, original) = ato_portable_application::build_authored_bundle_v2(&source).unwrap();
    let prepared = Client::connect(home.path(), workspace.path()).ok(
        "publication_prepare",
        json!({"source":"group","destination":scope()}),
    );
    assert_eq!(prepared["phase"], "prepared");
    assert_eq!(
        prepared["plan"]["base_contract_ref"],
        original.index.root_contract_ref.to_string()
    );
    assert_eq!(
        prepared["plan"]["derivations"],
        json!(original.index.derivations)
    );
    let route = &prepared["plan"]["declared_routes"][0];
    assert_eq!(route["realization"], "Native Process Group");
    assert_eq!(
        route["services"],
        json!([
            {"id":"backend","runtimes":{"python":"3.12"},"ports":["backend.http"]},
            {"id":"web","runtimes":{"python":"3.12"},"ports":["app.http"],"peer_ports":["backend.http"]}
        ])
    );
}

#[test]
fn missing_contained_browser_prevents_a_pass_for_a_frozen_browser_condition() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    site(workspace.path());
    let mut client = Client::connect(home.path(), workspace.path());
    let prepared = client.ok(
        "publication_prepare",
        json!({
            "source":"site","destination":scope(),
            "http_checks":[{"id":"home","path":"/","status":200}],
            "browser_task":"Check the page displays the Frozen site heading."
        }),
    );
    assert_ne!(
        prepared["plan"]["base_contract_ref"],
        prepared["plan"]["effective_contract_ref"]
    );
    let id = prepared["preparation_id"].as_str().unwrap();
    client.ok("publication_start", json!({"preparation_id":id}));
    let outcome = client.wait(id);
    assert_eq!(outcome["phase"], "inconclusive", "{outcome}");
    assert_eq!(
        outcome["attempts"][0]["issue"],
        "contained_browser_verification_is_not_configured"
    );
    assert_eq!(
        client.call("publication_browser_observe", json!({"preparation_id":id}))["isError"],
        true
    );
}

#[test]
fn unmanifested_python_routes_share_one_frozen_contract() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let source = workspace.path().join("dynamic");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("main.py"),
        "raise RuntimeError('publication preparation must not execute source')\n",
    )
    .unwrap();
    let mut client = Client::connect(home.path(), workspace.path());
    let mut request = json!({
        "source":"dynamic", "destination":scope(),
        "http_checks":[{"id":"root","path":"/","status":200}],
        "process_routes":[{"script":"./main.py","python_version":"3.11","guest_port":8080}]
    });
    let single = client.ok("publication_prepare", request.clone());
    request["process_routes"].as_array_mut().unwrap().push(
        json!({"script":"main.py","python_version":"3.12","guest_port":8080,"arguments":["8080"]}),
    );
    let multiple = client.ok("publication_prepare", request.clone());
    assert_eq!(single["phase"], "prepared");
    assert_eq!(multiple["phase"], "prepared");
    assert_eq!(single["plan"]["source_ref"], multiple["plan"]["source_ref"]);
    assert_eq!(
        single["plan"]["base_contract_ref"],
        multiple["plan"]["base_contract_ref"]
    );
    assert_ne!(
        single["plan"]["artifact_sha256"],
        multiple["plan"]["artifact_sha256"]
    );
    assert_eq!(
        multiple["plan"]["declared_routes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        multiple["plan"]["derivations"]
            .as_array()
            .unwrap()
            .contains(&single["plan"]["derivations"][0])
    );

    request["process_routes"][0]["script"] = json!("../external.py");
    assert_eq!(client.call("publication_prepare", request)["isError"], true);
}

#[test]
fn publication_oci_archives_are_explicit_workspace_inputs() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let source = workspace.path().join("site");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("index.html"), "<h1>Static test</h1>").unwrap();
    fs::write(source.join("image.tar"), b"not an OCI image").unwrap();
    let outside_archive = outside.path().join("image.tar");
    fs::copy(source.join("image.tar"), &outside_archive).unwrap();
    fs::copy(source.join("image.tar"), workspace.path().join("image.tar")).unwrap();
    let mut client = Client::connect(home.path(), workspace.path());
    let image = format!("docker.io/tester/image@sha256:{}", "a".repeat(64));
    let mut request = json!({
        "source":"site", "destination":scope(),
        "http_checks":[{"id":"root","path":"/","status":200}],
        "oci_archives":[{"image":image,"path":outside_archive}]
    });
    let rejected = client.call("publication_prepare", request.clone());
    assert_eq!(rejected["isError"], true);
    assert_eq!(
        rejected["structuredContent"]["error"],
        "The OCI archive must be a file inside the authorized workspace"
    );
    request["oci_archives"][0]["path"] = json!("site/image.tar");
    let rejected = client.call("publication_prepare", request.clone());
    assert_eq!(
        rejected["structuredContent"]["error"],
        "Place OCI archives beside the source directory, outside the captured source tree"
    );
    fs::remove_file(source.join("image.tar")).unwrap();
    request["oci_archives"][0]["path"] = json!("image.tar");
    let rejected = client.call("publication_prepare", request);
    assert_eq!(
        rejected["structuredContent"]["error"],
        "The supplied OCI archive must name a pinned image declared by the artifact"
    );
}
