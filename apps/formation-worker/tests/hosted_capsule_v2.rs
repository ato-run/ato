//! Hosted Formation of an `ato.capsule/2` source: the bundle, the receipt and
//! the additive result the control plane accepts. The API is a local fixture;
//! the bundle builder, the runtime attempt and the verifier are real.
//!
//! The positive cases execute a candidate, so they are Linux-only (bubblewrap
//! plus the pinned toolchain). The refusals need no execution and run
//! everywhere: they prove the typed codes reach the reporter.

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
}
impl SourceFetcher for Fetcher {
    fn fetch(&self, _: &str, _: &Value) -> Result<Vec<u8>> {
        Ok(self.bytes.clone())
    }
}

struct Packer;
/// One request header, lowercased name, trimmed value.
fn header_value(header: &str, name: &str) -> Option<String> {
    header.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix(&format!("{name}:"))
            .map(|value| value.trim().to_owned())
    })
}

impl TreePacker for Packer {
    fn pack(&self, root: &Path) -> Result<Vec<u8>> {
        ato_formation_worker::pack::pack_tree(root)
    }
}

/// What the fixture saw: the published result, the registered bundle bytes,
/// and whether the bundle upload happened before the result.
struct Captured {
    result: Arc<Mutex<Option<Value>>>,
    bundle: Arc<Mutex<Option<Vec<u8>>>>,
    bundle_path: Arc<Mutex<Option<String>>>,
    bundle_identity: Arc<Mutex<Option<(String, String)>>>,
    bundle_refusals: Arc<AtomicUsize>,
    uploads: Arc<AtomicUsize>,
}

struct ApiFixture {
    url: String,
    captured: Captured,
    stopped: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}

impl ApiFixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let captured = Captured {
            result: Arc::new(Mutex::new(None)),
            bundle: Arc::new(Mutex::new(None)),
            bundle_path: Arc::new(Mutex::new(None)),
            bundle_identity: Arc::new(Mutex::new(None)),
            bundle_refusals: Arc::new(AtomicUsize::new(0)),
            uploads: Arc::new(AtomicUsize::new(0)),
        };
        let result = Arc::clone(&captured.result);
        let bundle = Arc::clone(&captured.bundle);
        let bundle_path = Arc::clone(&captured.bundle_path);
        let bundle_identity = Arc::clone(&captured.bundle_identity);
        let bundle_refusals = Arc::clone(&captured.bundle_refusals);
        let uploads = Arc::clone(&captured.uploads);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let task = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 8192];
                let header_end = loop {
                    let count = match stream.read(&mut chunk) {
                        Ok(0) => break 0,
                        Ok(count) => count,
                        Err(_) => break 0,
                    };
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(position) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break position + 4;
                    }
                };
                if header_end == 0 {
                    continue;
                }
                let header = String::from_utf8_lossy(&bytes[..header_end]).to_string();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let body = &bytes[header_end..];
                if header.starts_with("POST /v1/internal/formation/results ") {
                    if let Ok(value) = serde_json::from_slice::<Value>(body) {
                        *result.lock().unwrap() = Some(value["result"].clone());
                    }
                } else if header.starts_with("PUT /v1/internal/formation/attempts/")
                    && header.contains("/portable-bundle/")
                {
                    // The route refuses a registration that does not name K and
                    // D. Mirroring that here is what makes this suite catch a
                    // worker that omits the headers: the fixture must not be
                    // more permissive than the control plane it stands in for.
                    let path = header
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let contract_ref = header_value(&header, "x-ato-contract-ref");
                    let derivation_ref = header_value(&header, "x-ato-derivation-ref");
                    let content_address = |value: &Option<String>| {
                        value
                            .as_deref()
                            .is_some_and(|v| v.starts_with("sha256:") && v.len() == 71)
                    };
                    if !content_address(&contract_ref) || !content_address(&derivation_ref) {
                        bundle_refusals.fetch_add(1, Ordering::SeqCst);
                        let refusal = r#"{"error":"portable_bundle_identity_invalid"}"#;
                        let _ = write!(
                            stream,
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            refusal.len(),
                            refusal
                        );
                        continue;
                    }
                    *bundle_path.lock().unwrap() = Some(path);
                    *bundle.lock().unwrap() = Some(body.to_vec());
                    *bundle_identity.lock().unwrap() =
                        Some((contract_ref.unwrap(), derivation_ref.unwrap()));
                    uploads.fetch_add(1, Ordering::SeqCst);
                }
                let response = if header.contains("/portable-bundle/") {
                    r#"{"bundle_id":"bnd_fixture","artifact_ref":"bnd_fixture","transport_digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","validation_status":"uploading"}"#.to_owned()
                } else {
                    r#"{"compute_schema_id":"schema_fixture"}"#.to_owned()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
            }
        });
        Self {
            url,
            captured,
            stopped,
            task: Some(task),
        }
    }
}

impl Drop for ApiFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn scratch_dir() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp/capsule-v2-tests");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

/// A v2 process route: one Python server, one HTTP observation.
fn process_manifest(runtime: &str, version: &str, server: &str) -> String {
    format!(
        r#"
schema = "ato.capsule/2"

[application]
title = "Hosted v2"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[derivation]]
id = "app"
use = "ato.process@1"
argv = ["{runtime}", "{server}"]
cwd = "."
guest_port = 8000
runtimes = {{ {runtime} = "{version}" }}

[effects]
default = "pure"
"#
    )
}

#[cfg(target_os = "linux")]
const PYTHON_SERVER: &str = r#"import os
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'hosted portable v2')
HTTPServer(('127.0.0.1', int(os.environ['ATO_ENDPOINT_APP_HTTP_PORT'])), H).serve_forever()
"#;

const NODE_SERVER: &str = r#"require('http').createServer((req, res) => {
  res.writeHead(200); res.end('hosted portable v2');
}).listen(Number(process.env.ATO_ENDPOINT_APP_HTTP_PORT), '127.0.0.1');
"#;

struct Hosted {
    outcome: Result<ato_formation_worker::job::JobOutcome>,
    fixture: ApiFixture,
    _source_dir: tempfile::TempDir,
}

/// Run one v2 job against the local API fixture.
fn hosted_with(
    manifest: &str,
    extra: &[(&str, &str)],
    network: &str,
    subdirectory: &str,
) -> Hosted {
    let source = scratch_dir();
    std::fs::write(source.path().join("capsule.toml"), manifest).unwrap();
    std::fs::write(
        source.path().join("workspace.txt"),
        "portable v2 fixture workspace",
    )
    .unwrap();
    for (name, contents) in extra {
        if let Some(parent) = Path::new(name).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(source.path().join(parent)).unwrap();
        }
        std::fs::write(source.path().join(name), contents).unwrap();
    }
    let location = if subdirectory.is_empty() {
        ""
    } else {
        // Keep the manifest outside the selected subdirectory, as the control
        // plane's copy would be: the frontend materializes it into the source
        // root the archive expands to.
        ""
    };
    let _ = location;
    let fetcher = Fetcher {
        bytes: if subdirectory.is_empty() {
            ato_formation_worker::pack::pack_tree(source.path()).unwrap()
        } else {
            let selected = source.path().join(subdirectory);
            std::fs::create_dir_all(&selected).unwrap();
            std::fs::write(selected.join("workspace.txt"), "portable v2 fixture").unwrap();
            ato_formation_worker::pack::pack_tree(source.path()).unwrap()
        },
    };
    let scratch = scratch_dir();
    let fixture = ApiFixture::new();
    let api = FormationApi::new(
        reqwest::blocking::Client::new(),
        fixture.url.clone(),
        "synthetic-test-token".into(),
    );
    let context = JobContext {
        api: &api,
        fetcher: &fetcher,
        packer: &Packer,
        work_root: scratch.path(),
        shim: Path::new(env!("CARGO_BIN_EXE_ato-formation-worker")),
        worker_id: "hosted_v2_test",
        limits: BuildLimits::default(),
        source_limits: SourceLimits::default(),
    };
    let outcome = run_claimed_job(
        &context,
        &BuildAttempt {
            job_id: "v2_job".into(),
            attempt_id: "v2_attempt".into(),
            attempt_fence: 1,
        },
        &json!({
            "job_id": "v2_job",
            "source": {
                "kind": "git_hub",
                "owner": "ato-run",
                "repository": "portable-v2-fixture",
                "resolved_commit_sha": "a".repeat(40),
                "subdirectory": subdirectory,
            },
            "target": {"triple": "x86_64-linux-gnu", "workspace_guest_root": "/app"},
            "policy": {"policy_version": "formation-policy-v1", "network": network},
        }),
        "compute_v2",
        "revision_v2",
        false,
    );
    Hosted {
        outcome,
        fixture,
        _source_dir: source,
    }
}

fn refusal_code(error: &anyhow::Error) -> String {
    if let Some(failure) = error.downcast_ref::<ato_formation::failure::FormationFailure>() {
        return failure.code.clone();
    }
    if let Some(failure) = error.downcast_ref::<ato_formation_worker::job::HostedAttemptFailure>() {
        return failure.report.code.clone();
    }
    format!("untyped: {error:#}")
}

#[test]
fn an_unknown_capsule_schema_is_refused_typed() {
    let hosted = hosted_with(
        r#"
schema = "ato.capsule/3"
[application]
title = "Unsupported"
"#,
        &[],
        "denied",
        "",
    );
    let error = hosted
        .outcome
        .expect_err("an unreadable schema must be refused");
    assert_eq!(
        refusal_code(&error),
        "unsupported_capsule_schema",
        "{error:#}"
    );
    assert!(hosted.fixture.captured.result.lock().unwrap().is_none());
    assert_eq!(hosted.fixture.captured.uploads.load(Ordering::SeqCst), 0);
}

#[test]
fn an_invalid_capsule_v2_document_is_refused_typed() {
    // A v2 document with no Derivations: the strict v2 grammar refuses it, and
    // nothing is built or uploaded.
    let hosted = hosted_with(
        r#"
schema = "ato.capsule/2"

[application]
title = "Invalid v2"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/other"
status = 200

[[derivation]]
id = "app"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = { python = "3.12" }

[effects]
default = "pure"
"#,
        &[("app.py", "print('never runs')")],
        "denied",
        "",
    );
    let error = hosted
        .outcome
        .expect_err("an invalid v2 document must be refused");
    assert_eq!(refusal_code(&error), "capsule_toml_v2_invalid", "{error:#}");
    assert!(hosted.fixture.captured.result.lock().unwrap().is_none());
}

#[test]
fn a_portable_oci_route_without_an_admitted_network_is_refused_typed() {
    let hosted = hosted_with(
        r#"
schema = "ato.capsule/2"

[application]
title = "Memos-like"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[state]]
id = "data"
use = "ato.state.filesystem@1"
mount = "/var/opt/memos"
access = "read-write"

[contract]
mode = "all"

[[contract.observation]]
id = "app-ready"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[derivation]]
id = "oci-linux-amd64"
use = "ato.oci@1"
argv = ["--addr", "0.0.0.0", "--port", "5230", "--data", "/var/opt/memos"]
cwd = "."
guest_port = 5230
runtimes = { "oci.image" = "docker.io/neosmemo/memos@sha256:51a4cef418b1f173ac37139ad99de08da5b8662136007231d3ac8a0498a3095a", "oci.platform" = "linux/amd64", "oci.entrypoint" = "/usr/local/memos/memos", "oci.memory_bytes" = "536870912", "oci.cpu_limit_millis" = "1000", "oci.pids_limit" = "128" }

[effects]
default = "pure"
"#,
        &[],
        "denied",
        "",
    );
    let error = hosted
        .outcome
        .expect_err("an OCI pull needs an admitted network");
    assert_eq!(
        refusal_code(&error),
        "capsule_v2_network_not_admitted",
        "{error:#}"
    );
    assert!(hosted.fixture.captured.result.lock().unwrap().is_none());
    assert_eq!(hosted.fixture.captured.uploads.load(Ordering::SeqCst), 0);
}

#[test]
fn a_node_20_portable_route_is_refused_by_runtime_policy() {
    let manifest = process_manifest("python", "3.12", "server.py").replace(
        "runtimes = { python = \"3.12\" }",
        "runtimes = { python = \"3.12\", node = \"20.20.2\" }",
    );
    let hosted = hosted_with(
        &manifest,
        &[
            ("server.py", "print('never runs')"),
            ("server.js", NODE_SERVER),
        ],
        "denied",
        "",
    );
    let error = hosted
        .outcome
        .expect_err("an affected Node release must not execute");
    assert_eq!(
        refusal_code(&error),
        "runtime_version_not_patched",
        "{error:#}"
    );
    // The refusal is admission: no result is published for a route that never ran.
    assert!(hosted.fixture.captured.result.lock().unwrap().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn a_portable_v2_process_source_publishes_the_additive_result() {
    let hosted = hosted_with(
        &process_manifest("python", "3.12", "server.py"),
        &[("server.py", PYTHON_SERVER)],
        "denied",
        "",
    );
    let job = hosted
        .outcome
        .as_ref()
        .unwrap_or_else(|error| panic!("the portable v2 job runs: {error:#}"));
    assert_eq!(job.materialization_ref, "bnd_fixture");
    let result = hosted
        .fixture
        .captured
        .result
        .lock()
        .unwrap()
        .clone()
        .expect("a fully satisfied route publishes its result");

    // Additive facts: the control plane needs the bundle and the route it ran.
    assert_eq!(result["formation_kind"], "portable_v2");
    assert_eq!(result["schema_profile"], "ato.portable-application/1");
    assert_eq!(
        result["realization_candidates"][0]["kind"],
        "portable_application"
    );
    assert_eq!(
        result["realization_candidates"][0]["schema_profile"],
        "ato.portable-application/1"
    );
    assert_eq!(
        result["realization_candidates"][0]["bundle_artifact_ref"],
        "bnd_fixture"
    );
    let bundle_bytes = hosted
        .fixture
        .captured
        .bundle
        .lock()
        .unwrap()
        .clone()
        .expect("the bundle was registered before the result");
    let bundle_sha = ato_portable_application::bundle_sha256(&bundle_bytes);
    assert_eq!(result["portable_bundle_sha256"], bundle_sha);
    assert_eq!(
        result["materializations"][0]["kind"],
        "portable_application"
    );
    assert_eq!(
        result["materializations"][0]["media_type"],
        "application/vnd.ato.portable-application.v3"
    );
    assert_eq!(result["materializations"][0]["digest"], bundle_sha);
    // The registration named the identity the API checks. A worker that sent
    // only the transport digest would be refused `portable_bundle_identity_invalid`
    // by the real route, and the fixture now refuses it too.
    let (registered_k, registered_d) = hosted
        .fixture
        .captured
        .bundle_identity
        .lock()
        .unwrap()
        .clone()
        .expect("the bundle registration named its Contract and Derivation");
    assert_eq!(registered_k, result["contract_ref"].as_str().unwrap());
    assert_eq!(registered_d, result["derivation_ref"].as_str().unwrap());
    assert_eq!(
        hosted
            .fixture
            .captured
            .bundle_refusals
            .load(Ordering::SeqCst),
        0,
        "the control plane refused no registration"
    );
    let derivation_ref = result["derivation_ref"].as_str().expect("a route ran");
    assert_eq!(
        result["realization_candidates"][0]["derivation_ref"],
        derivation_ref
    );
    let contract_ref = result["contract_ref"].as_str().expect("K is stated");
    let bundle = ato_objects::decode_portable_application_bundle(&bundle_bytes).expect("bundle");
    assert_eq!(
        contract_ref, bundle.index.root_contract_ref,
        "the published K is the bundle root Contract"
    );
    assert_eq!(
        result["protocol"], "ato.formation-result.v2",
        "v2 keeps the v1 result protocol"
    );
    assert_eq!(
        result["verification_receipt"]["fully_satisfied"], true,
        "{result}"
    );
    assert_eq!(result["verification_receipt"]["contract_ref"], contract_ref);
    assert_eq!(
        result["verification_receipt"]["derivation_ref"],
        derivation_ref
    );
    eprintln!(
        "[hosted-capsule-v2] contract_ref={contract_ref} derivation_ref={derivation_ref} \
         portable_bundle_sha256={bundle_sha} fully_satisfied={} artifact_ref={}",
        result["verification_receipt"]["fully_satisfied"], result["portable_bundle_artifact_ref"],
    );
}

/// Gate A acceptance for the real memos source. It needs a Linux Docker host
/// with the pinned image already resolvable, so it is opt-in like the other
/// physical acceptance cases.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Linux Docker, network policy admission and the pinned memos image"]
fn memos_v2_hosted_formation_returns_a_satisfied_receipt() {
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/discover-oss/memos/capsule.toml"),
    )
    .expect("memos sample");
    let hosted = hosted_with(&manifest, &[], "dependency_resolution", "");
    let job = hosted
        .outcome
        .as_ref()
        .unwrap_or_else(|error| panic!("memos forms a capsule: {error:#}"));
    let result = hosted
        .fixture
        .captured
        .result
        .lock()
        .unwrap()
        .clone()
        .expect("a fully satisfied memos route publishes its result");
    assert_eq!(result["formation_kind"], "portable_v2");
    assert_eq!(result["verification_receipt"]["fully_satisfied"], true);
    assert_eq!(
        result["materializations"][0]["kind"],
        "portable_application"
    );
    assert_eq!(result["portable_bundle_artifact_ref"], "bnd_fixture");
    assert!(
        result["derivation_ref"]
            .as_str()
            .unwrap_or_default()
            .starts_with("sha256:")
    );
    assert_eq!(job.materialization_ref, "bnd_fixture");
    eprintln!(
        "[gate-a-memos] contract_ref={} derivation_ref={} portable_bundle_sha256={} \
         fully_satisfied={} artifact_ref={} format=portable_v2",
        result["contract_ref"],
        result["derivation_ref"],
        result["portable_bundle_sha256"],
        result["verification_receipt"]["fully_satisfied"],
        result["portable_bundle_artifact_ref"],
    );
}

#[test]
fn gpu_candidate_is_quarantined_before_missing_host_refusal_without_publishing_success() {
    let manifest = process_manifest("python", "3.12", "server.py")
        + r#"
[derivation.host]
os = "linux"
arch = "x86_64"
[[derivation.host.accelerators]]
vendor = "nvidia"
count = 1
min_vram_mib = 1048576
"#;
    let hosted = hosted_with(
        &manifest,
        &[("server.py", "raise Exception('must not execute')")],
        "denied",
        "",
    );
    let error = hosted
        .outcome
        .expect_err("a host without the declared GPU cannot satisfy Formation");
    assert_eq!(refusal_code(&error), "gpu_host_unavailable", "{error:#}");
    assert!(hosted.fixture.captured.bundle.lock().unwrap().is_some());
    assert!(hosted.fixture.captured.result.lock().unwrap().is_none());
}
