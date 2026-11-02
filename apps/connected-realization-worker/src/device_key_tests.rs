//! Stub control-plane tests for the Runner side of the device-key contract.
//!
//! A local HTTP server parses the exact request the worker sends — method, path
//! and query, headers, body — recomputes the canonical signing string from
//! section 11.3 and verifies the Ed25519 signature with the enrolled public key.
//! The tests therefore pin the wire contract, not merely some header being set.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::*;
use crate::device_key::RunnerDeviceKey;

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    path_and_query: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    fn path(&self) -> &str {
        self.path_and_query
            .split_once('?')
            .map_or(self.path_and_query.as_str(), |(path, _)| path)
    }

    fn signed(&self) -> bool {
        self.header(device_key::SIGNATURE_HEADER).is_some()
    }
}

/// Signature verification the stub performs, so a test can also exercise a
/// rejected signature (tampered body) instead of only asserting client headers.
struct StubState {
    verifying_key: VerifyingKey,
    epoch: u64,
    runner_id: String,
    challenge_nonce: String,
    verify: bool,
    tamper: bool,
    seen_nonces: Mutex<BTreeSet<String>>,
    script: Mutex<VecDeque<(u16, String)>>,
    requests: Mutex<Vec<RecordedRequest>>,
    stop: AtomicBool,
}

struct StubControlPlane {
    base: String,
    state: Arc<StubState>,
    handle: Option<JoinHandle<()>>,
}

impl StubControlPlane {
    fn start(
        key: &RunnerDeviceKey,
        verify: bool,
        tamper: bool,
        script: Vec<(u16, String)>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub control plane");
        listener
            .set_nonblocking(true)
            .expect("nonblocking stub listener");
        let base = format!("http://{}", listener.local_addr().expect("stub address"));
        let state = Arc::new(StubState {
            verifying_key: key.verifying_key(),
            epoch: key.key_epoch(),
            runner_id: key.runner_id().to_owned(),
            challenge_nonce: "challenge-nonce".to_owned(),
            verify,
            tamper,
            seen_nonces: Mutex::new(BTreeSet::new()),
            script: Mutex::new(script.into()),
            requests: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let server_state = Arc::clone(&state);
        let handle = thread::spawn(move || {
            while !server_state.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    // The listener is non-blocking so shutdown is prompt; a BSD
                    // accept can inherit that flag, which would turn every
                    // request read into a spurious EOF. Handling a connection on
                    // its own blocking thread keeps one slow client from
                    // stalling the rest.
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let connection_state = Arc::clone(&server_state);
                        thread::spawn(move || handle_connection(stream, &connection_state));
                    }
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base,
            state,
            handle: Some(handle),
        }
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().unwrap().clone()
    }
}

impl Drop for StubControlPlane {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_connection(stream: TcpStream, state: &StubState) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stub stream"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path_and_query = parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        let _ = reader.read_exact(&mut body);
    }
    let request = RecordedRequest {
        method,
        path_and_query,
        headers,
        body,
    };
    let (status, response) = respond(&request, state);
    state.requests.lock().unwrap().push(request);
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
        response.len()
    );
}

fn error_body(code: &str) -> String {
    json!({ "error": code }).to_string()
}

fn verifies_b64(public_key_b64: &str, message: &[u8], signature_b64: &str) -> bool {
    let Ok(public_key) = URL_SAFE_NO_PAD.decode(public_key_b64) else {
        return false;
    };
    let Ok(public_key) = <[u8; 32]>::try_from(public_key.as_slice()) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key) else {
        return false;
    };
    let Ok(signature) = URL_SAFE_NO_PAD.decode(signature_b64) else {
        return false;
    };
    let Ok(signature) = Signature::from_slice(&signature) else {
        return false;
    };
    verifying_key.verify_strict(message, &signature).is_ok()
}

/// Absolute freshness: no more than the section 11.2 window either way.
fn timestamp_is_fresh(timestamp: &str) -> bool {
    let Ok(timestamp) = OffsetDateTime::parse(timestamp, &Rfc3339) else {
        return false;
    };
    (OffsetDateTime::now_utc() - timestamp)
        .whole_seconds()
        .abs()
        <= device_key::SIGNATURE_FRESHNESS.as_secs() as i64
}

fn respond(request: &RecordedRequest, state: &StubState) -> (u16, String) {
    if state.verify
        && device_key::is_signed_control_request(&request.method, &request.path_and_query)
    {
        let (Some(epoch), Some(timestamp), Some(nonce), Some(signature)) = (
            request.header(device_key::KEY_EPOCH_HEADER),
            request.header(device_key::TIMESTAMP_HEADER),
            request.header(device_key::NONCE_HEADER),
            request.header(device_key::SIGNATURE_HEADER),
        ) else {
            return (401, error_body("signature_required"));
        };
        let Ok(epoch) = epoch.parse::<u64>() else {
            return (401, error_body("signature_invalid"));
        };
        if epoch != state.epoch {
            return (401, error_body("signature_invalid"));
        }
        // A tampering intermediary is simulated by verifying against a body the
        // client did not sign.
        let verified_body = if state.tamper {
            let mut body = request.body.clone();
            body.push(b' ');
            body
        } else {
            request.body.clone()
        };
        let message = device_key::control_signing_string(
            &request.method,
            &request.path_and_query,
            &verified_body,
            timestamp,
            nonce,
            epoch,
        );
        let Ok(signature_bytes) = URL_SAFE_NO_PAD.decode(signature) else {
            return (401, error_body("signature_invalid"));
        };
        let Ok(signature) = Signature::from_slice(&signature_bytes) else {
            return (401, error_body("signature_invalid"));
        };
        if state
            .verifying_key
            .verify_strict(message.as_bytes(), &signature)
            .is_err()
        {
            return (401, error_body("signature_invalid"));
        }
        if !timestamp_is_fresh(timestamp) {
            return (401, error_body("signature_stale"));
        }
        if !state.seen_nonces.lock().unwrap().insert(nonce.to_owned()) {
            return (409, error_body("nonce_replayed"));
        }
    }
    if state.verify
        && let Some(rejection) = check_body_proof(request, state)
    {
        return rejection;
    }
    let mut script = state.script.lock().unwrap();
    if script.len() > 1 {
        script.pop_front().unwrap_or((200, "{}".to_owned()))
    } else {
        script.front().cloned().unwrap_or((200, "{}".to_owned()))
    }
}

/// Section 11.5 body proofs: register proves the register string with the key it
/// is registering; rotate proves the rotate string with the key that still has
/// authority.
fn check_body_proof(request: &RecordedRequest, state: &StubState) -> Option<(u16, String)> {
    if request.method != "POST" {
        return None;
    }
    let path = request.path();
    let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
    let signature = body.get("signature")?.as_str()?;
    if path.starts_with("/v1/runners/") && path.ends_with("/device-keys") {
        let public_key = body.get("public_key")?.as_str()?;
        let epoch = body.get("key_epoch")?.as_u64()?;
        let message =
            device_key::register_signing_string(&state.runner_id, &state.challenge_nonce, epoch);
        if !verifies_b64(public_key, message.as_bytes(), signature) {
            return Some((401, error_body("signature_invalid")));
        }
    } else if path.ends_with("/device-keys/rotate") {
        let next_public_key = body.get("next_public_key")?.as_str()?;
        let next_epoch = body.get("next_key_epoch")?.as_u64()?;
        let next_public_key = URL_SAFE_NO_PAD.decode(next_public_key).ok()?;
        let message = device_key::rotate_signing_string(
            &state.runner_id,
            state.epoch,
            next_epoch,
            &next_public_key,
            &state.challenge_nonce,
        );
        let Ok(signature_bytes) = URL_SAFE_NO_PAD.decode(signature) else {
            return Some((401, error_body("signature_invalid")));
        };
        let Ok(signature) = Signature::from_slice(&signature_bytes) else {
            return Some((401, error_body("signature_invalid")));
        };
        if state
            .verifying_key
            .verify_strict(message.as_bytes(), &signature)
            .is_err()
        {
            return Some((401, error_body("signature_invalid")));
        }
    }
    None
}

fn challenge_body() -> String {
    json!({
        "challenge_id": "challenge-id",
        "nonce": "challenge-nonce",
        "expires_at": "2026-11-03T00:05:00Z",
    })
    .to_string()
}

fn api_for(server: &StubControlPlane, key: Option<RunnerDeviceKey>) -> HttpRunnerApi {
    let mut api = HttpRunnerApi::new(&server.base, "runner_1", "bearer-token").unwrap();
    api.device_key = key;
    api
}

fn registered_key(root: &Path) -> (RunnerDeviceKey, std::path::PathBuf) {
    let path = root.join(device_key::DEVICE_KEY_FILE_NAME);
    let mut key = RunnerDeviceKey::generate("runner_1", device_key::FIRST_KEY_EPOCH).unwrap();
    key.set_registered(true);
    key.save_new(&path).unwrap();
    (key, path)
}

#[test]
fn q4_a_signed_status_report_carries_a_verified_signature() {
    let root = tempfile::tempdir().unwrap();
    let (key, _path) = registered_key(root.path());
    let server = StubControlPlane::start(&key, true, false, vec![(200, "{}".to_owned())]);
    let api = api_for(&server, Some(key));

    api.report_status("lease_1", "ready").unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path(), "/v1/runner-leases/lease_1/status");
    assert!(request.signed(), "the status report must be signed");
    let epoch: u64 = request
        .header(device_key::KEY_EPOCH_HEADER)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(epoch, device_key::FIRST_KEY_EPOCH);
    let message = device_key::control_signing_string(
        &request.method,
        &request.path_and_query,
        &request.body,
        request.header(device_key::TIMESTAMP_HEADER).unwrap(),
        request.header(device_key::NONCE_HEADER).unwrap(),
        epoch,
    );
    assert!(key_verifies(root.path(), message.as_bytes(), request));
}

fn key_verifies(root: &Path, message: &[u8], request: &RecordedRequest) -> bool {
    let Some(signature) = request.header(device_key::SIGNATURE_HEADER) else {
        return false;
    };
    let Ok(key) = RunnerDeviceKey::load_if_present(&root.join(device_key::DEVICE_KEY_FILE_NAME))
    else {
        return false;
    };
    key.is_some_and(|key| key.verifies(message, signature))
}

#[test]
fn q4_claim_and_open_requests_are_signed_and_verified() {
    let root = tempfile::tempdir().unwrap();
    let (key, _path) = registered_key(root.path());
    let server = StubControlPlane::start(
        &key,
        true,
        false,
        vec![
            (200, json!({ "lease": null }).to_string()),
            (200, json!({ "leases": [] }).to_string()),
        ],
    );
    let api = api_for(&server, Some(key));

    assert!(api.claim_next().unwrap().lease.is_none());
    assert!(api.open_leases().unwrap().is_empty());

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].path_and_query,
        "/v1/runners/runner_1/leases/next?wait_ms=20000"
    );
    assert!(requests[0].signed());
    assert_eq!(requests[1].path(), "/v1/runners/runner_1/leases/open");
    assert!(requests[1].signed());
}

#[test]
fn q4_a_tampered_body_is_rejected_and_the_runner_does_not_fall_back_to_bearer() {
    let root = tempfile::tempdir().unwrap();
    let (key, _path) = registered_key(root.path());
    // The stub verifies against a body that differs from the signed one, exactly
    // as a tampering intermediary would produce.
    let server = StubControlPlane::start(&key, true, true, vec![(200, "{}".to_owned())]);
    let api = api_for(&server, Some(key));

    let error = api.report_status("lease_1", "ready").unwrap_err();
    assert!(!format!("{error:#}").is_empty());
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        1,
        "a rejected signature must not be retried, and never as bearer-only"
    );
    assert!(requests[0].signed());
    assert!(!requests[0].body.is_empty(), "the body really was covered");
}

#[test]
fn q4_a_stale_timestamp_is_rejected_as_signature_stale() {
    let root = tempfile::tempdir().unwrap();
    let (key, _path) = registered_key(root.path());
    let server = StubControlPlane::start(&key, true, false, vec![(200, "{}".to_owned())]);

    let path = "/v1/runner-leases/lease_1/status";
    let body = json!({ "status": "ready" }).to_string();
    let stale_timestamp = (OffsetDateTime::now_utc()
        - time::Duration::seconds(device_key::SIGNATURE_FRESHNESS.as_secs() as i64 + 100))
    .replace_nanosecond(0)
    .unwrap()
    .format(&Rfc3339)
    .unwrap();
    let headers = key.sign_control_at(
        "POST",
        path,
        body.as_bytes(),
        &stale_timestamp,
        &device_key::fresh_nonce(),
    );
    let mut request = reqwest::blocking::Client::new()
        .post(format!("{}{path}", server.base))
        .bearer_auth("bearer-token")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    for (name, value) in headers.into_pairs() {
        request = request.header(name, value);
    }
    let response = request.send().unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn q40_a_retried_report_is_resigned_with_a_fresh_nonce_and_never_replayed() {
    let root = tempfile::tempdir().unwrap();
    let (key, _path) = registered_key(root.path());
    // Two transient failures then success: the retry must resend the same body
    // under a NEW nonce, or the control plane would answer nonce_replayed.
    let server = StubControlPlane::start(
        &key,
        true,
        false,
        vec![
            (503, "{}".to_owned()),
            (503, "{}".to_owned()),
            (200, "{}".to_owned()),
        ],
    );
    let api = api_for(&server, Some(key));

    api.report_status("lease_1", "ready").unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let bodies: BTreeSet<Vec<u8>> = requests
        .iter()
        .map(|request| request.body.clone())
        .collect();
    assert_eq!(bodies.len(), 1, "the body is byte-identical on every retry");
    let nonces: BTreeSet<&str> = requests
        .iter()
        .map(|request| request.header(device_key::NONCE_HEADER).unwrap())
        .collect();
    assert_eq!(nonces.len(), 3, "each attempt carries a fresh nonce");
}

#[test]
fn q4_register_binds_the_key_on_the_migration_path() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(device_key::DEVICE_KEY_FILE_NAME);
    let key = RunnerDeviceKey::generate("runner_1", device_key::FIRST_KEY_EPOCH).unwrap();
    key.save_new(&path).unwrap();
    assert!(!key.registered());
    // The stub verifies the register proof with the public key in the body.
    let server = StubControlPlane::start(
        &key,
        true,
        false,
        vec![(200, challenge_body()), (201, "{}".to_owned())],
    );
    let api = api_for(&server, None);

    let key = register_device_key_if_needed(&api, &path, key).unwrap();
    assert!(key.registered());
    let reloaded = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert!(reloaded.registered(), "registration is durable");
    assert_eq!(reloaded.public_key_bytes(), key.public_key_bytes());

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].path(),
        "/v1/runners/runner_1/device-keys/challenge"
    );
    assert_eq!(requests[1].path(), "/v1/runners/runner_1/device-keys");
}

#[test]
fn q4_a_control_plane_without_device_key_endpoints_leaves_the_runner_bearer_only() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(device_key::DEVICE_KEY_FILE_NAME);
    let key = RunnerDeviceKey::generate("runner_1", device_key::FIRST_KEY_EPOCH).unwrap();
    key.save_new(&path).unwrap();
    // Flag off: the challenge endpoint does not exist.
    let server = StubControlPlane::start(&key, false, false, vec![(404, error_body("not_found"))]);
    let api = api_for(&server, None);

    let key = register_device_key_if_needed(&api, &path, key).unwrap();
    assert!(
        !key.registered(),
        "an absent endpoint never marks the key active"
    );
    assert!(
        !RunnerDeviceKey::load_if_present(&path)
            .unwrap()
            .unwrap()
            .registered()
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn q30_a_failed_rotation_keeps_the_old_key() {
    let root = tempfile::tempdir().unwrap();
    let (key, path) = registered_key(root.path());
    let old_public = key.public_key_b64();
    let server = StubControlPlane::start(
        &key,
        true,
        false,
        vec![
            (200, challenge_body()),
            (409, error_body("key_epoch_conflict")),
        ],
    );
    let mut api = api_for(&server, Some(key));

    let error = api.rotate_device_key(&path).unwrap_err();
    assert!(format!("{error:#}").contains("rotation"), "{error:#}");
    let reloaded = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert_eq!(reloaded.key_epoch(), device_key::FIRST_KEY_EPOCH);
    assert_eq!(reloaded.public_key_b64(), old_public);
    assert_eq!(
        api.device_key.as_ref().unwrap().public_key_b64(),
        old_public,
        "the in-memory key is unchanged too"
    );
}

#[test]
fn q30_a_successful_rotation_installs_the_next_epoch_key() {
    let root = tempfile::tempdir().unwrap();
    let (key, path) = registered_key(root.path());
    let old_public = key.public_key_b64();
    // The stub verifies the rotate proof with the OLD key over the successor.
    let server = StubControlPlane::start(
        &key,
        true,
        false,
        vec![(200, challenge_body()), (200, "{}".to_owned())],
    );
    let mut api = api_for(&server, Some(key));

    api.rotate_device_key(&path).unwrap();

    let reloaded = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert_eq!(reloaded.key_epoch(), device_key::FIRST_KEY_EPOCH + 1);
    assert_ne!(reloaded.public_key_b64(), old_public);
    assert!(reloaded.registered());
    assert_eq!(
        api.device_key.as_ref().unwrap().key_epoch(),
        device_key::FIRST_KEY_EPOCH + 1
    );
}

#[test]
fn q38_two_starts_use_the_same_public_key_and_epoch() {
    let root = tempfile::tempdir().unwrap();
    let (_, path) = registered_key(root.path());
    let first = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    let server = StubControlPlane::start(&first, true, false, vec![(200, "{}".to_owned())]);
    let second = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert_eq!(
        first.public_key_b64(),
        second.public_key_b64(),
        "a restart reuses the same identity"
    );

    api_for(&server, Some(first))
        .report_status("lease_1", "ready")
        .unwrap();
    // A second worker process from the same file verifies against the same key.
    api_for(&server, Some(second))
        .report_status("lease_1", "ready")
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let epochs: BTreeSet<&str> = requests
        .iter()
        .map(|request| request.header(device_key::KEY_EPOCH_HEADER).unwrap())
        .collect();
    assert_eq!(epochs.len(), 1, "one identity, not a new one per start");
}

#[test]
fn q92_a_device_key_changes_only_the_headers_not_the_claimed_body() {
    let root = tempfile::tempdir().unwrap();
    let (key, _path) = registered_key(root.path());
    let keyed = StubControlPlane::start(&key, true, false, vec![(200, "{}".to_owned())]);
    let plain = StubControlPlane::start(&key, false, false, vec![(200, "{}".to_owned())]);

    api_for(&keyed, Some(key))
        .report_status("lease_1", "ready")
        .unwrap();
    api_for(&plain, None)
        .report_status("lease_1", "ready")
        .unwrap();

    let keyed_requests = keyed.requests();
    let plain_requests = plain.requests();
    assert!(keyed_requests[0].signed());
    assert!(!plain_requests[0].signed());
    assert_eq!(
        keyed_requests[0].body, plain_requests[0].body,
        "a key is proof, not a capability: it never changes what the Runner claims"
    );
}

#[test]
fn q4_doctor_reports_presence_and_epoch_without_any_key_material() {
    let root = tempfile::tempdir().unwrap();
    let key = RunnerDeviceKey::generate("runner_1", 2).unwrap();
    let path = root.path().join(device_key::DEVICE_KEY_FILE_NAME);
    key.save_new(&path).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let secret = stored["secret_key"].as_str().unwrap().to_owned();

    let report = diagnostics::report(root.path());
    assert_eq!(report["device_key"]["present"], true);
    assert_eq!(report["device_key"]["epoch"], 2);
    let text = serde_json::to_string(&report).unwrap();
    assert!(
        !text.contains(&key.public_key_b64()),
        "doctor reports presence and epoch only"
    );
    assert!(!text.contains(&secret), "doctor never prints key material");

    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        diagnostics::report(empty.path())["device_key"]["present"],
        false
    );
}

#[test]
fn q30_a_key_bound_to_another_runner_is_not_silently_rebound() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(device_key::DEVICE_KEY_FILE_NAME);
    let mut key = RunnerDeviceKey::generate("runner_other", device_key::FIRST_KEY_EPOCH).unwrap();
    key.set_registered(true);
    key.save_new(&path).unwrap();
    let old_public = key.public_key_b64();

    let error = bind_device_key_to_identity(&path, key, "runner_1", false).unwrap_err();
    assert!(format!("{error:#}").contains("stale key file"), "{error:#}");
    // Nothing was rewritten: the control plane may still hold this key.
    let reloaded = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert_eq!(reloaded.runner_id(), "runner_other");
    assert_eq!(reloaded.public_key_b64(), old_public);
    assert!(reloaded.registered());
}

#[test]
fn q4_an_interrupted_enrollment_reuses_its_key_but_a_stale_identity_key_is_replaced() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(device_key::DEVICE_KEY_FILE_NAME);

    // First enrollment: a key with no identity yet, exactly what an interrupted
    // enrollment leaves behind.
    let first = device_key_for_enrollment(&path).unwrap();
    assert!(first.runner_id().is_empty());
    // The retry signs with the SAME key, so a response this process never saw
    // cannot leave a second key bound on the control plane.
    let retry = device_key_for_enrollment(&path).unwrap();
    assert_eq!(retry.public_key_b64(), first.public_key_b64());

    // A key already tied to another identity must never sign a new enrollment.
    bind_device_key_to_identity(&path, retry, "runner_old", false).unwrap();
    let next = device_key_for_enrollment(&path).unwrap();
    assert!(next.runner_id().is_empty());
    assert_ne!(next.public_key_b64(), first.public_key_b64());
    let reloaded = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert_eq!(reloaded.public_key_b64(), next.public_key_b64());
}

#[test]
fn q30_an_enrollment_that_registered_the_key_needs_no_register_call() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(device_key::DEVICE_KEY_FILE_NAME);
    let key = RunnerDeviceKey::generate("", device_key::FIRST_KEY_EPOCH).unwrap();
    key.save_new(&path).unwrap();

    // The additive answer the control plane now returns when it stored the key.
    let response = json!({
        "runner": { "id": "runner_1" },
        "runner_token": "token",
        "device_key": { "key_epoch": 1, "registered": true },
    });
    let registered = enrollment_device_key_registration(&response, key.key_epoch()).unwrap();
    assert!(registered, "the response is authoritative");
    bind_device_key_to_identity(&path, key, "runner_1", registered).unwrap();
    let reloaded = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert!(reloaded.registered());
    assert_eq!(reloaded.runner_id(), "runner_1");

    // With the key already registered, the migration path must not touch the
    // network at all.
    let server = StubControlPlane::start(&reloaded, true, false, vec![(500, "{}".to_owned())]);
    let api = api_for(&server, None);
    let key = register_device_key_if_needed(&api, &path, reloaded).unwrap();
    assert!(key.registered());
    assert!(
        server.requests().is_empty(),
        "a registered key must not fall back to challenge/register"
    );
}

#[test]
fn q30_an_older_enrollment_answer_still_uses_the_register_migration_path() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(device_key::DEVICE_KEY_FILE_NAME);
    let key = RunnerDeviceKey::generate("", device_key::FIRST_KEY_EPOCH).unwrap();
    key.save_new(&path).unwrap();

    // No device_key block: an API that predates the additive field.
    let response = json!({
        "runner": { "id": "runner_1" },
        "runner_token": "token",
    });
    let registered = enrollment_device_key_registration(&response, key.key_epoch()).unwrap();
    assert!(!registered);
    bind_device_key_to_identity(&path, key, "runner_1", registered).unwrap();
    let reloaded = RunnerDeviceKey::load_if_present(&path).unwrap().unwrap();
    assert!(!reloaded.registered());

    // The migration path proves the key with the bearer it already holds.
    let server = StubControlPlane::start(
        &reloaded,
        true,
        false,
        vec![(200, challenge_body()), (201, "{}".to_owned())],
    );
    let api = api_for(&server, None);
    let key = register_device_key_if_needed(&api, &path, reloaded).unwrap();
    assert!(key.registered());
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn q30_an_enrollment_answer_for_another_epoch_fails_closed() {
    let key = RunnerDeviceKey::generate("runner_1", 1).unwrap();
    let response = json!({ "device_key": { "key_epoch": 2, "registered": true } });
    let error = enrollment_device_key_registration(&response, key.key_epoch()).unwrap_err();
    assert!(format!("{error:#}").contains("epoch"), "{error:#}");

    // A registered flag without the epoch the contract promises is not trusted
    // either: the Runner cannot tell which key the control plane really holds.
    let response = json!({ "device_key": { "registered": true } });
    assert!(enrollment_device_key_registration(&response, key.key_epoch()).is_err());

    // registered:false keeps the migration path.
    let response = json!({ "device_key": { "key_epoch": 1, "registered": false } });
    assert!(!enrollment_device_key_registration(&response, key.key_epoch()).unwrap());
}
