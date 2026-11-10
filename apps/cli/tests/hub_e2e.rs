//! End-to-end `ato hub push|pull` tests against the **real** coordinator.
//!
//! Unlike `tests/hub.rs` (which uses a recording stub), these tests start the
//! `capsulehub` service library in-process on an ephemeral loopback port. That
//! is the same coordinator code the `ato-capsulehub` binary runs: real
//! `PUT`/`GET`/`HEAD`/`DELETE` handling, real on-disk content-addressed
//! storage, real operator-token checks.
//!
//! Everything is hermetic: temporary storage root, ephemeral port, no external
//! network, no shared state.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use ato_objects::capsulehub::{HubKey, SharedLink};
use ato_portable_application::build_static_bundle;
use capsulehub::{PutAuth, Secret, ServerConfig};
use sha2::{Digest, Sha256};

const PUT_TOKEN: &str = "e2e-put-token";
const DELETE_TOKEN: &str = "e2e-delete-token";
const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Real coordinator, in-process
// ---------------------------------------------------------------------------

struct RealHub {
    address: SocketAddr,
    storage: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RealHub {
    /// Starts the coordinator library on `127.0.0.1:0`.
    ///
    /// `put_token = None` uses the coordinator's open upload mode;
    /// `allow_anonymous_read = false` makes reads require the upload token
    /// (the configuration the pull credential exists for).
    fn start(
        put_token: Option<&str>,
        delete_token: Option<&str>,
        allow_anonymous_read: bool,
    ) -> Self {
        let storage = tempfile::tempdir().expect("temporary coordinator storage");
        let config = ServerConfig {
            bind: "127.0.0.1:0".parse().expect("loopback bind address"),
            storage: storage.path().to_path_buf(),
            max_object_bytes: MAX_OBJECT_BYTES,
            max_storage_bytes: None,
            put_auth: if put_token.is_some() {
                PutAuth::Token
            } else {
                PutAuth::Open
            },
            put_token: put_token.map(Secret::new),
            delete_token: delete_token.map(Secret::new),
            // Rate limiting and retention are covered by the service's own
            // tests; disabling them keeps this test deterministic.
            rate_limit_per_minute: 0,
            retention: None,
            allow_anonymous_read,
        };

        let stop = Arc::new(AtomicBool::new(false));
        let stop_from_thread = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime for the coordinator");
            runtime.block_on(async move {
                let hub = capsulehub::start_in_background(config)
                    .await
                    .expect("the coordinator starts");
                ready_tx
                    .send(hub.local_addr())
                    .expect("report the coordinator address");
                while !stop_from_thread.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                hub.shutdown().await;
            });
        });
        let address = ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the coordinator becomes ready");
        Self {
            address,
            storage,
            stop,
            thread: Some(thread),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn object_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(self.storage.path().join("objects"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn partial_files(&self) -> Vec<PathBuf> {
        fs::read_dir(self.storage.path().join("tmp"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn object_file(&self, object_id: &str) -> PathBuf {
        let hex = object_id
            .strip_prefix("sha256:")
            .expect("object ids are sha256-prefixed");
        self.storage.path().join("objects").join(hex)
    }
}

impl Drop for RealHub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------------------
// CLI helpers
// ---------------------------------------------------------------------------

fn ato() -> AssertCommand {
    let mut command =
        AssertCommand::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.timeout(Duration::from_secs(60));
    command.env_remove("ATO_HUB_URL");
    command.env_remove("ATO_HUB_TOKEN");
    command
}

fn sample_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k")
}

fn v3_bundle(directory: &Path) -> PathBuf {
    let (bytes, _) =
        build_static_bundle(&sample_source(), "Capsulehub e2e").expect("the static sample packs");
    let path = directory.join("e2e.capsule");
    fs::write(&path, bytes).expect("write the v3 fixture");
    path
}

fn v2_bundle(directory: &Path) -> PathBuf {
    let bytes = format!(
        "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
        "0".repeat(64)
    );
    let path = directory.join("computation-v2.capsule");
    fs::write(&path, bytes).expect("write the v2 fixture");
    path
}

fn unknown_profile_bundle(directory: &Path, v3: &Path) -> PathBuf {
    let bytes = fs::read(v3).expect("read the v3 fixture");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("valid bundle JSON");
    value["index"]["profile"] =
        serde_json::Value::String("ato.portable-application/999".to_owned());
    let path = directory.join("unknown-profile.capsule");
    fs::write(&path, serde_json::to_vec(&value).expect("serialize"))
        .expect("write the unknown-profile fixture");
    path
}

fn push(command: &mut AssertCommand, capsule: &Path, hub: &RealHub, token: Option<&str>) -> Output {
    command.args(["hub", "push"]).arg(capsule).args([
        "--hub-url",
        &hub.base_url(),
        "--allow-loopback-http",
    ]);
    if let Some(token) = token {
        command.args(["--token", token]);
    }
    command.output().expect("ato hub push runs")
}

fn pull(command: &mut AssertCommand, link: &str, output: &Path) -> Output {
    command
        .args(["hub", "pull", link, "--allow-loopback-http", "--output"])
        .arg(output)
        .output()
        .expect("ato hub pull runs")
}

fn pull_with_token(command: &mut AssertCommand, link: &str, output: &Path, token: &str) -> Output {
    command
        .args([
            "hub",
            "pull",
            link,
            "--allow-loopback-http",
            "--token",
            token,
            "--output",
        ])
        .arg(output)
        .output()
        .expect("ato hub pull runs")
}

fn pull_with_env_token(
    command: &mut AssertCommand,
    link: &str,
    output: &Path,
    token: &str,
    configured_hub: Option<&str>,
) -> Output {
    command
        .args(["hub", "pull", link, "--allow-loopback-http", "--output"])
        .arg(output);
    if let Some(configured_hub) = configured_hub {
        command.arg("--hub-url").arg(configured_hub);
    }
    command
        .env("ATO_HUB_TOKEN", token)
        .output()
        .expect("ato hub pull runs")
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

/// Minimal raw HTTP request for operator endpoints (the CLI intentionally has
/// no delete command).
fn raw_request(
    address: SocketAddr,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut stream = TcpStream::connect(address).expect("connect to the coordinator");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: 0\r\n"
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .expect("write the request");
    stream.flush().expect("flush the request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read the response");
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("response header separator");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let status = head
        .lines()
        .next()
        .expect("status line")
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status");
    let body = String::from_utf8_lossy(&raw[split + 4..]).into_owned();
    (status, body)
}

/// Parses a link and rebuilds it with different key/cid claims for tamper
/// tests.
fn retarget(link: &str, key: HubKey, cid: String) -> String {
    let parsed = SharedLink::parse(link).expect("the produced link parses");
    let base = parsed.base_url().expect("link base");
    SharedLink::new(&base, parsed.object_id, key, cid)
        .expect("well-formed link")
        .to_url(&base)
        .expect("render the link")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn real_coordinator_push_pull_round_trip_is_byte_identical() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let original = fs::read(&capsule).unwrap();
    let hub = RealHub::start(Some(PUT_TOKEN), Some(DELETE_TOKEN), true);

    let pushed = push(&mut ato(), &capsule, &hub, Some(PUT_TOKEN));
    assert_success(&pushed);
    let link = link_from(&pushed);
    let parsed = SharedLink::parse(&link).unwrap();

    // The coordinator really stored one content-addressed object with the id
    // printed in the link, and it is ciphertext, not the bundle.
    let files = hub.object_files();
    assert_eq!(files.len(), 1, "exactly one object is stored");
    let stored = fs::read(&files[0]).unwrap();
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(&stored)),
        parsed.object_id.as_str()
    );
    assert!(stored.starts_with(b"ATOHUB"));
    assert_ne!(stored, original);
    assert!(hub.partial_files().is_empty());

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &link, &output);
    assert_success(&pulled);
    assert_eq!(fs::read(&output).unwrap(), original);
    let stdout = String::from_utf8_lossy(&pulled.stdout);
    assert!(
        stdout.contains(&format!("contract_ref={}", parsed.cid)),
        "pull must report the same ContractRef the link declares: {stdout}"
    );
}

#[test]
fn real_coordinator_tamper_after_store_fails_at_the_digest_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = RealHub::start(Some(PUT_TOKEN), Some(DELETE_TOKEN), true);
    let link = link_from(&push(&mut ato(), &capsule, &hub, Some(PUT_TOKEN)));
    let parsed = SharedLink::parse(&link).unwrap();

    // Simulate a compromised/incorrect coordinator: same length, different
    // bytes on disk.
    let object = hub.object_file(parsed.object_id.as_str());
    let mut tampered = fs::read(&object).unwrap();
    for byte in &mut tampered {
        *byte ^= 0xa5;
    }
    fs::write(&object, &tampered).unwrap();

    let output = directory.path().join("pulled.capsule");
    let pulled = pull(&mut ato(), &link, &output);
    assert!(!pulled.status.success());
    let message = stderr(&pulled);
    assert!(
        message.contains("ciphertext digest mismatch"),
        "the digest boundary must be named: {message}"
    );
    assert!(!output.exists());
}

#[test]
fn real_coordinator_key_and_cid_tamper_fail_at_their_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = RealHub::start(Some(PUT_TOKEN), Some(DELETE_TOKEN), true);
    let link = link_from(&push(&mut ato(), &capsule, &hub, Some(PUT_TOKEN)));
    let parsed = SharedLink::parse(&link).unwrap();

    let replacement = HubKey::generate().unwrap();
    let replacement_secret = replacement.to_fragment_secret();
    let wrong_key_link = retarget(&link, replacement, parsed.cid.clone());
    let key_output = directory.path().join("wrong-key.capsule");
    let wrong_key = pull(&mut ato(), &wrong_key_link, &key_output);
    assert!(!wrong_key.status.success());
    let key_message = stderr(&wrong_key);
    assert!(
        key_message.contains("decrypt") || key_message.contains("authentication"),
        "the authentication boundary must be named: {key_message}"
    );
    assert!(!key_output.exists());
    assert!(
        !key_message.contains(&replacement_secret)
            && !key_message.contains(&parsed.key.to_fragment_secret()),
        "key material must never appear in an error message"
    );

    let wrong_cid_link = retarget(
        &link,
        HubKey::from_fragment_secret(&parsed.key.to_fragment_secret()).unwrap(),
        format!("sha256:{}", "b".repeat(64)),
    );
    let cid_output = directory.path().join("wrong-cid.capsule");
    let wrong_cid = pull(&mut ato(), &wrong_cid_link, &cid_output);
    assert!(!wrong_cid.status.success());
    assert!(
        stderr(&wrong_cid).contains("capsule identity mismatch"),
        "the identity boundary must be named: {}",
        stderr(&wrong_cid)
    );
    assert!(!cid_output.exists());
}

#[test]
fn real_coordinator_enforces_put_and_operator_delete_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let hub = RealHub::start(Some(PUT_TOKEN), Some(DELETE_TOKEN), true);

    let missing = push(&mut ato(), &capsule, &hub, None);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("check --token"),
        "the 401 must be named: {}",
        stderr(&missing)
    );
    assert!(
        hub.object_files().is_empty(),
        "an unauthenticated upload must store nothing"
    );

    let pushed = push(&mut ato(), &capsule, &hub, Some(PUT_TOKEN));
    assert_success(&pushed);
    let link = link_from(&pushed);
    let parsed = SharedLink::parse(&link).unwrap();
    let target = format!("/v1/capsulehub/objects/{}", parsed.object_id);
    let object = hub.object_file(parsed.object_id.as_str());
    assert!(object.is_file());

    assert_eq!(
        raw_request(hub.address, "DELETE", &target, &[]).0,
        403,
        "delete requires the operator token"
    );
    let wrong = "Bearer wrong-token";
    assert_eq!(
        raw_request(hub.address, "DELETE", &target, &[("Authorization", wrong)]).0,
        403
    );
    let upload_credential = format!("Bearer {PUT_TOKEN}");
    assert_eq!(
        raw_request(
            hub.address,
            "DELETE",
            &target,
            &[("Authorization", &upload_credential)]
        )
        .0,
        403,
        "the upload token is not an operator credential"
    );
    assert!(object.is_file(), "a refused delete must keep the object");

    let operator = format!("Bearer {DELETE_TOKEN}");
    assert_eq!(
        raw_request(
            hub.address,
            "DELETE",
            &target,
            &[("Authorization", &operator)]
        )
        .0,
        204
    );
    assert!(!object.exists());

    let output = directory.path().join("removed.capsule");
    let pulled = pull(&mut ato(), &link, &output);
    assert!(!pulled.status.success());
    assert!(
        stderr(&pulled).contains("does not have object"),
        "{}",
        stderr(&pulled)
    );
    assert!(!output.exists());
}

#[test]
fn real_coordinator_authenticated_reads_accept_a_pull_token() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let original = fs::read(&capsule).unwrap();
    // Reads require the upload token: the configuration the pull credential is for.
    let hub = RealHub::start(Some(PUT_TOKEN), Some(DELETE_TOKEN), false);

    let pushed = push(&mut ato(), &capsule, &hub, Some(PUT_TOKEN));
    assert_success(&pushed);
    let link = link_from(&pushed);

    let anonymous_output = directory.path().join("anonymous.capsule");
    let anonymous = pull(&mut ato(), &link, &anonymous_output);
    assert!(!anonymous.status.success());
    assert!(
        stderr(&anonymous).contains("hub refused the download (401"),
        "{}",
        stderr(&anonymous)
    );
    assert!(
        stderr(&anonymous).contains("check --token") || stderr(&anonymous).contains("--token"),
        "{}",
        stderr(&anonymous)
    );
    assert!(!anonymous_output.exists());

    let credentialed_output = directory.path().join("credentialed.capsule");
    let credentialed = pull_with_token(&mut ato(), &link, &credentialed_output, PUT_TOKEN);
    assert_success(&credentialed);
    assert_eq!(fs::read(&credentialed_output).unwrap(), original);

    // The environment token is sent because the link's hub is the configured
    // hub (`--hub-url` with a trailing slash, normalized).
    let env_output = directory.path().join("env-token.capsule");
    let env_pulled = pull_with_env_token(
        &mut ato(),
        &link,
        &env_output,
        PUT_TOKEN,
        Some(&format!("{}/", hub.base_url())),
    );
    assert_success(&env_pulled);
    assert_eq!(fs::read(&env_output).unwrap(), original);
}

#[test]
fn real_coordinator_withholds_the_env_token_from_a_foreign_hub() {
    let directory = tempfile::tempdir().unwrap();
    let capsule = v3_bundle(directory.path());
    let original = fs::read(&capsule).unwrap();
    // Reads need the upload token, so a withheld token fails closed (401).
    let hub = RealHub::start(Some(PUT_TOKEN), Some(DELETE_TOKEN), false);
    let pushed = push(&mut ato(), &capsule, &hub, Some(PUT_TOKEN));
    assert_success(&pushed);
    let link = link_from(&pushed);

    // 1. A configured hub that is NOT the link's hub: the env token is withheld.
    let foreign_output = directory.path().join("foreign.capsule");
    let foreign = pull_with_env_token(
        &mut ato(),
        &link,
        &foreign_output,
        PUT_TOKEN,
        Some("https://other.example"),
    );
    assert!(!foreign.status.success());
    let message = stderr(&foreign);
    assert!(
        message.contains("--token explicitly"),
        "the credential rule must be named: {message}"
    );
    assert!(
        !message.contains(PUT_TOKEN),
        "a withheld environment token must never be echoed: {message}"
    );
    assert!(!foreign_output.exists());

    // 2. No configured hub at all: the env token is withheld as well.
    let unconfigured_output = directory.path().join("unconfigured.capsule");
    let unconfigured =
        pull_with_env_token(&mut ato(), &link, &unconfigured_output, PUT_TOKEN, None);
    assert!(!unconfigured.status.success());
    assert!(
        stderr(&unconfigured).contains("--token explicitly"),
        "{}",
        stderr(&unconfigured)
    );
    assert!(!stderr(&unconfigured).contains(PUT_TOKEN));
    assert!(!unconfigured_output.exists());

    // 3. A different path on the same origin is a different hub.
    let path_output = directory.path().join("other-path.capsule");
    let path_mismatch = pull_with_env_token(
        &mut ato(),
        &link,
        &path_output,
        PUT_TOKEN,
        Some(&format!("{}/other", hub.base_url())),
    );
    assert!(!path_mismatch.status.success());
    assert!(
        stderr(&path_mismatch).contains("--token explicitly"),
        "{}",
        stderr(&path_mismatch)
    );
    assert!(!path_output.exists());

    // 4. An explicit --token is the user's choice for this link: it is sent
    //    even when the configured hub differs.
    let explicit_output = directory.path().join("explicit.capsule");
    let explicit = ato()
        .args(["hub", "pull", &link, "--allow-loopback-http"])
        .args(["--token", PUT_TOKEN])
        .args(["--hub-url", "https://other.example", "--output"])
        .arg(&explicit_output)
        .output()
        .expect("ato hub pull runs");
    assert_success(&explicit);
    assert_eq!(fs::read(&explicit_output).unwrap(), original);

    // 5. ATO_HUB_URL is the configured hub, so the env token is sent.
    let env_url_output = directory.path().join("env-url.capsule");
    let env_url = ato()
        .args(["hub", "pull", &link, "--allow-loopback-http", "--output"])
        .arg(&env_url_output)
        .env("ATO_HUB_URL", hub.base_url())
        .env("ATO_HUB_TOKEN", PUT_TOKEN)
        .output()
        .expect("ato hub pull runs");
    assert_success(&env_url);
    assert_eq!(fs::read(&env_url_output).unwrap(), original);
}

#[test]
fn real_coordinator_stores_nothing_for_v2_or_unknown_profiles() {
    let directory = tempfile::tempdir().unwrap();
    let v3 = v3_bundle(directory.path());
    let hub = RealHub::start(Some(PUT_TOKEN), Some(DELETE_TOKEN), true);

    let v2 = v2_bundle(directory.path());
    let v2_pushed = push(&mut ato(), &v2, &hub, Some(PUT_TOKEN));
    assert!(!v2_pushed.status.success());
    assert!(
        stderr(&v2_pushed).contains("computation-root wire v2"),
        "{}",
        stderr(&v2_pushed)
    );

    let unknown = unknown_profile_bundle(directory.path(), &v3);
    let unknown_pushed = push(&mut ato(), &unknown, &hub, Some(PUT_TOKEN));
    assert!(!unknown_pushed.status.success());
    assert!(
        stderr(&unknown_pushed).contains("unsupported")
            && stderr(&unknown_pushed).contains("profile"),
        "{}",
        stderr(&unknown_pushed)
    );

    assert!(
        hub.object_files().is_empty(),
        "a refused push must store no object"
    );
    assert!(
        hub.partial_files().is_empty(),
        "a refused push must leave no partial upload"
    );
}
