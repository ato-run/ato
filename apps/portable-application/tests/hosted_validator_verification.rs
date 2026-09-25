//! The Hosted `.capsule` verifier against a Hosted-like API.
//!
//! ```text
//! running candidate ◀── observe_url relay ◀── validator_agent ──▶ ack_runtime
//! (owned by the Run)        (this API)          (K ⊨ C, receipt)
//! ```
//!
//! The candidate is already running when the job is claimed, owned by the
//! Hosted Run: a static materialization the API serves, or a `python3`
//! process the API relays to. The validator only observes it through the
//! API and acks a receipt; nothing here starts, stops or cleans up a
//! candidate on the validator's behalf.
//!
//! Set `ATO_HOSTED_VALIDATOR_FIXTURE_OUT=<dir>` to write each case's
//! normalized outcome, for comparing two builds of the validator.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ato_objects::{PortableApplicationBundle, PortableDependencyProfile};
use ato_portable_application::instance_snapshot::{
    DATA_JSON_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotAssetV1,
    InstanceSnapshotResourceV1, InstanceSnapshotV1, attach_instance_snapshot,
};
use ato_portable_application::portability_export::repack_portable_dependencies;
use ato_portable_application::validator_agent::{
    ValidatorAgent, ValidatorAgentConfig, ValidatorRunOutcome,
};
use ato_portable_application::{
    PortableRealizationKind, ValidatedPortableApplication, build_multi_derivation_bundle,
    build_static_bundle, bundle_sha256, materialize_tree, validate_bytes_all,
};
use base64::Engine;
use serde_json::{Value, json};

const RUNTIME_JOBS: &str = "/v1/capsule-bundles/runtime-verification-jobs";

/// The receipt fields the Hosted API accepts (its receipt schema is strict).
const HOSTED_RECEIPT_KEYS: &[&str] = &[
    "schema",
    "bundle_sha256",
    "contract_ref",
    "derivation_ref",
    "target",
    "observations",
    "fully_satisfied",
    "execution",
];
const HOSTED_EXECUTION_KEYS: &[&str] = &[
    "realization",
    "runtime_executable",
    "runtime_version",
    "pid",
    "container_id",
    "image",
    "platform",
    "endpoint",
    "run_id",
    "lease_id",
    "attempt_id",
    "services",
];

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

// ── the running candidate, owned by the Hosted Run ──────────────────────────

/// What answers the Contract's observations. The validator never learns
/// which: it only sees the API's relay.
enum Candidate {
    /// A static materialization, served by the API itself.
    Static { root: PathBuf, entry: String },
    /// A process the Hosted Run started; the API relays to its ready URL.
    Process { child: Child, ready_url: String },
    HostedProcess {
        _owned: ato_runtime_attempt::launch::process_executor::LaunchedProcess,
        ready_url: String,
    },
}

impl Candidate {
    fn ready_url(&self) -> Option<&str> {
        match self {
            Self::Static { .. } => None,
            Self::Process { ready_url, .. } | Self::HostedProcess { ready_url, .. } => {
                Some(ready_url)
            }
        }
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        if let Self::Process { child, .. } = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Start `python3 -m http.server` over `root`, as the selected LocalProcess
/// Derivation does, and wait until it accepts connections.
fn start_process(root: &Path) -> Candidate {
    let port = {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    if let Some(shim) = std::env::var_os("ATO_HOSTED_PROCESS_SHIM") {
        use ato_ipc::runtime_launch::*;
        use ato_runtime_attempt::launch::{process_executor::*, resolved::*};
        let mut spec = RuntimeLaunchSpecV1::parse(include_str!(
            "../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/fastapi-process.json"
        ))
        .unwrap();
        spec.realization = LaunchRealizationV1::Process(ProcessRealizationV1 {
            argv: vec![
                "python3".into(),
                "-m".into(),
                "http.server".into(),
                port.to_string(),
                "--bind".into(),
                "127.0.0.1".into(),
            ],
            executable: None,
        });
        spec.secret_grants.clear();
        spec.state_attachments.clear();
        spec.readiness = ReadinessV1::Http {
            endpoint_name: "web".into(),
            path: "/".into(),
            timeout_ms: 10_000,
        };
        let endpoint = EndpointV1 {
            name: "web".into(),
            protocol: "http".into(),
            guest_port: Some(port),
            allocation: EndpointAllocationV1::Automatic,
            preferred_port: None,
        };
        spec.endpoints = vec![endpoint.clone()];
        let context = ResolvedRuntimeLaunchContext::new(
            root.to_path_buf(),
            "",
            BTreeMap::new(),
            vec![],
            vec![],
            vec![allocate_endpoint(&endpoint, port)],
        )
        .unwrap();
        let mut owned = launch_process_with(
            &spec,
            &context,
            &ProcessLaunchHost {
                shim: shim.into(),
                runtime_root: root.parent().unwrap().join("process-runtime"),
                output: None,
            },
        )
        .unwrap();
        wait_until_ready(
            &spec,
            &context,
            &mut owned,
            &LoopbackReadinessProbe::new(reqwest::blocking::Client::new()),
        )
        .unwrap();
        return Candidate::HostedProcess {
            _owned: owned,
            ready_url: format!("http://127.0.0.1:{port}/"),
        };
    }
    let child = Command::new("python3")
        .args([
            "-m",
            "http.server",
            &port.to_string(),
            "--bind",
            "127.0.0.1",
        ])
        .arg("--directory")
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("python3 runs the LocalProcess candidate");
    let deadline = Instant::now() + Duration::from_secs(20);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "the candidate never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    Candidate::Process {
        child,
        ready_url: format!("http://127.0.0.1:{port}/"),
    }
}

// ── the Hosted-like API ─────────────────────────────────────────────────────

struct HostedJob {
    job: Value,
    bundle: Vec<u8>,
    surface_port: String,
    requirement_ids: Vec<String>,
    route_realization: String,
    contract_ref: String,
}

#[derive(Default)]
struct Recorded {
    claimed: bool,
    observations: Vec<(String, String, String)>,
    ack: Option<Value>,
    verification_status: Option<String>,
}

struct HostedApi {
    base_url: String,
    stopped: Arc<AtomicBool>,
    recorded: Arc<Mutex<Recorded>>,
    thread: Option<std::thread::JoinHandle<()>>,
    port: u16,
}

impl Drop for HostedApi {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl HostedApi {
    fn start(job: HostedJob, candidate: Arc<Candidate>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stopped = Arc::new(AtomicBool::new(false));
        let recorded = Arc::new(Mutex::new(Recorded::default()));
        let thread = {
            let stopped = stopped.clone();
            let recorded = recorded.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let Some(request) = read_request(&mut stream) else {
                        continue;
                    };
                    let (status, content_type, body) = route(&request, &job, &candidate, &recorded);
                    respond(&mut stream, status, content_type, &body);
                }
            })
        };
        Self {
            base_url: format!("http://localhost:{port}"),
            stopped,
            recorded,
            thread: Some(thread),
            port,
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), query),
        None => (target.clone(), ""),
    };
    let query = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (percent_decode(name), percent_decode(value)))
        .collect();
    Some(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap();
                decoded.push(u8::from_str_radix(hex, 16).unwrap());
                index += 3;
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).unwrap()
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Fixture\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn json_body(value: Value) -> (u16, &'static str, Vec<u8>) {
    (200, "application/json", serde_json::to_vec(&value).unwrap())
}

fn route(
    request: &Request,
    hosted: &HostedJob,
    candidate: &Candidate,
    recorded: &Mutex<Recorded>,
) -> (u16, &'static str, Vec<u8>) {
    let job_id = hosted.job["job_id"].as_str().unwrap();
    let claim_id = hosted.job["claim_id"].as_str().unwrap();
    let job_path = format!("{RUNTIME_JOBS}/{job_id}");
    let fenced = || {
        request
            .headers
            .get("x-ato-validation-claim-id")
            .map(String::as_str)
            != Some(claim_id)
    };
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/v1/capsule-bundles/validation-jobs/claim")
        | ("POST", "/v1/portable-application-exports/jobs/claim") => {
            (204, "text/plain", Vec::new())
        }
        ("POST", path) if path == format!("{RUNTIME_JOBS}/claim") => {
            let mut recorded = recorded.lock().unwrap();
            if recorded.claimed {
                return (204, "text/plain", Vec::new());
            }
            recorded.claimed = true;
            json_body(json!({ "job": hosted.job }))
        }
        ("GET", path) if path == format!("{job_path}/content") => {
            if fenced() {
                return (409, "application/json", br#"{"error":"fenced"}"#.to_vec());
            }
            (200, "application/vnd.ato.capsule", hosted.bundle.clone())
        }
        ("GET", path) if path == format!("{job_path}/observe") => {
            if fenced() {
                return (409, "application/json", br#"{"error":"fenced"}"#.to_vec());
            }
            let method = request
                .query
                .get("method")
                .cloned()
                .unwrap_or_else(|| "GET".to_owned())
                .to_ascii_uppercase();
            let port = request.query.get("port").cloned().unwrap_or_default();
            let path = request
                .query
                .get("path")
                .cloned()
                .unwrap_or_else(|| "/".to_owned());
            recorded.lock().unwrap().observations.push((
                port.clone(),
                method.clone(),
                path.clone(),
            ));
            if method != "GET" || port != hosted.surface_port {
                return (
                    422,
                    "application/json",
                    br#"{"error":"observation_out_of_profile"}"#.to_vec(),
                );
            }
            let (status, body) = match candidate {
                Candidate::Static { root, entry } => {
                    let file = if path == "/" {
                        entry.clone()
                    } else {
                        path.trim_start_matches('/').to_owned()
                    };
                    match std::fs::read(root.join(file)) {
                        Ok(body) => (200, body),
                        Err(_) => {
                            return (
                                422,
                                "application/json",
                                br#"{"error":"observation_out_of_profile"}"#.to_vec(),
                            );
                        }
                    }
                }
                Candidate::Process { ready_url, .. }
                | Candidate::HostedProcess { ready_url, .. } => {
                    let client = reqwest::blocking::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .timeout(Duration::from_secs(5))
                        .build()
                        .unwrap();
                    let target = format!("{}{}", ready_url.trim_end_matches('/'), path);
                    let response = client.get(target).send().unwrap();
                    let status = response.status().as_u16();
                    (status, response.bytes().unwrap().to_vec())
                }
            };
            json_body(json!({
                "port": port,
                "method": method,
                "path": path,
                "status": status,
                "body_base64": base64::engine::general_purpose::STANDARD.encode(body),
                "instance_id": "ins_fixture",
            }))
        }
        ("POST", path) if path == format!("{job_path}/ack") => {
            if fenced() {
                return (409, "application/json", br#"{"error":"fenced"}"#.to_vec());
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let status = hosted_verification_status(hosted, &body["receipt"]);
            let mut recorded = recorded.lock().unwrap();
            recorded.ack = Some(body);
            recorded.verification_status = Some(status.clone());
            json_body(json!({ "verification_status": status }))
        }
        _ => (
            404,
            "application/json",
            br#"{"error":"not_found"}"#.to_vec(),
        ),
    }
}

/// How the Hosted API decides an acked receipt: the transport, K and the
/// selected D it expects, K's observation set, this job's Run identity, and
/// every observation satisfied.
fn hosted_verification_status(hosted: &HostedJob, receipt: &Value) -> String {
    let job = &hosted.job;
    let mut observed = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    observed.sort();
    let mut expected = hosted.requirement_ids.clone();
    expected.sort();
    let execution = &receipt["execution"];
    let execution_matches = job["run_id"].is_null()
        || (execution["run_id"] == job["run_id"]
            && execution["lease_id"] == job["lease_id"]
            && execution["attempt_id"] == job["job_id"]
            && execution["realization"] == hosted.route_realization.as_str());
    let verified = receipt["bundle_sha256"] == job["transport_digest"]
        && receipt["contract_ref"] == hosted.contract_ref.as_str()
        && receipt["derivation_ref"] == job["selected_derivation_ref"]
        && observed == expected
        && execution_matches
        && receipt["fully_satisfied"] == true
        && receipt["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|observation| observation["outcome"] == "satisfied");
    if verified { "verified" } else { "failed" }.to_owned()
}

// ── fixtures ────────────────────────────────────────────────────────────────

struct Fixture {
    bytes: Vec<u8>,
    bundle: PortableApplicationBundle,
    route: ValidatedPortableApplication,
}

fn static_fixture() -> Fixture {
    let (bytes, bundle) =
        build_static_bundle(&samples().join("interop-static-k"), "Ato portability proof").unwrap();
    fixture(bytes, bundle, PortableRealizationKind::StaticWeb)
}

fn multi_fixture(realization: PortableRealizationKind) -> Fixture {
    let (bytes, bundle) = build_multi_derivation_bundle(
        &samples().join("interop-multi-derivation"),
        "Ato portability proof",
    )
    .unwrap();
    fixture(bytes, bundle, realization)
}

/// A v4 static bundle whose K binds a portable Instance snapshot.
fn snapshot_fixture() -> Fixture {
    let (_, original) =
        build_static_bundle(&samples().join("interop-static-k"), "Ato portability proof").unwrap();
    let (_, original) = repack_portable_dependencies(
        &original,
        PortableDependencyProfile::Cached,
        &BTreeMap::new(),
    )
    .unwrap();
    let resource = br#"{"todos":["one","two"]}"#.to_vec();
    let asset = b"portable-photo-bytes".to_vec();
    let resource_ref = bundle_sha256(&resource);
    let asset_ref = bundle_sha256(&asset);
    let snapshot = InstanceSnapshotV1 {
        schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
        resources: vec![InstanceSnapshotResourceV1 {
            slot: "main".to_owned(),
            protocol: DATA_JSON_PROTOCOL.to_owned(),
            content_ref: resource_ref.clone(),
        }],
        assets: vec![InstanceSnapshotAssetV1 {
            alias: "asset-1".to_owned(),
            content_ref: asset_ref.clone(),
            filename: "photo.jpg".to_owned(),
            content_type: "image/jpeg".to_owned(),
            size: asset.len() as u64,
        }],
        asset_bindings: vec![],
    };
    let (bytes, bundle) = attach_instance_snapshot(
        &original,
        snapshot,
        &BTreeMap::from([(resource_ref, resource), (asset_ref, asset)]),
    )
    .unwrap();
    fixture(bytes, bundle, PortableRealizationKind::StaticWeb)
}

fn fixture(
    bytes: Vec<u8>,
    bundle: PortableApplicationBundle,
    realization: PortableRealizationKind,
) -> Fixture {
    let route = validate_bytes_all(&bytes)
        .unwrap()
        .1
        .into_iter()
        .find(|route| route.realization == realization)
        .expect("the bundle declares the route");
    Fixture {
        bytes,
        bundle,
        route,
    }
}

fn realization_name(kind: PortableRealizationKind) -> &'static str {
    match kind {
        PortableRealizationKind::StaticWeb => "static_web",
        PortableRealizationKind::LocalProcess => "local_process",
        PortableRealizationKind::OciContainer => "oci_container",
        PortableRealizationKind::OciServiceGroup => "oci_service_group",
    }
}

/// How the Run's served workspace differs from the bundle's tree.
enum Served {
    AsBundled,
    Without(&'static str),
    Rewritten(&'static str, &'static [u8]),
}

/// What the API tells the validator about the job, besides the bundle.
struct JobShape {
    selected_derivation_ref: Option<String>,
    instance_snapshot_ref: Option<String>,
    transport_digest: Option<String>,
}

impl JobShape {
    fn of(fixture: &Fixture) -> Self {
        Self {
            selected_derivation_ref: Some(fixture.route.derivation_ref.to_string()),
            instance_snapshot_ref: None,
            transport_digest: None,
        }
    }
}

struct CaseResult {
    outcome: Result<ValidatorRunOutcome, String>,
    receipt: Option<Value>,
    verification_status: Option<String>,
    observations: Vec<(String, String, String)>,
    endpoint: Option<String>,
}

fn run_case(name: &str, fixture: &Fixture, served: Served, shape: JobShape) -> CaseResult {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("workspace");
    materialize_tree(&fixture.bundle, &fixture.route, &root).unwrap();
    match served {
        Served::AsBundled => {}
        Served::Without(file) => std::fs::remove_file(root.join(file)).unwrap(),
        Served::Rewritten(file, body) => std::fs::write(root.join(file), body).unwrap(),
    }
    let realization = realization_name(fixture.route.realization);
    let candidate = Arc::new(match fixture.route.realization {
        PortableRealizationKind::StaticWeb => Candidate::Static {
            root: root.clone(),
            entry: fixture.route.application.surfaces[0]
                .entry
                .clone()
                .unwrap_or_else(|| "index.html".to_owned()),
        },
        _ => start_process(&root),
    });
    let job_id = format!("pvj_{name}");
    let process = candidate.ready_url().is_some();
    let job = json!({
        "job_id": job_id,
        "claim_id": format!("claim_{name}"),
        "claim_expires_at": "2026-09-25T00:10:00.000Z",
        "bundle_id": format!("bnd_{name}"),
        "transport_digest": shape
            .transport_digest
            .unwrap_or_else(|| bundle_sha256(&fixture.bytes)),
        "size_bytes": fixture.bytes.len(),
        "selected_derivation_ref": shape.selected_derivation_ref,
        "run_id": process.then(|| format!("run_{name}")),
        "lease_id": process.then(|| format!("lease_{name}")),
        "attempt_id": job_id,
        "execution_id": process.then(|| format!("exe_{name}")),
        "realization": realization,
        "endpoint": candidate.ready_url(),
        "execution_evidence": process.then(|| json!({
            "realization": realization,
            "runtime_executable": "python3",
            "runtime_version": "3.12",
            "platform": "linux/amd64",
        })),
        "instance_snapshot_ref": shape.instance_snapshot_ref,
        "download_url": format!("{RUNTIME_JOBS}/{job_id}/content"),
        "observe_url": format!("{RUNTIME_JOBS}/{job_id}/observe"),
    });
    let endpoint = candidate.ready_url().map(str::to_owned);
    let api = HostedApi::start(
        HostedJob {
            job,
            bundle: fixture.bytes.clone(),
            surface_port: fixture.route.application.surfaces[0].port.clone(),
            requirement_ids: fixture
                .route
                .contract
                .requirements
                .iter()
                .map(|requirement| requirement.id.clone())
                .collect(),
            route_realization: realization.to_owned(),
            contract_ref: fixture.route.contract_ref.to_string(),
        },
        candidate.clone(),
    );
    let agent = ValidatorAgent::new(ValidatorAgentConfig {
        api_url: api.base_url.clone(),
        token: "fixture-token".to_owned(),
        agent_id: "fixture-validator".to_owned(),
        work_root: scratch.path().join("validator"),
        poll_interval: Duration::from_millis(10),
    })
    .unwrap();
    let outcome = agent.run_once().map_err(|error| format!("{error:#}"));
    let recorded = std::mem::take(&mut *api.recorded.lock().unwrap());
    let result = CaseResult {
        outcome,
        receipt: recorded.ack.map(|ack| ack["receipt"].clone()),
        verification_status: recorded.verification_status,
        observations: recorded.observations,
        endpoint,
    };
    write_normalized(name, &result);
    result
}

/// The case as a comparable document: volatile endpoints replaced.
fn write_normalized(name: &str, result: &CaseResult) {
    let Some(dir) = std::env::var_os("ATO_HOSTED_VALIDATOR_FIXTURE_OUT") else {
        return;
    };
    let mut receipt = result.receipt.clone();
    if let (Some(receipt), Some(endpoint)) = (receipt.as_mut(), result.endpoint.as_deref())
        && receipt["execution"]["endpoint"] == endpoint
    {
        receipt["execution"]["endpoint"] = json!("<candidate endpoint>");
    }
    let document = json!({
        "outcome": match &result.outcome {
            Ok(outcome) => json!(format!("{outcome:?}")),
            Err(error) => json!({ "error": error }),
        },
        "verification_status": result.verification_status,
        "observations": result.observations,
        "receipt": receipt,
        "receipt_canonical_sha256": result.receipt.as_ref().map(|receipt| {
            let mut receipt = receipt.clone();
            if receipt["execution"]["endpoint"].is_string() {
                receipt["execution"]["endpoint"] = json!("<candidate endpoint>");
            }
            bundle_sha256(&serde_jcs::to_vec(&receipt).unwrap())
        }),
    });
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&document).unwrap(),
    )
    .unwrap();
}

// ── assertions shared by every acked case ───────────────────────────────────

fn outcome_of(receipt: &Value, id: &str) -> (String, Value) {
    let observation = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|observation| observation["id"] == id)
        .unwrap_or_else(|| panic!("the receipt names observation {id}"));
    (
        observation["outcome"].as_str().unwrap().to_owned(),
        observation.clone(),
    )
}

/// The receipt is the Hosted one: this transport, the bundle's K, the selected
/// D, the Hosted target and schema, the Hosted Run's identity, and nothing the
/// Hosted API's strict receipt schema would refuse.
fn assert_hosted_receipt(fixture: &Fixture, result: &CaseResult, name: &str) -> Value {
    let receipt = result
        .receipt
        .clone()
        .expect("the validator acked a receipt");
    let keys = receipt
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert!(
        keys.is_subset(&HOSTED_RECEIPT_KEYS.iter().copied().collect()),
        "{keys:?}"
    );
    let execution_keys = receipt["execution"]
        .as_object()
        .expect("the Hosted receipt carries execution evidence")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert!(
        execution_keys.is_subset(&HOSTED_EXECUTION_KEYS.iter().copied().collect()),
        "{execution_keys:?}"
    );
    assert_eq!(receipt["schema"], "ato.contract-verification-receipt/1");
    assert_eq!(receipt["target"], json!({ "kind": "ato-run-hosted" }));
    assert_eq!(receipt["bundle_sha256"], bundle_sha256(&fixture.bytes));
    assert_eq!(receipt["contract_ref"], fixture.route.contract_ref.as_str());
    assert_eq!(
        receipt["derivation_ref"],
        fixture.route.derivation_ref.as_str()
    );
    assert_eq!(
        receipt["execution"]["realization"],
        realization_name(fixture.route.realization)
    );
    assert_eq!(receipt["execution"]["attempt_id"], format!("pvj_{name}"));
    if fixture.route.realization == PortableRealizationKind::StaticWeb {
        for absent in ["run_id", "lease_id", "endpoint"] {
            assert!(receipt["execution"][absent].is_null(), "{absent}");
        }
    } else {
        assert_eq!(receipt["execution"]["run_id"], format!("run_{name}"));
        assert_eq!(receipt["execution"]["lease_id"], format!("lease_{name}"));
        assert_eq!(
            receipt["execution"]["endpoint"].as_str(),
            result.endpoint.as_deref()
        );
        assert_eq!(receipt["execution"]["runtime_executable"], "python3");
    }
    // K's observations, in K's order, each decided once.
    let ids = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected = fixture
        .route
        .contract
        .requirements
        .iter()
        .map(|requirement| requirement.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, expected);
    let all_satisfied = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|observation| observation["outcome"] == "satisfied");
    assert_eq!(receipt["fully_satisfied"], all_satisfied);
    // Only K's HTTP observations were asked of the Run, and only as GET.
    let asked = result
        .observations
        .iter()
        .map(|(port, method, path)| (port.as_str(), method.as_str(), path.as_str()))
        .collect::<Vec<_>>();
    let required = fixture
        .route
        .contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == "ato.contract.http@1")
        .map(|requirement| {
            (
                requirement.port.as_deref().unwrap(),
                "GET",
                requirement.path.as_deref().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(asked, required);
    receipt
}

fn assert_verified(fixture: &Fixture, result: &CaseResult, name: &str) -> Value {
    let receipt = assert_hosted_receipt(fixture, result, name);
    assert_eq!(
        result.outcome,
        Ok(ValidatorRunOutcome::HostedVerified {
            bundle_id: format!("bnd_{name}"),
            fully_satisfied: true,
        })
    );
    assert_eq!(receipt["fully_satisfied"], true);
    assert_eq!(result.verification_status.as_deref(), Some("verified"));
    receipt
}

fn assert_failed(fixture: &Fixture, result: &CaseResult, name: &str) -> Value {
    let receipt = assert_hosted_receipt(fixture, result, name);
    assert_eq!(
        result.outcome,
        Ok(ValidatorRunOutcome::HostedVerified {
            bundle_id: format!("bnd_{name}"),
            fully_satisfied: false,
        })
    );
    assert_eq!(receipt["fully_satisfied"], false);
    assert_eq!(result.verification_status.as_deref(), Some("failed"));
    receipt
}

fn assert_refused(result: &CaseResult, message: &str) {
    let error = result.outcome.as_ref().expect_err("the job is refused");
    assert!(error.contains(message), "{error}");
    assert!(result.receipt.is_none(), "a refused job acks no receipt");
    assert!(
        result.observations.is_empty(),
        "a refused job observes nothing"
    );
}

// ── A–H ─────────────────────────────────────────────────────────────────────

#[test]
fn a_static_hosted_observation_is_verified() {
    let fixture = static_fixture();
    let result = run_case(
        "a_static",
        &fixture,
        Served::AsBundled,
        JobShape::of(&fixture),
    );
    let receipt = assert_verified(&fixture, &result, "a_static");
    let (_, proof) = outcome_of(&receipt, "app-proof");
    assert_eq!(proof["evidence"]["status"], 200);
    assert_eq!(
        proof["evidence"]["body_sha256"],
        bundle_sha256(&std::fs::read(samples().join("interop-static-k/proof.txt")).unwrap())
    );
}

#[test]
fn b_process_hosted_observation_is_verified() {
    let fixture = multi_fixture(PortableRealizationKind::LocalProcess);
    let result = run_case(
        "b_process",
        &fixture,
        Served::AsBundled,
        JobShape::of(&fixture),
    );
    let receipt = assert_verified(&fixture, &result, "b_process");
    assert!(receipt["execution"]["request_id"].is_null());
    assert_eq!(outcome_of(&receipt, "app-root").0, "satisfied");
}

#[test]
fn c_http_status_mismatch_fails_the_observation() {
    let fixture = multi_fixture(PortableRealizationKind::LocalProcess);
    let result = run_case(
        "c_status",
        &fixture,
        Served::Without("proof.txt"),
        JobShape::of(&fixture),
    );
    let receipt = assert_failed(&fixture, &result, "c_status");
    let (outcome, proof) = outcome_of(&receipt, "app-proof");
    assert_eq!(outcome, "failed");
    assert_eq!(proof["evidence"]["status"], 404);
    assert!(
        proof["failure"]
            .as_str()
            .unwrap()
            .starts_with("http_status_mismatch:")
    );
    assert_eq!(outcome_of(&receipt, "app-root").0, "satisfied");
}

#[test]
fn d_body_digest_mismatch_fails_the_observation() {
    let fixture = multi_fixture(PortableRealizationKind::LocalProcess);
    let result = run_case(
        "d_body",
        &fixture,
        Served::Rewritten("proof.txt", b"not the bundled proof\n"),
        JobShape::of(&fixture),
    );
    let receipt = assert_failed(&fixture, &result, "d_body");
    let (outcome, proof) = outcome_of(&receipt, "app-proof");
    assert_eq!(outcome, "failed");
    assert_eq!(proof["evidence"]["status"], 200);
    assert_eq!(
        proof["evidence"]["body_sha256"],
        bundle_sha256(b"not the bundled proof\n")
    );
    assert!(
        proof["failure"]
            .as_str()
            .unwrap()
            .starts_with("http_body_digest_mismatch:")
    );
}

#[test]
fn e_workspace_identity_is_the_bundled_tree() {
    // The multi-derivation bundle's static route: same K, other D.
    let fixture = multi_fixture(PortableRealizationKind::StaticWeb);
    let result = run_case(
        "e_workspace",
        &fixture,
        Served::AsBundled,
        JobShape::of(&fixture),
    );
    let receipt = assert_verified(&fixture, &result, "e_workspace");
    let (outcome, identity) = outcome_of(&receipt, "source-identity");
    assert_eq!(outcome, "satisfied");
    assert_eq!(
        identity["evidence"],
        json!({ "input": "workspace", "digest": fixture.route.tree_ref.as_str() })
    );
}

#[test]
fn f1_restored_instance_snapshot_is_verified() {
    let fixture = snapshot_fixture();
    let snapshot_ref = fixture.route.instance_snapshot_ref.clone().unwrap();
    let result = run_case(
        "f1_snapshot_restored",
        &fixture,
        Served::AsBundled,
        JobShape {
            instance_snapshot_ref: Some(snapshot_ref.to_string()),
            ..JobShape::of(&fixture)
        },
    );
    let receipt = assert_verified(&fixture, &result, "f1_snapshot_restored");
    let snapshot = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|observation| observation["evidence"]["digest"] == snapshot_ref.as_str())
        .expect("the snapshot observation names what was restored");
    assert_eq!(snapshot["outcome"], "satisfied");
}

#[test]
fn f2_unrestored_instance_snapshot_fails() {
    let fixture = snapshot_fixture();
    let result = run_case(
        "f2_snapshot_missing",
        &fixture,
        Served::AsBundled,
        JobShape::of(&fixture),
    );
    let receipt = assert_failed(&fixture, &result, "f2_snapshot_missing");
    let failures = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|observation| observation["failure"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].starts_with("instance_snapshot_missing:"));
}

#[test]
fn f3_another_restored_snapshot_fails() {
    let fixture = snapshot_fixture();
    let other = bundle_sha256(b"another Instance's saved state");
    let result = run_case(
        "f3_snapshot_other",
        &fixture,
        Served::AsBundled,
        JobShape {
            instance_snapshot_ref: Some(other.clone()),
            ..JobShape::of(&fixture)
        },
    );
    let receipt = assert_failed(&fixture, &result, "f3_snapshot_other");
    let failed = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|observation| observation["outcome"] == "failed")
        .unwrap();
    assert!(
        failed["failure"]
            .as_str()
            .unwrap()
            .starts_with("instance_snapshot_mismatch:")
    );
    assert_eq!(failed["evidence"]["digest"], other.as_str());
}

#[test]
fn g1_a_derivation_the_bundle_does_not_declare_is_refused() {
    let fixture = multi_fixture(PortableRealizationKind::LocalProcess);
    let result = run_case(
        "g1_wrong_derivation",
        &fixture,
        Served::AsBundled,
        JobShape {
            selected_derivation_ref: Some(bundle_sha256(b"a Derivation from elsewhere")),
            ..JobShape::of(&fixture)
        },
    );
    assert_refused(&result, "is not declared by this bundle");
}

#[test]
fn g2_a_job_without_a_selected_derivation_is_refused() {
    let fixture = multi_fixture(PortableRealizationKind::LocalProcess);
    let result = run_case(
        "g2_no_derivation",
        &fixture,
        Served::AsBundled,
        JobShape {
            selected_derivation_ref: None,
            ..JobShape::of(&fixture)
        },
    );
    assert_refused(&result, "runtime job omitted selected_derivation_ref");
}

#[test]
fn h_a_transport_that_is_not_the_claimed_bundle_is_refused() {
    let fixture = static_fixture();
    let result = run_case(
        "h_transport",
        &fixture,
        Served::AsBundled,
        JobShape {
            transport_digest: Some(bundle_sha256(b"other bytes")),
            ..JobShape::of(&fixture)
        },
    );
    assert_refused(&result, "bundle transport digest mismatch");
}
