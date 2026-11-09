//! `ato hub push` / `ato hub pull` integration tests.
//!
//! Each test runs the real `ato` binary against an in-process HTTP hub on a
//! loopback port. The stub hub is intentionally dumb: it stores ciphertext by
//! id, can serve a tampered byte, and records every request so the tests can
//! assert that the link fragment (the key) never reaches the network and that
//! redirects are not followed.
//!
//! Capsules are built at test time through the portable-application builders
//! (v3) and the snapshot attach path (v4), so no binary fixtures are added.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use ato_objects::PortableDependencyProfile;
use ato_objects::capsulehub::{HubKey, SharedLink};
use ato_portable_application::build_static_bundle;
use ato_portable_application::bundle_sha256;
use ato_portable_application::instance_snapshot::{
    DATA_JSON_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotAssetV1,
    InstanceSnapshotResourceV1, InstanceSnapshotV1, attach_instance_snapshot,
};
use ato_portable_application::portability_export::repack_portable_dependencies;
use sha2::{Digest, Sha256};

const PUT_TOKEN: &str = "hub-upload-token";

fn ato() -> AssertCommand {
    let mut command =
        AssertCommand::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.timeout(Duration::from_secs(60));
    // A hub URL must never come from the ambient environment in these tests.
    command.env_remove("ATO_HUB_URL");
    command.env_remove("ATO_HUB_TOKEN");
    command
}

fn sample_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k")
}

fn v3_bundle(directory: &Path) -> PathBuf {
    let (bytes, _) = build_static_bundle(&sample_source(), "Capsulehub round trip")
        .expect("the static sample packs");
    let path = directory.join("round-trip.capsule");
    fs::write(&path, bytes).expect("write the v3 bundle");
    path
}

fn v4_bundle(directory: &Path) -> PathBuf {
    let (_, bundle) = build_static_bundle(&sample_source(), "Capsulehub snapshot")
        .expect("the static sample packs");
    let (_, repacked) =
        repack_portable_dependencies(&bundle, PortableDependencyProfile::Cached, &BTreeMap::new())
            .expect("the bundle repacks as v4");
    let saved_data = br#"{"todos":["hub","pull"]}"#.to_vec();
    let asset = b"capsulehub-snapshot-asset".to_vec();
    let saved_data_ref = bundle_sha256(&saved_data);
    let asset_ref = bundle_sha256(&asset);
    let snapshot = InstanceSnapshotV1 {
        schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
        resources: vec![InstanceSnapshotResourceV1 {
            slot: "main".to_owned(),
            protocol: DATA_JSON_PROTOCOL.to_owned(),
            content_ref: saved_data_ref.clone(),
            save: None,
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
    let content = BTreeMap::from([(saved_data_ref, saved_data), (asset_ref, asset)]);
    let (bytes, _) = attach_instance_snapshot(&repacked, snapshot, &content)
        .expect("the snapshot binds into v4");
    let path = directory.join("snapshot.capsule");
    fs::write(&path, bytes).expect("write the v4 bundle");
    path
}

fn v2_bundle(directory: &Path) -> PathBuf {
    // The v2 reader accepts this minimal envelope; hub push must reject it by
    // identity domain before any validation or upload.
    let bytes = format!(
        "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
        "0".repeat(64)
    );
    let path = directory.join("computation-v2.capsule");
    fs::write(&path, bytes).expect("write the v2 bundle");
    path
}

fn unknown_profile_bundle(directory: &Path, v3: &Path) -> PathBuf {
    let bytes = fs::read(v3).expect("read the v3 bundle");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
    value["index"]["profile"] =
        serde_json::Value::String("ato.portable-application/999".to_owned());
    let path = directory.join("unknown-profile.capsule");
    fs::write(&path, serde_json::to_vec(&value).expect("serialize")).expect("write the bundle");
    path
}

fn tampered_bundle(directory: &Path, v3: &Path) -> PathBuf {
    let mut bytes = fs::read(v3).expect("read the v3 bundle");
    let needle = b"\"root_contract_ref\":\"sha256:";
    let position = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("v3 bundles declare root_contract_ref");
    let digit = position + needle.len();
    bytes[digit] = if bytes[digit] == b'a' { b'b' } else { b'a' };
    let path = directory.join("tampered.capsule");
    fs::write(&path, bytes).expect("write the tampered bundle");
    path
}

// ---------------------------------------------------------------------------
// Minimal recording HTTP hub
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct RecordedRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body_len: usize,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone)]
enum GetBehavior {
    Serve,
    Redirect(String),
    Corrupt,
    Missing,
}

struct HubState {
    objects: HashMap<String, Vec<u8>>,
    requests: Vec<RecordedRequest>,
    get_behavior: GetBehavior,
    put_token: Option<String>,
}

struct TestHub {
    address: SocketAddr,
    state: Arc<Mutex<HubState>>,
}

impl TestHub {
    fn start(put_token: Option<&str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let address = listener.local_addr().expect("bound address");
        let state = Arc::new(Mutex::new(HubState {
            objects: HashMap::new(),
            requests: Vec::new(),
            get_behavior: GetBehavior::Serve,
            put_token: put_token.map(str::to_owned),
        }));
        let shared = state.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let shared = shared.clone();
                thread::spawn(move || handle_connection(stream, shared));
            }
        });
        Self { address, state }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn set_get_behavior(&self, behavior: GetBehavior) {
        self.state.lock().unwrap().get_behavior = behavior;
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    fn stored(&self, id: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().objects.get(id).cloned()
    }
}

fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<HubState>>) {
    let mut raw = Vec::new();
    let mut buffer = [0u8; 8192];
    let header_end = loop {
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => raw.extend_from_slice(&buffer[..read]),
            Err(_) => return,
        }
        if let Some(position) = find(&raw, b"\r\n\r\n") {
            break position;
        }
        if raw.len() > 64 * 1024 {
            return;
        }
    };
    let head = String::from_utf8_lossy(&raw[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(": ") else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        headers.push((name.to_ascii_lowercase(), value.to_owned()));
    }
    let mut body = raw[header_end + 4..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => body.extend_from_slice(&buffer[..read]),
            Err(_) => break,
        }
    }
    body.truncate(content_length);

    let (status, response_headers, response_body) = {
        let mut state = state.lock().unwrap();
        state.requests.push(RecordedRequest {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
            body_len: body.len(),
        });
        route(&mut state, &method, &target, &headers, &body)
    };
    write_response(&mut stream, status, &response_headers, &response_body);
}

fn route(
    state: &mut HubState,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let Some(id) = target.strip_prefix("/v1/capsulehub/objects/") else {
        return (404, Vec::new(), b"not found".to_vec());
    };
    let id = id.to_owned();
    match method {
        "PUT" => {
            if let Some(expected) = state.put_token.as_deref() {
                let presented = headers
                    .iter()
                    .find(|(name, _)| name == "authorization")
                    .map(|(_, value)| value.as_str());
                if presented != Some(&format!("Bearer {expected}")) {
                    return (401, Vec::new(), b"unauthorized".to_vec());
                }
            }
            let actual = format!("sha256:{:x}", Sha256::digest(body));
            if actual != id {
                return (400, Vec::new(), b"digest mismatch".to_vec());
            }
            let created = state.objects.insert(id, body.to_vec()).is_none();
            (if created { 201 } else { 200 }, Vec::new(), Vec::new())
        }
        "GET" => match state.get_behavior.clone() {
            GetBehavior::Redirect(location) => {
                (302, vec![("location".to_owned(), location)], Vec::new())
            }
            GetBehavior::Corrupt => match state.objects.get(&id) {
                Some(bytes) => {
                    let mut corrupted = bytes.clone();
                    let last = corrupted.len() - 1;
                    corrupted[last] ^= 0x80;
                    (200, Vec::new(), corrupted)
                }
                None => (404, Vec::new(), b"not found".to_vec()),
            },
            GetBehavior::Missing => (404, Vec::new(), b"not found".to_vec()),
            GetBehavior::Serve => match state.objects.get(&id) {
                Some(bytes) => (200, Vec::new(), bytes.clone()),
                None => (404, Vec::new(), b"not found".to_vec()),
            },
        },
        _ => (405, Vec::new(), b"method not allowed".to_vec()),
    }
}

fn write_response(stream: &mut TcpStream, status: u16, headers: &[(String, String)], body: &[u8]) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// CLI helpers
// ---------------------------------------------------------------------------

fn push(command: &mut AssertCommand, capsule: &Path, hub: &TestHub) -> Output {
    command
        .args(["hub", "push"])
        .arg(capsule)
        .args(["--hub-url", &hub.base_url()])
        .args(["--allow-loopback-http", "--token", PUT_TOKEN])
        .output()
        .expect("ato hub push runs")
}

fn push_without_token(command: &mut AssertCommand, capsule: &Path, hub: &TestHub) -> Output {
    command
        .args(["hub", "push"])
        .arg(capsule)
        .args(["--hub-url", &hub.base_url()])
        .args(["--allow-loopback-http"])
        .output()
        .expect("ato hub push runs")
}

fn pull(command: &mut AssertCommand, link: &str, output: &Path, force: bool) -> Output {
    let builder = command.args(["hub", "pull", link]);
    builder
        .arg("--output")
        .arg(output)
        .arg("--allow-loopback-http");
    if force {
        builder.arg("--force");
    }
    builder.output().expect("ato hub pull runs")
}

fn link_from(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .unwrap_or_else(|| panic!("push must print a shared link, got: {stdout}"))
        .to_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command must succeed: {}",
        stderr(output)
    );
}

fn read_link(link: &str) -> SharedLink {
    SharedLink::parse(link).expect("the produced link parses")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn push_then_pull_round_trips_the_exact_bundle_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let original = fs::read(&capsule).unwrap();
    let hub = TestHub::start(Some(PUT_TOKEN));

    let pushed = push(&mut ato(), &capsule, &hub);
    assert_success(&pushed);
    let link = link_from(&pushed);
    let parsed = read_link(&link);

    // The upload carried only ciphertext addressed by its own digest, with the
    // upload token, and never the fragment key.
    let requests = hub.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer hub-upload-token")
    );
    assert_eq!(
        requests[0].header("content-type"),
        Some("application/octet-stream")
    );
    let key_secret = parsed.key.to_fragment_secret();
    assert_eq!(
        requests[0].target,
        format!("/v1/capsulehub/objects/{}", parsed.object_id)
    );
    assert!(!requests[0].target.contains('#'));
    assert!(!requests[0].target.contains(&key_secret));
    assert_eq!(
        requests[0].body_len,
        hub.stored(parsed.object_id.as_str()).unwrap().len()
    );
    let ciphertext = hub.stored(parsed.object_id.as_str()).unwrap();
    assert!(ciphertext.starts_with(b"ATOHUB"));
    assert_ne!(ciphertext, original);

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &link, &output, false);
    assert_success(&pulled);
    assert_eq!(fs::read(&output).unwrap(), original);

    let stdout = String::from_utf8_lossy(&pulled.stdout);
    assert!(
        stdout.contains(&format!("contract_ref={}", parsed.cid)),
        "{stdout}"
    );

    // The download request never carried the key or a fragment either.
    let pulls: Vec<_> = hub
        .requests()
        .into_iter()
        .filter(|request| request.method == "GET")
        .collect();
    assert_eq!(pulls.len(), 1);
    assert_eq!(pulls[0].header("authorization"), None);
    assert_eq!(
        pulls[0].target,
        format!("/v1/capsulehub/objects/{}", parsed.object_id)
    );
    assert!(!pulls[0].target.contains('#'));
    assert!(!pulls[0].target.contains(&key_secret));
}

#[test]
fn pull_rejects_a_tampered_ciphertext_at_the_digest_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));
    let link = link_from(&push(&mut ato(), &capsule, &hub));
    hub.set_get_behavior(GetBehavior::Corrupt);

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &link, &output, false);
    assert!(!pulled.status.success());
    assert!(
        stderr(&pulled).contains("ciphertext digest mismatch"),
        "{}",
        stderr(&pulled)
    );
    assert!(!output.exists(), "a rejected download must write nothing");
}

#[test]
fn pull_rejects_a_wrong_key_at_the_decryption_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));
    let link = link_from(&push(&mut ato(), &capsule, &hub));
    let parsed = read_link(&link);

    let replacement = HubKey::generate().unwrap();
    let wrong_key_link = SharedLink::new(
        &parsed.base_url().unwrap(),
        parsed.object_id.clone(),
        replacement,
        parsed.cid.clone(),
    )
    .unwrap()
    .to_url(&parsed.base_url().unwrap())
    .unwrap();
    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &wrong_key_link, &output, false);
    assert!(!pulled.status.success());
    let message = stderr(&pulled);
    assert!(
        message.contains("decrypt") || message.contains("authentication"),
        "{message}"
    );
    assert!(!output.exists());
    // A key must never be echoed into an error message.
    let replacement_secret = SharedLink::parse(&wrong_key_link)
        .unwrap()
        .key
        .to_fragment_secret();
    assert!(!message.contains(&replacement_secret));
    assert!(!message.contains(&parsed.key.to_fragment_secret()));
}

#[test]
fn pull_rejects_a_wrong_cid_claim_after_validation() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));
    let link = link_from(&push(&mut ato(), &capsule, &hub));
    let parsed = read_link(&link);
    let wrong_cid = format!("sha256:{}", "b".repeat(64));
    let wrong_cid_link = SharedLink::new(
        &parsed.base_url().unwrap(),
        parsed.object_id.clone(),
        HubKey::from_fragment_secret(&parsed.key.to_fragment_secret()).unwrap(),
        wrong_cid,
    )
    .unwrap()
    .to_url(&parsed.base_url().unwrap())
    .unwrap();

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &wrong_cid_link, &output, false);
    assert!(!pulled.status.success());
    assert!(
        stderr(&pulled).contains("capsule identity mismatch"),
        "{}",
        stderr(&pulled)
    );
    assert!(!output.exists());
}

#[test]
fn pull_does_not_follow_redirects() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));
    let link = link_from(&push(&mut ato(), &capsule, &hub));
    let sink = TestHub::start(None);
    hub.set_get_behavior(GetBehavior::Redirect(format!(
        "{}/v1/capsulehub/objects/{}",
        sink.base_url(),
        read_link(&link).object_id
    )));

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &link, &output, false);
    assert!(!pulled.status.success());
    assert!(stderr(&pulled).contains("redirect"), "{}", stderr(&pulled));
    assert!(
        sink.requests().is_empty(),
        "the redirect target must never be contacted"
    );
    assert!(!output.exists());
}

#[test]
fn push_rejects_computation_root_v2_before_uploading() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v2_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));

    let pushed = push(&mut ato(), &capsule, &hub);
    assert!(!pushed.status.success());
    let message = stderr(&pushed);
    assert!(
        message.contains("computation-root wire v2"),
        "the v2 rejection must be named: {message}"
    );
    assert!(hub.requests().is_empty(), "nothing may be uploaded");
}

#[test]
fn push_rejects_an_unknown_profile_before_uploading() {
    let directory = tempfile::tempdir().unwrap();
    let v3 = v3_bundle(directory.path());
    let capsule = unknown_profile_bundle(directory.path(), &v3);
    let hub = TestHub::start(Some(PUT_TOKEN));

    let pushed = push(&mut ato(), &capsule, &hub);
    assert!(!pushed.status.success());
    let message = stderr(&pushed);
    assert!(
        message.contains("unsupported") && message.contains("profile"),
        "the profile rejection must be named: {message}"
    );
    assert!(hub.requests().is_empty(), "nothing may be uploaded");
}

#[test]
fn push_rejects_a_bundle_that_fails_validation_before_uploading() {
    let directory = tempfile::tempdir().unwrap();
    let v3 = v3_bundle(directory.path());
    let capsule = tampered_bundle(directory.path(), &v3);
    let hub = TestHub::start(Some(PUT_TOKEN));

    let pushed = push(&mut ato(), &capsule, &hub);
    assert!(!pushed.status.success());
    assert!(hub.requests().is_empty(), "nothing may be uploaded");
}

#[test]
fn v4_snapshot_bundles_round_trip_through_the_hub() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v4_bundle(directory.path());
    let original = fs::read(&capsule).unwrap();
    let hub = TestHub::start(Some(PUT_TOKEN));

    let pushed = push(&mut ato(), &capsule, &hub);
    assert_success(&pushed);
    let link = link_from(&pushed);
    let parsed = read_link(&link);
    assert_eq!(parsed.cid.len(), "sha256:".len() + 64);

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &link, &output, false);
    assert_success(&pulled);
    assert_eq!(fs::read(&output).unwrap(), original);
}

#[test]
fn pull_refuses_to_overwrite_without_force() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let original = fs::read(&capsule).unwrap();
    let hub = TestHub::start(Some(PUT_TOKEN));
    let link = link_from(&push(&mut ato(), &capsule, &hub));

    let output = directory.path().join("pulled.capsule");
    assert_success(&pull(&mut ato(), &link, &output, false));
    assert_eq!(fs::read(&output).unwrap(), original);

    let refused = pull(&mut ato(), &link, &output, false);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("refusing to overwrite"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(fs::read(&output).unwrap(), original);

    assert_success(&pull(&mut ato(), &link, &output, true));
    assert_eq!(fs::read(&output).unwrap(), original);
}

#[test]
fn plain_http_loopback_requires_the_explicit_flag() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));

    let pushed = ato()
        .args(["hub", "push"])
        .arg(&capsule)
        .args(["--hub-url", &hub.base_url()])
        .args(["--token", PUT_TOKEN])
        .output()
        .unwrap();
    assert!(!pushed.status.success());
    assert!(
        stderr(&pushed).contains("plain HTTP") || stderr(&pushed).contains("HTTPS is required"),
        "{}",
        stderr(&pushed)
    );
    assert!(hub.requests().is_empty());

    let link = link_from(&push(&mut ato(), &capsule, &hub));
    let output = directory.path().join("pulled.capsule");
    let pulled = ato()
        .args(["hub", "pull", &link, "--output"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(!pulled.status.success());
    assert!(
        stderr(&pulled).contains("plain HTTP") || stderr(&pulled).contains("HTTPS is required"),
        "{}",
        stderr(&pulled)
    );
    assert!(!output.exists());
}

#[test]
fn push_requires_an_explicit_hub_url() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let pushed = ato()
        .args(["hub", "push"])
        .arg(&capsule)
        .args(["--allow-loopback-http", "--token", PUT_TOKEN])
        .output()
        .unwrap();
    assert!(!pushed.status.success());
    assert!(
        stderr(&pushed).contains("hub base URL is required"),
        "{}",
        stderr(&pushed)
    );
}

#[test]
fn push_accepts_the_hub_url_from_the_environment() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));
    let pushed = ato()
        .args(["hub", "push"])
        .arg(&capsule)
        .args(["--allow-loopback-http", "--token", PUT_TOKEN])
        .env("ATO_HUB_URL", hub.base_url())
        .output()
        .unwrap();
    assert_success(&pushed);
    assert!(!link_from(&pushed).is_empty());
}

#[test]
fn push_reports_a_missing_upload_token() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));

    let pushed = push_without_token(&mut ato(), &capsule, &hub);
    assert!(!pushed.status.success());
    assert!(
        stderr(&pushed).contains("check --token"),
        "{}",
        stderr(&pushed)
    );
}

#[test]
fn pull_reports_a_missing_hub_object() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));
    let link = link_from(&push(&mut ato(), &capsule, &hub));
    hub.set_get_behavior(GetBehavior::Missing);

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &link, &output, false);
    assert!(!pulled.status.success());
    assert!(
        stderr(&pulled).contains("does not have object"),
        "{}",
        stderr(&pulled)
    );
    assert!(!output.exists());
}

#[test]
fn pull_never_runs_or_imports_the_capsule() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = TestHub::start(Some(PUT_TOKEN));
    let link = link_from(&push(&mut ato(), &capsule, &hub));

    let ato_home = directory.path().join("ato-home");
    fs::create_dir_all(&ato_home).unwrap();
    let output_dir = directory.path().join("downloads");
    fs::create_dir_all(&output_dir).unwrap();
    let output = output_dir.join("pulled.capsule");

    let pulled = ato()
        .args(["hub", "pull", &link, "--allow-loopback-http", "--output"])
        .arg(&output)
        .env("ATO_HOME", &ato_home)
        .output()
        .unwrap();
    assert_success(&pulled);

    // No Run state, no import, and no leftover temporary files: the command
    // only materializes the requested file.
    assert_eq!(
        fs::read_dir(&ato_home).unwrap().count(),
        0,
        "pull must not create local Run/import state"
    );
    let entries: Vec<_> = fs::read_dir(&output_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("pulled.capsule")]);
}
