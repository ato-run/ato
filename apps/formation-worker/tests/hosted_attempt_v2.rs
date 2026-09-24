//! Real Hosted job, common executor and HTTP observation; simulated artifact API.
use anyhow::Result;
use ato_formation::source::SourceLimits;
use ato_formation_worker::{
    api::FormationApi,
    build::BuildAttempt,
    job::{JobContext, SourceFetcher, TreePacker, run_claimed_job},
    sandbox::BuildLimits,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

struct Fetcher {
    bytes: Vec<u8>,
    calls: AtomicUsize,
}
impl SourceFetcher for Fetcher {
    fn fetch(&self, _: &str, _: &Value) -> Result<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.bytes.clone())
    }
}
struct Packer;
impl TreePacker for Packer {
    fn pack(&self, root: &Path) -> Result<Vec<u8>> {
        ato_formation_worker::pack::pack_tree(root)
    }
}
struct ApiFixture {
    url: String,
    result: Arc<Mutex<Option<Value>>>,
    stopped: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}
impl ApiFixture {
    fn new(fail_upload: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let result = Arc::new(Mutex::new(None));
        let captured = result.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let task = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let end = loop {
                    let count = stream.read(&mut chunk).unwrap();
                    assert_ne!(count, 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(pos) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let header = String::from_utf8_lossy(&bytes[..end]).to_string();
                let len = header
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < end + len {
                    let count = stream.read(&mut chunk).unwrap();
                    assert_ne!(count, 0);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let is_result = header.starts_with("POST /v1/internal/formation/results ");
                if is_result {
                    *captured.lock().unwrap() = Some(
                        serde_json::from_slice::<Value>(&bytes[end..end + len]).unwrap()["result"]
                            .clone(),
                    );
                }
                let status = if fail_upload && !is_result {
                    "503 Unavailable"
                } else {
                    "200 OK"
                };
                let body = if header.starts_with("POST /v1/internal/workspaces ") {
                    r#"{"materialization_ref":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#
                } else {
                    r#"{"compute_schema_id":"schema_fixture"}"#
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            url,
            result,
            stopped,
            task: Some(task),
        }
    }
}
impl Drop for ApiFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.task.take().unwrap().join().unwrap();
    }
}

fn scratch_dir() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp/hosted-attempt-tests");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

fn hosted(fail_upload: bool, wrong_status: bool, process: Option<&str>) -> Value {
    let source = scratch_dir();
    std::fs::write(
        source.path().join("index.html"),
        "<h1>Hosted common attempt</h1>",
    )
    .unwrap();
    if wrong_status {
        std::fs::write(
            source.path().join("capsule.toml"),
            r#"
schema = "ato.capsule/1"
[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."
[[derive.step]]
id = "web"
use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html"
[[port]]
id = "web.http"
use = "ato.http@1"
from = "web"
[[contract.require]]
id = "page"
use = "ato.contract.http@1"
port = "web.http"
path = "/"
method = "GET"
[contract.require.expect]
status = 201
"#,
        )
        .unwrap();
    }
    if let Some(runtime) = process {
        let (version, binary, filename, source_code) = if runtime == "node" {
            (
                "22.14.0",
                "node",
                "server.js",
                r#"require('http').createServer((req,res)=>{res.writeHead(process.version === 'v22.14.0' ? 200 : 500); res.end('node hosted');}).listen(Number(process.env.ATO_ENDPOINT_APP_HTTP_PORT),'127.0.0.1');"#,
            )
        } else {
            (
                "3.12.7",
                "python3",
                "server.py",
                r#"import os,sys
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200 if sys.version_info[:3] == (3,12,7) else 500)
  self.end_headers()
  self.wfile.write(b'python hosted')
HTTPServer(('127.0.0.1',int(os.environ['ATO_ENDPOINT_APP_HTTP_PORT'])),H).serve_forever()
"#,
            )
        };
        let manifest = format!(
            r#"
schema = "ato.capsule/1"
[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."
[[runtime]]
name = "{runtime}"
version = "{version}"
[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/opt/ato/toolchains/{runtime}/{version}/bin/{binary}", "/app/{filename}"]
[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000
[[contract.require]]
id = "health"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
[contract.require.expect]
status = 200
"#
        );
        std::fs::write(source.path().join("capsule.toml"), manifest).unwrap();
        std::fs::write(source.path().join(filename), source_code).unwrap();
    }
    let fetcher = Fetcher {
        bytes: ato_formation_worker::pack::pack_tree(source.path()).unwrap(),
        calls: AtomicUsize::new(0),
    };
    let scratch = scratch_dir();
    let server = ApiFixture::new(fail_upload);
    let api = FormationApi::new(
        reqwest::blocking::Client::new(),
        server.url.clone(),
        "synthetic-test-token".into(),
    );
    let context = JobContext {
        api: &api,
        fetcher: &fetcher,
        packer: &Packer,
        work_root: scratch.path(),
        shim: Path::new(env!("CARGO_BIN_EXE_ato-formation-worker")),
        worker_id: "hosted_test",
        limits: BuildLimits::default(),
        source_limits: SourceLimits::default(),
    };
    let attempt = BuildAttempt {
        job_id: "hosted_job".into(),
        attempt_id: "hosted_attempt".into(),
        attempt_fence: 1,
    };
    let job = json!({"job_id":"hosted_job", "source":{"subdirectory":""}, "target":{"triple":if process.is_some() {"aarch64-linux-gnu"} else {"wasm32-browser"}, "workspace_guest_root":"/app"}, "policy":{"network":if process.is_some() {"dependency_resolution"} else {"denied"}}});
    let outcome = run_claimed_job(
        &context,
        &attempt,
        &job,
        "compute_test",
        "revision_test",
        false,
    );
    let path = scratch.path().join("hosted_attempt/hosted-outcome.json");
    assert!(path.exists(), "outcome missing: {outcome:?}");
    let record: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let endpoint = record["attempt"]["receipt"]["execution"]["endpoint"]
        .as_str()
        .unwrap();
    assert!(
        reqwest::blocking::get(endpoint).is_err(),
        "candidate still answering after Stop: {endpoint}"
    );
    if fail_upload || wrong_status {
        assert!(outcome.is_err());
        assert!(server.result.lock().unwrap().is_none());
    } else {
        assert!(outcome.is_ok(), "{outcome:?}");
        let result = server.result.lock().unwrap().clone().unwrap();
        assert_eq!(result["protocol"], "ato.formation-result.v2");
        assert!(result.get("program_intent_ref").is_none());
        assert!(result.get("effective_build_plan_ref").is_none());
        assert!(
            result["formation_key"]
                .as_str()
                .unwrap()
                .starts_with("v2:sha256:")
        );
        assert_eq!(result["verification_receipt"], record["attempt"]["receipt"]);
        // Test-only export: the shared golden originates from a real observation.
        if process.is_none()
            && let Ok(path) = std::env::var("HOSTED_V2_GOLDEN_OUTPUT")
        {
            std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
        }
    }
    let again = run_claimed_job(
        &context,
        &attempt,
        &job,
        "compute_test",
        "revision_test",
        false,
    );
    assert!(again.is_err());
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        1,
        "redelivery must stop before fetching or execution"
    );
    record
}
#[test]
fn hosted_static_is_observed_then_published() {
    let r = hosted(false, false, None);
    assert_eq!(r["attempt"]["receipt"]["fully_satisfied"], true);
    for name in ["seal", "runtime_verification", "cleanup", "publication"] {
        assert_eq!(r["outcomes"][name]["state"], "succeeded");
    }
}
#[test]
fn publication_failure_keeps_the_satisfied_receipt() {
    let r = hosted(true, false, None);
    assert_eq!(r["attempt"]["receipt"]["fully_satisfied"], true);
    assert_eq!(r["outcomes"]["runtime_verification"]["state"], "succeeded");
    assert_eq!(r["outcomes"]["cleanup"]["state"], "succeeded");
    assert_eq!(r["outcomes"]["publication"]["state"], "failed");
}
#[test]
fn a_failed_http_contract_is_not_published() {
    let r = hosted(false, true, None);
    assert_eq!(r["attempt"]["receipt"]["fully_satisfied"], false);
    assert_eq!(r["outcomes"]["runtime_verification"]["state"], "failed");
    assert_eq!(r["outcomes"]["cleanup"]["state"], "succeeded");
    assert_ne!(r["outcomes"]["publication"]["state"], "succeeded");
}

#[cfg(target_os = "linux")]
#[test]
fn hosted_python_and_node_use_the_common_process_runtime() {
    for runtime in ["python", "node"] {
        let record = hosted(false, false, Some(runtime));
        assert_eq!(
            record["attempt"]["receipt"]["fully_satisfied"], true,
            "{runtime}: {record}"
        );
        assert_eq!(record["outcomes"]["cleanup"]["state"], "succeeded");
    }
}

#[test]
fn hosted_v2_golden_is_shared_with_the_receiver() {
    let result: Value =
        serde_json::from_str(include_str!("fixtures/formation-v2/static-result.json")).unwrap();
    let expected: Value =
        serde_json::from_str(include_str!("fixtures/formation-v2/expected-digests.json")).unwrap();
    assert_eq!(
        ato_formation_worker::job::digest(&serde_jcs::to_vec(&result).unwrap()),
        expected["static-result"]
    );
    let receipt: ato_formation::verify::ContractVerificationReceipt =
        serde_json::from_value(result["verification_receipt"].clone()).unwrap();
    assert!(receipt.fully_satisfied);
    assert_eq!(receipt.contract_ref, result["contract_ref"]);
    assert_eq!(receipt.derivation_ref, result["derivation_ref"]);
    let execution = receipt.execution.unwrap();
    assert_eq!(execution.request_id.as_deref(), result["job_id"].as_str());
    assert_eq!(
        execution.attempt_id.as_deref(),
        result["attempt_id"].as_str()
    );
}
