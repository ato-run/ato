//! Real Linux process adapter. These are infrastructure fixtures, not OSS
//! functional acceptance or real-provider evidence.
#![cfg(target_os = "linux")]

use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    detect::DetectorEvidence,
    execution::{InputFacts, RuntimeBinding, lower_execution},
};
use ato_runtime_attempt::{
    ephemeral::{RequiredPort, TemporaryRealization, TemporaryRealizationRequest},
    network_bridge::HostBridge,
};
use netd::egress::gate::{EgressAllowance, EgressGate};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

fn root() -> tempfile::TempDir {
    let root = std::env::var_os("ATO_EXPLORATION_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".tmp/fx"));
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

const SERVER: &str = r#"
import json, os, socket, urllib.request
from http.server import HTTPServer, BaseHTTPRequestHandler
with open('/data/probe', 'w') as f: f.write('isolated')
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        result = {'state': open('/data/probe').read()}
        try:
            socket.create_connection(('1.1.1.1', 443), timeout=1).close()
            result['direct'] = 'escaped'
        except OSError: result['direct'] = 'blocked'
        try:
            urllib.request.urlopen('https://example.com/', timeout=2)
            result['broker'] = 'escaped'
        except Exception as e: result['broker'] = str(e)
        self.send_response(200); self.end_headers()
        self.wfile.write(json.dumps(result).encode())
    def log_message(self, *args): pass
HTTPServer(('127.0.0.1', int(os.environ['ATO_ENDPOINT_APP_HTTP_PORT'])), Handler).serve_forever()
"#;

#[test]
fn actual_namespace_ingress_state_and_denied_egress() {
    assert!(
        ato_runtime_attempt::launch::sandbox::containment_available(),
        "Linux acceptance must enforce containment"
    );
    let root = root();
    let source = root.path().join("src");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("server.py"), SERVER).unwrap();
    let draft = parse_capsule_toml(
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
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "/app/server.py"]
cwd = "."
[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8080
[[state]]
id = "app.data"
use = "ato.state.filesystem@1"
mount = "/data"
access = "read-write"
[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
[contract.require.expect]
status = 200
"#,
    )
    .unwrap();
    let (_, d) = bind(
        &draft,
        &BindingContext {
            source_closure_ref: &format!("sha256:{}", "1".repeat(64)),
        },
    )
    .unwrap();
    let plan = lower_execution(
        &d,
        InputFacts::capture(&DetectorEvidence {
            present_files: vec!["server.py".into()],
            ..Default::default()
        }),
        RuntimeBinding {
            workspace_guest_root: "/app",
            target_triple: "aarch64-unknown-linux-gnu",
        },
    )
    .unwrap();
    let gate = EgressGate::start(
        "127.0.0.1:0".parse().unwrap(),
        EgressAllowance {
            hosts: vec!["exploration-denied.invalid".into()],
            ports: vec![443],
            max_transfer_bytes: 1024,
        },
    )
    .unwrap();
    let socket = std::path::absolute(root.path().join("gate.sock")).unwrap();
    let bridge = HostBridge::start(&socket, gate.address()).unwrap();
    let scratch = root.path().join("run");
    let shim = PathBuf::from(env!("CARGO_BIN_EXE_ato-formation-worker"));
    let run = TemporaryRealization::launch_scoped(
        &TemporaryRealizationRequest {
            workspace: &source,
            scratch: &scratch,
            derivation: &d,
            plan: &plan,
            ports: &[RequiredPort {
                port_id: "app.http".into(),
                guest_port: 8080,
            }],
            shim: &shim,
            attempt_id: "fixture",
        },
        &socket,
    )
    .unwrap();
    let response: serde_json::Value = reqwest::blocking::get(format!(
        "http://127.0.0.1:{}/",
        run.endpoints()[0].host_port
    ))
    .unwrap()
    .json()
    .unwrap();
    assert_eq!(response["state"], "isolated");
    assert_eq!(response["direct"], "blocked");
    assert!(
        response["broker"].as_str().unwrap().contains("403"),
        "{response}"
    );
    run.destroy().unwrap();
    assert!(!scratch.exists());
    drop(bridge);
    assert!(!socket.exists());
    assert!(
        gate.report()
            .refused
            .iter()
            .any(|d| d.target == "example.com")
    );
}

#[test]
fn broker_rejects_wrong_phase_and_private_targets_without_external_access() {
    let root = root();
    let gate = EgressGate::start(
        "127.0.0.1:0".parse().unwrap(),
        EgressAllowance {
            hosts: vec!["localhost".into()],
            ports: vec![443],
            max_transfer_bytes: 1024,
        },
    )
    .unwrap();
    let socket = std::path::absolute(root.path().join("gate.sock")).unwrap();
    let _bridge = HostBridge::start(&socket, gate.address()).unwrap();
    for host in ["example.com", "localhost"] {
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n"
        )
        .unwrap();
        let mut response = [0; 256];
        let read = stream.read(&mut response).unwrap();
        assert!(String::from_utf8_lossy(&response[..read]).contains("403"));
    }
}
