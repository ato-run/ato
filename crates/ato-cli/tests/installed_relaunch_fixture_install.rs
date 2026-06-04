use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use serial_test::serial;

const SHA: &str = "1111111111111111111111111111111111111111";
const REPO: &str = "ato-run/installed-relaunch-node";
const TARBALL_PREFIX: &str = "installed-relaunch-node-111111111111";

#[test]
#[serial]
fn installed_relaunch_fixture_installs_without_external_network() {
    let scratch = ScratchDir::new("installed_relaunch_fixture_install");
    let ato_home = scratch.path().join("ato-home");
    let home = scratch.path().join("home");
    let output_dir = scratch.path().join("store");
    fs::create_dir_all(&ato_home).expect("create ATO_HOME");
    fs::create_dir_all(&home).expect("create HOME");

    assert_port_available(18880);

    let archive = deterministic_github_tarball(fixture_dir(), TARBALL_PREFIX);
    let server = MockInstallServer::start(archive);

    let output = Command::new(assert_cmd::cargo::cargo_bin!("ato"))
        .current_dir(scratch.path())
        .env("ATO_HOME", &ato_home)
        .env("HOME", &home)
        .env("ATO_STORE_API_URL", server.base_url())
        .env("ATO_GITHUB_API_BASE_URL", server.base_url())
        .env("ATO_TELEMETRY", "0")
        .args([
            "install",
            "--from-gh-repo",
            "github.com/ato-run/installed-relaunch-node",
            "--output",
        ])
        .arg(&output_dir)
        .args(["--yes", "--no-project", "--json"])
        .output()
        .expect("run ato install");

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let result = parse_install_result_json(&output.stdout);
    let lifecycle = result
        .get("install_lifecycle")
        .and_then(|value| value.as_object())
        .expect("install_lifecycle object");
    let ipk = lifecycle
        .get("install_profile_key")
        .and_then(|value| value.as_str())
        .expect("install_profile_key");
    assert_ipk_shape(ipk);

    let installed_path = result
        .get("path")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .expect("installed artifact path");
    assert!(
        installed_path.exists(),
        "installed artifact missing: {}",
        installed_path.display()
    );

    let current_revision_path = lifecycle
        .get("current_revision_path")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .expect("current_revision_path");
    assert!(
        current_revision_path.exists(),
        "current revision path missing: {}",
        current_revision_path.display()
    );

    assert_capsule_archive_contains_fixture(&installed_path);
    assert!(
        tree_contains_file_named(
            &current_revision_path,
            "installed-relaunch-node-0.1.0.capsule"
        ) || tree_contains_file_named(&current_revision_path, "capsule.toml"),
        "current revision did not materialize fixture artifact/source under {}",
        current_revision_path.display()
    );

    let requests = server.requests();
    assert_eq!(
        requests,
        vec![
            "/v1/github/repos/ato-run/installed-relaunch-node/install-draft".to_string(),
            format!("/repos/ato-run/installed-relaunch-node/tarball/{SHA}"),
        ],
        "unexpected mock server requests; install should stay on mocked Store/GitHub paths"
    );
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("installed-relaunch-node")
}

fn assert_port_available(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|err| {
        panic!("fixture port {port} must be available for serial smoke: {err}")
    });
    drop(listener);
}

fn assert_ipk_shape(value: &str) {
    assert_eq!(value.len(), "ipk_".len() + 32, "unexpected IPK length");
    assert!(value.starts_with("ipk_"), "unexpected IPK prefix: {value}");
    assert!(
        value
            .trim_start_matches("ipk_")
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
        "IPK must be lowercase hex: {value}"
    );
}

fn parse_install_result_json(stdout: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stdout);
    for (index, _) in text.match_indices('{').rev() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text[index..])
            && value.get("install_lifecycle").is_some()
        {
            return value;
        }
    }
    panic!("install result JSON not found in stdout:\n{text}");
}

fn deterministic_github_tarball(src: PathBuf, root_prefix: &str) -> Vec<u8> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&src).min_depth(1) {
        let entry = entry.expect("walk fixture");
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&src)
            .expect("strip fixture prefix")
            .to_path_buf();
        files.push((rel, entry.path().to_path_buf()));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));

    let mut bytes = Vec::new();
    let encoder = flate2::write::GzEncoder::new(&mut bytes, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (rel, full_path) in files {
        let contents = fs::read(&full_path).expect("read fixture file");
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                Path::new(root_prefix).join(rel),
                std::io::Cursor::new(contents),
            )
            .expect("append deterministic tar entry");
    }
    builder
        .into_inner()
        .expect("finish tar builder")
        .finish()
        .expect("finish gzip encoder");
    bytes
}

fn assert_capsule_archive_contains_fixture(path: &Path) {
    let bytes = fs::read(path).expect("read installed capsule");
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    let mut names = Vec::new();
    let mut payload = None;
    for entry in archive.entries().expect("read capsule archive entries") {
        let mut entry = entry.expect("capsule archive entry");
        let name = entry
            .path()
            .expect("entry path")
            .to_string_lossy()
            .into_owned();
        if name == "payload.tar.zst" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).expect("read payload.tar.zst");
            payload = Some(bytes);
        }
        names.push(name);
    }
    assert!(
        names.iter().any(|name| name.ends_with("capsule.toml")),
        "capsule archive missing capsule.toml: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "payload.tar.zst"),
        "capsule archive missing payload.tar.zst: {names:?}"
    );

    let payload = payload.expect("payload.tar.zst bytes");
    let decoder = zstd::stream::read::Decoder::new(std::io::Cursor::new(payload))
        .expect("zstd payload decoder");
    let mut payload_archive = tar::Archive::new(decoder);
    let mut payload_names = Vec::new();
    for entry in payload_archive.entries().expect("read payload entries") {
        let mut entry = entry.expect("payload entry");
        let name = entry
            .path()
            .expect("payload entry path")
            .to_string_lossy()
            .into_owned();
        if name.ends_with("server.js") {
            let mut body = String::new();
            entry
                .read_to_string(&mut body)
                .expect("read payload server.js");
            assert!(
                body.contains("Ato installed relaunch fixture"),
                "server.js marker missing"
            );
        }
        payload_names.push(name);
    }
    assert!(
        payload_names.iter().any(|name| name.ends_with("server.js")),
        "payload missing server.js: {payload_names:?}"
    );
}

fn tree_contains_file_named(root: &Path, file_name: &str) -> bool {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .any(|entry| entry.file_type().is_file() && entry.file_name() == file_name)
}

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(name: &str) -> Self {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf();
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos(),
            rand::random::<u64>()
        );
        let path = workspace.join(".tmp").join(name).join(unique);
        fs::create_dir_all(&path).expect("create scratch dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct MockInstallServer {
    base_url: String,
    shutdown: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockInstallServer {
    fn start(archive: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock install server");
        listener
            .set_nonblocking(true)
            .expect("set mock listener nonblocking");
        let addr = listener.local_addr().expect("mock listener addr");
        let shutdown = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_shutdown = Arc::clone(&shutdown);
        let thread_requests = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("set mock stream blocking");
                        handle_request(&mut stream, &archive, &thread_requests);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            shutdown,
            requests,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for MockInstallServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_request(stream: &mut TcpStream, archive: &[u8], requests: &Arc<Mutex<Vec<String>>>) {
    let mut request = [0u8; 8192];
    let size = stream.read(&mut request).expect("read mock request");
    let request_text = String::from_utf8_lossy(&request[..size]);
    let request_line = request_text.lines().next().unwrap_or_default();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    if path != "/" {
        requests.lock().expect("requests lock").push(path.clone());
    }

    match path.as_str() {
        "/v1/github/repos/ato-run/installed-relaunch-node/install-draft" => {
            let body = install_draft_json();
            write_response(stream, "200 OK", "application/json", body.as_bytes());
        }
        path if path == format!("/repos/ato-run/installed-relaunch-node/tarball/{SHA}") => {
            write_response(stream, "200 OK", "application/gzip", archive);
        }
        _ => {
            write_response(
                stream,
                "404 Not Found",
                "application/json",
                br#"{"error":"unexpected mock path"}"#,
            );
        }
    }
}

fn install_draft_json() -> String {
    let preview_toml =
        fs::read_to_string(fixture_dir().join("capsule.toml")).expect("read fixture capsule.toml");
    serde_json::json!({
        "repo": {
            "owner": "ato-run",
            "repo": "installed-relaunch-node",
            "fullName": REPO,
            "defaultBranch": "main"
        },
        "capsuleToml": { "exists": true },
        "repoRef": REPO,
        "proposedRunCommand": "node server.js",
        "proposedInstallCommand": "ato install --from-gh-repo github.com/ato-run/installed-relaunch-node",
        "resolvedRef": {
            "ref": "main",
            "sha": SHA
        },
        "manifestSource": "existing",
        "previewToml": preview_toml,
        "capsuleHint": {
            "confidence": "high",
            "warnings": [],
            "launchability": "runnable"
        },
        "inferenceMode": "fixture",
        "retryable": false
    })
    .to_string()
}

fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let header = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .expect("write response header");
    stream.write_all(body).expect("write response body");
}
