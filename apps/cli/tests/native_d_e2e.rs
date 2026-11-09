//! PR8 — end-to-end acceptance matrix and self-hosted operation evidence.
//!
//! One Identity K (`GET /proof.txt` with an exact body digest) is declared with
//! three Derivations: a static web route, a native macOS route and a native
//! Windows route. Every test below drives the **real** `ato` binary (or the
//! authoritative portable-application API) against a bundle built by the real
//! `ato pack` path; nothing here stands in for acceptance.
//!
//! Documented environment limits (this suite reports them, it never turns them
//! into a pass):
//!
//! * A host that is itself sandboxed cannot nest a Seatbelt profile
//!   (`sandbox_apply: Operation not permitted`). The macOS native route is then
//!   refused at admission with the named `native_sandbox_unavailable`, so a
//!   native launch cannot be observed. The launch-dependent legs of rows 3, 4
//!   (signature), 5 and 6 are gated on the measured capability: on a capable
//!   host they assert the full acceptance, here they assert the measured
//!   fail-closed refusal and print a `SKIP (host capability)` line.
//! * A durable local Instance start has no unsigned-native override, so a
//!   durable *start* of the unsigned fixture native route is refused there by
//!   design; the state row is therefore exercised at the `LocalInstance` level
//!   (the design brief's option (b)).
//! * v0 has no shared save format between a static-web route (browser
//!   `localStorage`, captured from a live browser) and a native route (a
//!   filesystem slot). The cross-D state fixture therefore carries two native
//!   routes that both declare the same slot, and the test states plainly that
//!   the web -> native -> web runtime round trip is not met. See
//!   `samples/native-desktop-stateful/capsule.toml`.
//!
//! The coordinator is the real self-hosted binary (`ato-capsulehub`) when it
//! has been built; otherwise the same coordinator library is started in-process
//! and the test says which one ran.
//!
//! Live acceptance (a host where `sandbox-exec` can be applied, e.g. an
//! unsandboxed shell) — build the coordinator first so the distribution rows
//! run against the real binary, then:
//!
//! ```text
//! cargo build -p capsulehub --bin ato-capsulehub
//! cargo test -p ato-cli --test native_d_e2e -- --nocapture --test-threads=1
//! ```
//!
//! `SKIP (host capability)` lines only appear when `NativeFacts::probe()` says
//! the native Adapter cannot enforce its mandatory policies on this host.
//!
//! Rows and the tests that carry them (DESIGN §9):
//!
//! | Row | Test |
//! |---|---|
//! | 1 same K, distinct Ds | `one_source_declares_three_routes_over_one_contract` |
//! | 2 explicit D selection | `a_run_selects_exactly_the_declared_derivation` |
//! | 3 same requirement set from observation only | `the_same_requirement_set_is_verified_for_web_and_native` |
//! | 4 invalid native fails closed | `an_artifact_digest_mismatch_is_refused_before_admission`, `an_unsigned_artifact_needs_the_declared_local_override`, `a_host_that_cannot_enforce_the_native_policy_refuses_admission` |
//! | 5 lifecycle leaves no owned process | `a_native_run_leaves_no_owned_process` (stop), `a_crashed_native_leader_leaves_no_owned_process` (crash), `a_native_run_that_never_serves_is_reaped_at_the_readiness_timeout` (timeout) |
//! | 6 state handover | `instance_state_survives_a_derivation_switch_when_both_declare_the_slot` |
//! | 7 hub push -> pull keeps bytes/ContractRef/routes | `a_distributed_three_route_bundle_keeps_its_bytes_contract_and_routes` |
//! | 8 hub tamper per boundary | `a_tampered_distribution_is_refused_at_each_boundary` |
//! | 9 v2/unknown rejected, v3/v4 accepted | `only_the_declared_portable_profiles_are_distributed` |
//! | 10 hosted path uses the same coordinator | the distribution tests: the spawned `ato-capsulehub` binary when built, else the same library in-process |
//!
//! Row 5's three triggers (stop, crash, readiness timeout) each build their own
//! artifact with the host's `tar` and run it live when the capability probe
//! allows; when it does not, each asserts the named refusal and that nothing was
//! started, and says so.
//!
//! Not exercised here, with the reason: restoring saved data *into a native
//! run*. A durable local Instance start carries no unsigned-native override, so
//! an unsigned native D cannot be started there (it is refused, and the test
//! asserts that), and `ato run` is ephemeral and restores no snapshot. The
//! state row therefore proves save/restore/cross-D compatibility through the
//! Instance APIs and proves live that the native route receives and writes its
//! declared slot; the middle "a restored snapshot is consumed by the native
//! workload" leg is the part that is not met.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::Command as AtoCommand;
use ato_objects::capsulehub::{HubKey, SharedLink};
use ato_portable_application::local_instance::LocalApplicationStore;
use ato_portable_application::state_continuity::StateCompatibility;
use ato_portable_application::{
    PortableRealizationKind, bundle_sha256, validate_bytes_all, validate_bytes_for_derivation,
};
use capsulehub::{PutAuth, Secret, ServerConfig};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const PUT_TOKEN: &str = "pr8-e2e-put-token";
const DELETE_TOKEN: &str = "pr8-e2e-delete-token";
const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
/// The contract every route of the multi-D fixture declares: one observation.
const CONTRACT_OBSERVATION_ID: &str = "proof";

// ---------------------------------------------------------------------------
// Fixtures and CLI helpers
// ---------------------------------------------------------------------------

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples")
}

/// The three-route fixture: one K, static web + native macOS + native Windows.
fn multi_d_root() -> PathBuf {
    samples().join("native-desktop-multi-d")
}

/// The cross-D state fixture: one K, two native routes that both declare the
/// same Run-scoped filesystem slot.
fn stateful_root() -> PathBuf {
    samples().join("native-desktop-stateful")
}

fn home(parent: &Path) -> PathBuf {
    let home = parent.join("home");
    fs::create_dir_all(&home).expect("create the test ATO_HOME");
    home
}

/// The `PATH` the acceptance runs see: the caller's, minus every directory that
/// carries a `docker` executable.
///
/// This fixture declares no OCI route, so Docker facts never enter an
/// assertion; but on a host where the Docker CLI wedges, every `ato`
/// invocation that probes host facts stalls for the probe's timeout, and a CLI
/// shim that leaves a child holding the probe pipes can stall it outright. The
/// native app still needs `python3`, so only the Docker directory is dropped.
fn acceptance_path() -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let kept: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|directory| !directory.join("docker").is_file())
        .collect();
    std::env::join_paths(kept).unwrap_or(path)
}

fn ato(home: &Path) -> AtoCommand {
    let mut command =
        AtoCommand::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.env("ATO_HOME", home);
    command.env("PATH", acceptance_path());
    // A developer's own hub configuration must never reach an acceptance run.
    command.env_remove("ATO_HUB_URL");
    command.env_remove("ATO_HUB_TOKEN");
    command.timeout(Duration::from_secs(180));
    command
}

fn pack(home: &Path, source: &Path, bundle: &Path) -> Output {
    ato(home)
        .arg("pack")
        .arg(source)
        .arg("--output")
        .arg(bundle)
        .output()
        .expect("ato pack runs")
}

fn run_bundle(
    home: &Path,
    capsule: &Path,
    derivation: Option<&str>,
    allow_unsigned_native: bool,
    receipt: Option<&Path>,
) -> Output {
    let mut command = ato(home);
    command.arg("run").arg(capsule).arg("--no-open");
    if let Some(derivation) = derivation {
        command.args(["--derivation", derivation]);
    }
    if allow_unsigned_native {
        command.arg("--allow-unsigned-native");
    }
    if let Some(receipt) = receipt {
        command.arg("--verification-receipt").arg(receipt);
    }
    command.output().expect("ato run runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} must succeed: {}",
        stderr(output)
    );
}

/// `derivation_ref=<ref>` lines `ato pack` prints, in bundle order.
fn packed_derivations(output: &Output) -> Vec<String> {
    stdout(output)
        .lines()
        .filter_map(|line| line.strip_prefix("derivation_ref="))
        .map(str::to_owned)
        .collect()
}

fn packed_value(output: &Output, key: &str) -> String {
    let prefix = format!("{key}=");
    stdout(output)
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("`ato pack` must print {key}: {}", stdout(output)))
        .to_owned()
}

/// The DerivationRef whose capability line names `route`.
fn packed_route(output: &Output, route: &str) -> String {
    let needle = format!(" route={route} ");
    stdout(output)
        .lines()
        .find(|line| line.starts_with("capability derivation_ref=") && line.contains(&needle))
        .and_then(|line| line.strip_prefix("capability derivation_ref="))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("`ato pack` must report route {route}: {}", stdout(output)))
}

fn read_receipt(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).expect("the Run emitted a verification receipt"))
        .expect("the receipt is JSON")
}

fn receipt_observations(receipt: &serde_json::Value) -> Vec<(String, String)> {
    receipt["observations"]
        .as_array()
        .expect("the receipt carries observations")
        .iter()
        .map(|observation| {
            (
                observation["id"]
                    .as_str()
                    .expect("observation id")
                    .to_owned(),
                observation["outcome"]
                    .as_str()
                    .expect("observation outcome")
                    .to_owned(),
            )
        })
        .collect()
}

/// The newest kept `ato run` scratch state file for `slot`.
///
/// `ato run` keeps its runtime scratch under `ATO_HOME/cache/ato-portable-run-*`
/// and mounts a declared slot at `<scratch>/state/<slot>` for the Run.
fn run_scratch_state_file(home: &Path, slot: &str) -> PathBuf {
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(home.join("cache"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("ato-portable-run-")
                })
                .filter_map(|entry| {
                    let path = entry.path().join("state").join(slot).join("save.json");
                    let modified = fs::metadata(&path).ok()?.modified().ok()?;
                    Some((modified, path))
                })
                .collect()
        })
        .unwrap_or_default();
    candidates.sort_by_key(|(modified, _)| *modified);
    candidates
        .pop()
        .map(|(_, path)| path)
        .expect("the Run left the declared state slot in its scratch")
}

/// The receipt schema its own evidence requires.
///
/// This is the runtime's one rule (`lib/runtime-attempt/src/verification.rs`):
/// native execution evidence is what `/5` exists for, connector evidence `/4`,
/// certificate evidence `/3`, a portability profile `/2`, and the base receipt
/// `/1`. Deriving it here makes the assertion check the version *and* the
/// evidence together: a native Run that carried its evidence under an older
/// schema, or a receipt whose fields and version disagree, would fail.
fn expected_receipt_schema(receipt: &serde_json::Value) -> &'static str {
    let execution = receipt.get("execution");
    let field = |name: &str| execution.and_then(|execution| execution.get(name));
    if field("native").is_some() {
        return "ato.contract-verification-receipt/5";
    }
    if field("connector").is_some()
        || field("relay_image_digest").is_some()
        || field("relay_container_id").is_some()
        || field("platform_emulated").is_some()
    {
        return "ato.contract-verification-receipt/4";
    }
    let carries_tls = receipt["observations"]
        .as_array()
        .is_some_and(|observations| {
            observations.iter().any(|observation| {
                observation
                    .get("evidence")
                    .and_then(|evidence| evidence.get("tls"))
                    .is_some()
            })
        });
    if carries_tls {
        return "ato.contract-verification-receipt/3";
    }
    if field("portability_profile").is_some() {
        return "ato.contract-verification-receipt/2";
    }
    "ato.contract-verification-receipt/1"
}

fn assert_receipt_names_route(
    receipt: &serde_json::Value,
    contract_ref: &str,
    derivation_ref: &str,
) {
    assert_eq!(receipt["contract_ref"], contract_ref);
    assert_eq!(receipt["derivation_ref"], derivation_ref);
    assert_eq!(
        receipt["schema"],
        expected_receipt_schema(receipt),
        "the receipt's schema must be the one its own evidence requires"
    );
}

/// Every observation K names, from the validated Contract (not from the receipt).
fn contract_observation_ids(
    route: &ato_portable_application::ValidatedPortableApplication,
) -> Vec<String> {
    route
        .contract
        .requirements
        .iter()
        .map(|requirement| requirement.id.clone())
        .collect()
}

fn native_route<'a>(
    routes: &'a [ato_portable_application::ValidatedPortableApplication],
    os: &str,
) -> &'a ato_portable_application::ValidatedPortableApplication {
    routes
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|native| native.descriptor.os.as_str() == os)
        })
        .unwrap_or_else(|| panic!("a native route built for {os}"))
}

fn web_route(
    routes: &[ato_portable_application::ValidatedPortableApplication],
) -> &ato_portable_application::ValidatedPortableApplication {
    routes
        .iter()
        .find(|route| route.realization == PortableRealizationKind::StaticWeb)
        .expect("the static web route")
}

// ---------------------------------------------------------------------------
// Host capability, reported not assumed
// ---------------------------------------------------------------------------

fn native_facts() -> ato_host_facts::NativeFacts {
    ato_host_facts::NativeFacts::probe()
}

/// Whether this host can apply the native Adapter's mandatory policies and
/// therefore launch a native Derivation at all.
fn host_can_execute_native() -> bool {
    let facts = native_facts();
    facts.sandbox_tool && facts.signature_tools
}

fn native_capability_detail() -> String {
    let facts = native_facts();
    format!(
        "sandbox_tool={} signature_tools={} detail={}",
        facts.sandbox_tool,
        facts.signature_tools,
        facts.sandbox_detail.as_deref().unwrap_or("none")
    )
}

/// The named native admission refusal a Run produced, when it refused.
///
/// A refusal names the boundary; recording it and continuing would be the
/// failure mode the invariants forbid, so the tests that must not launch assert
/// this and that no receipt was published.
fn native_admission_refusal(output: &Output) -> Option<String> {
    let message = stderr(output);
    message
        .contains("Native Desktop runtime admission failed")
        .then_some(message)
}

fn assert_no_owned_process(home: &Path) {
    #[cfg(unix)]
    {
        thread::sleep(Duration::from_millis(300));
        let listing = Command::new("pgrep")
            .arg("-f")
            .arg(home)
            .output()
            .expect("pgrep runs");
        assert!(
            listing.status.code() == Some(1) || listing.stdout.is_empty(),
            "a Run process outlived its Run under {}:\n{}",
            home.display(),
            String::from_utf8_lossy(&listing.stdout)
        );
    }
    #[cfg(not(unix))]
    let _ = home;
}

// ---------------------------------------------------------------------------
// Bundle bytes helpers (tampering is done on the real transport bytes)
// ---------------------------------------------------------------------------

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create the copied fixture directory");
    for entry in fs::read_dir(source).expect("read the fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

/// Flip one base64 character of the payload the reference names.
///
/// The index still declares the original digest, so this is exactly "the bytes
/// in the bundle do not hash to what the descriptor pins".
fn tamper_payload(bytes: &[u8], reference: &str) -> Vec<u8> {
    let text = String::from_utf8(bytes.to_vec()).expect("portable bundles are JSON text");
    let reference_marker = format!("\"reference\":\"{reference}\"");
    let bytes_marker = "\"bytes\":\"";
    let position = text
        .rfind(&reference_marker)
        .unwrap_or_else(|| panic!("the payload for {reference} is present"));
    let start = text[..position]
        .rfind(bytes_marker)
        .unwrap_or_else(|| panic!("the payload for {reference} carries bytes"))
        + bytes_marker.len();
    // Offset into the encoded data so the flip cannot land on padding bits.
    let index = start + 8;
    let mut tampered = text.into_bytes();
    tampered[index] = if tampered[index] == b'A' { b'B' } else { b'A' };
    tampered
}

// ---------------------------------------------------------------------------
// Real self-hosted coordinator (the `ato-capsulehub` binary or its library)
// ---------------------------------------------------------------------------

struct Coordinator {
    base: String,
    storage: TempDir,
    spawned_binary: bool,
    child: Option<Child>,
    stop: Option<Arc<AtomicBool>>,
    thread: Option<thread::JoinHandle<()>>,
}

/// The coordinator binary in this test binary's target directory.
///
/// `CARGO_BIN_EXE_<name>` is only defined for this package's own binaries, so
/// the self-hosted coordinator is resolved from the target directory instead;
/// when it has not been built the same library is started in-process and the
/// test records which one ran.
fn coordinator_binary() -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(format!("ato-capsulehub{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

impl Coordinator {
    fn start() -> Self {
        let storage = tempfile::tempdir().expect("temporary coordinator storage");
        if let Some(binary) = coordinator_binary() {
            match Self::spawn_binary(&binary, &storage) {
                Ok((base, child)) => {
                    return Self {
                        base,
                        storage,
                        spawned_binary: true,
                        child: Some(child),
                        stop: None,
                        thread: None,
                    };
                }
                Err(error) => {
                    eprintln!(
                        "note: spawning {} failed ({error}); \
                         starting the same coordinator library in-process",
                        binary.display()
                    );
                }
            }
        } else {
            eprintln!(
                "note: `ato-capsulehub` is not built in this target directory; \
                 starting the same coordinator library in-process \
                 (build it with `cargo build -p capsulehub --bin ato-capsulehub`)"
            );
        }
        Self::start_in_process(storage)
    }

    fn spawn_binary(binary: &Path, storage: &TempDir) -> Result<(String, Child), String> {
        let log_path = storage.path().join("coordinator.log");
        let log = fs::File::create(&log_path).map_err(|error| error.to_string())?;
        let child = Command::new(binary)
            .args([
                "--bind",
                "127.0.0.1:0",
                "--storage",
                &storage.path().display().to_string(),
                "--put-token",
                PUT_TOKEN,
                "--delete-token",
                DELETE_TOKEN,
                "--rate-limit-per-minute",
                "0",
                "--allow-anonymous-read",
                "true",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone().map_err(|error| error.to_string())?,
            ))
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| error.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let reported = fs::read_to_string(&log_path).ok().and_then(|text| {
                text.lines()
                    .find_map(|line| line.split("listening address=").nth(1))
                    .map(str::trim)
                    .and_then(|address| address.parse::<SocketAddr>().ok())
            });
            if let Some(address) = reported {
                return Ok((format!("http://{address}"), child));
            }
            if Instant::now() > deadline {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err("the coordinator did not report a listening address".to_owned());
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn start_in_process(storage: TempDir) -> Self {
        let config = ServerConfig {
            bind: "127.0.0.1:0".parse().expect("loopback bind address"),
            storage: storage.path().to_path_buf(),
            max_object_bytes: MAX_OBJECT_BYTES,
            max_storage_bytes: None,
            put_auth: PutAuth::Token,
            put_token: Some(Secret::new(PUT_TOKEN)),
            delete_token: Some(Secret::new(DELETE_TOKEN)),
            rate_limit_per_minute: 0,
            retention: None,
            allow_anonymous_read: true,
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
            base: format!("http://{address}"),
            storage,
            spawned_binary: false,
            child: None,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn base_url(&self) -> &str {
        &self.base
    }

    fn spawned_binary(&self) -> bool {
        self.spawned_binary
    }

    fn object_file(&self, object_id: &str) -> PathBuf {
        let hex = object_id
            .strip_prefix("sha256:")
            .expect("object ids are sha256-prefixed");
        self.storage.path().join("objects").join(hex)
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
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One real HTTP request, for the operator endpoints the CLI deliberately does
/// not expose (a coordinator "takedown by object id").
fn raw_request(
    address: &str,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let authority = address.trim_start_matches("http://");
    let mut stream = TcpStream::connect(authority).expect("connect to the coordinator");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nContent-Length: 0\r\n"
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
    (
        status,
        String::from_utf8_lossy(&raw[split + 4..]).into_owned(),
    )
}

fn hub_push(home: &Path, capsule: &Path, coordinator: &Coordinator, token: Option<&str>) -> Output {
    let mut command = ato(home);
    command.args(["hub", "push"]).arg(capsule).args([
        "--hub-url",
        coordinator.base_url(),
        "--allow-loopback-http",
    ]);
    if let Some(token) = token {
        command.args(["--token", token]);
    }
    command.output().expect("ato hub push runs")
}

fn hub_pull(home: &Path, link: &str, output: &Path, token: Option<&str>) -> Output {
    let mut command = ato(home);
    command.args(["hub", "pull", link, "--allow-loopback-http"]);
    if let Some(token) = token {
        command.args(["--token", token]);
    }
    command.arg("--output").arg(output);
    command.output().expect("ato hub pull runs")
}

fn shared_link(output: &Output) -> SharedLink {
    let text = stdout(output);
    let url = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .unwrap_or_else(|| panic!("push must print a shared link, got: {text}"));
    SharedLink::parse(url).expect("the produced link parses")
}

fn retarget(link: &SharedLink, key: HubKey, cid: String) -> String {
    let base = link.base_url().expect("link base");
    SharedLink::new(&base, link.object_id.clone(), key, cid)
        .expect("well-formed link")
        .to_url(&base)
        .expect("render the link")
}

// ---------------------------------------------------------------------------
// Row 1 — same K, multiple Ds
// ---------------------------------------------------------------------------

#[test]
fn one_source_declares_three_routes_over_one_contract() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");

    let bytes = fs::read(&capsule).unwrap();
    let (bundle, routes) = validate_bytes_all(&bytes).expect("every declared Derivation validates");
    assert_eq!(routes.len(), 3, "the fixture declares three Derivations");
    assert_eq!(
        packed_derivations(&packed),
        bundle.index.derivations,
        "the CLI reports the bundle's own DerivationRefs"
    );

    // 1. Same ContractRef for every route; three distinct DerivationRefs.
    let contract_refs: std::collections::BTreeSet<_> = routes
        .iter()
        .map(|route| route.contract_ref.as_str())
        .collect();
    assert_eq!(contract_refs.len(), 1, "one Identity K");
    assert_eq!(
        contract_refs.into_iter().next().unwrap(),
        packed_value(&packed, "contract_ref")
    );
    let derivation_refs: std::collections::BTreeSet<_> = routes
        .iter()
        .map(|route| route.derivation_ref.as_str())
        .collect();
    assert_eq!(derivation_refs.len(), 3, "DerivationRefs are distinct");

    // 2. `validate_bytes_all` validates all three; `validate_bytes_for_derivation`
    //    selects exactly the requested one and validates it on its own.
    for route in &routes {
        let (_, selected) = validate_bytes_for_derivation(&bytes, route.derivation_ref.as_str())
            .expect("the selected Derivation validates on its own");
        assert_eq!(selected.derivation_ref, route.derivation_ref);
        assert_eq!(selected.contract_ref, route.contract_ref);
        assert_eq!(selected.realization, route.realization);
    }

    // 3. Selecting a route removes nothing from K: literally the same Contract.
    for route in &routes {
        assert_eq!(
            route.contract, routes[0].contract,
            "adding or selecting a D does not rewrite K"
        );
        assert_eq!(
            contract_observation_ids(route),
            [CONTRACT_OBSERVATION_ID.to_owned()]
        );
    }

    // 4. The realization kinds and the declared native platforms.
    assert_eq!(
        routes
            .iter()
            .map(|route| route.realization)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            PortableRealizationKind::StaticWeb,
            PortableRealizationKind::NativeDesktop,
        ]
        .into_iter()
        .collect()
    );
    let macos = native_route(&routes, "macos");
    let windows = native_route(&routes, "windows");
    let macos_native = macos.native.as_ref().unwrap();
    let windows_native = windows.native.as_ref().unwrap();
    assert_eq!(macos_native.descriptor.arch.as_str(), "aarch64");
    assert_eq!(macos_native.descriptor.entry, "app/run.sh");
    assert_eq!(windows_native.descriptor.arch.as_str(), "x86_64");
    assert_eq!(windows_native.descriptor.entry, "app/run.bat");
    assert_eq!(macos_native.ports[0].id, "app.http");
    assert_eq!(macos_native.ports[0].guest_port, 18080);
    assert_ne!(
        macos_native.descriptor.artifact,
        windows_native.descriptor.artifact
    );
    // The macOS artifact is unsigned, which the route declares explicitly.
    assert!(macos_native.descriptor.signature.allows_local_override());
    assert!(!windows_native.descriptor.signature.allows_local_override());

    // 5. A different fixture that declares the same K with two routes and a
    //    state slot still produces the same ContractRef: D membership and a
    //    declared state slot are not part of K.
    let stateful = scratch.path().join("stateful.capsule");
    let packed_stateful = pack(&home, &stateful_root(), &stateful);
    assert_success(&packed_stateful, "ato pack of the state fixture");
    assert_eq!(
        packed_value(&packed_stateful, "contract_ref"),
        packed_value(&packed, "contract_ref")
    );
}

// ---------------------------------------------------------------------------
// Row 2 — explicit D selection, never implicit
// ---------------------------------------------------------------------------

#[test]
fn a_run_selects_exactly_the_declared_derivation() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let web_ref = packed_route(&packed, "Static Web");
    let contract_ref = packed_value(&packed, "contract_ref");
    // The validated route, not the printed label: both native routes share the
    // printed `route=Native Desktop` label, and only the descriptor names the
    // platform.
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let windows_ref = native_route(&routes, "windows")
        .derivation_ref
        .as_str()
        .to_owned();

    // No implicit selection: a multi-route bundle refuses a Run without one.
    let receipt = scratch.path().join("no-selection.json");
    let implicit = run_bundle(&home, &capsule, None, false, Some(&receipt));
    assert!(
        !implicit.status.success(),
        "a multi-D bundle never picks a D"
    );
    assert!(
        stderr(&implicit).contains("select one with --derivation"),
        "{}",
        stderr(&implicit)
    );
    assert!(!receipt.exists(), "a refused Run publishes no receipt");

    // An undeclared reference is refused by name, before anything runs.
    let undeclared = format!("sha256:{}", "a".repeat(64));
    let unknown_receipt = scratch.path().join("unknown.json");
    let unknown = run_bundle(
        &home,
        &capsule,
        Some(&undeclared),
        false,
        Some(&unknown_receipt),
    );
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("is not declared by this bundle"),
        "{}",
        stderr(&unknown)
    );
    assert!(!unknown_receipt.exists());

    // The explicitly selected web Derivation runs and the receipt names it —
    // the same ContractRef, that DerivationRef, and nothing else.
    let web_receipt = scratch.path().join("web.json");
    let web = run_bundle(&home, &capsule, Some(&web_ref), false, Some(&web_receipt));
    assert_success(&web, "an explicitly selected static web Run");
    let receipt = read_receipt(&web_receipt);
    assert_receipt_names_route(&receipt, &contract_ref, &web_ref);
    assert_eq!(receipt["fully_satisfied"], true);

    // Selecting the Windows route on a macOS host is an admission refusal for
    // that D only: it never invalidates the bundle or another route.
    let windows_receipt = scratch.path().join("windows.json");
    let windows = run_bundle(
        &home,
        &capsule,
        Some(&windows_ref),
        false,
        Some(&windows_receipt),
    );
    if !windows.status.success() {
        assert!(
            stderr(&windows).contains("built for windows")
                || stderr(&windows).contains("native_platform_mismatch"),
            "a Windows D on a macOS host is a platform mismatch: {}",
            stderr(&windows)
        );
        assert!(!windows_receipt.exists());
    } else {
        // A Windows host: the same bundle and the same selection are admitted.
        let receipt = read_receipt(&windows_receipt);
        assert_receipt_names_route(&receipt, &contract_ref, &windows_ref);
    }
    assert_no_owned_process(&home);
}

/// Every declared execution record a successful native Run must carry.
///
/// These are evidence, never the verdict: the observations above are what
/// decides `fully_satisfied`. `override_expected` is true for the unsigned
/// fixture, whose route declares `require-valid-or-local-override`.
fn assert_native_evidence(
    receipt: &serde_json::Value,
    route: &ato_portable_application::ValidatedPortableApplication,
    override_expected: bool,
) {
    let native = route.native.as_ref().expect("the native descriptor");
    let evidence = &receipt["execution"]["native"];
    assert_eq!(evidence["artifact_ref"], native.descriptor.artifact);
    assert_eq!(evidence["entry"], native.descriptor.entry);
    assert_eq!(
        evidence["signature_policy"],
        native.descriptor.signature.as_str()
    );
    assert_eq!(
        evidence["local_override"], override_expected,
        "the local override decision is recorded, never inferred"
    );
    // `codesign` and Gatekeeper are two separate measured verdicts.
    assert!(
        matches!(
            evidence["codesign_verify"].as_str(),
            Some("valid" | "invalid")
        ),
        "{}",
        evidence["codesign_verify"]
    );
    assert!(
        matches!(
            evidence["signature_kind"].as_str(),
            Some("developer_id" | "other" | "adhoc" | "unsigned" | "unknown")
        ),
        "{}",
        evidence["signature_kind"]
    );
    assert!(
        matches!(
            evidence["gatekeeper"].as_str(),
            Some("accepted" | "rejected" | "unavailable")
        ),
        "{}",
        evidence["gatekeeper"]
    );
    // The containment that was actually applied, and the mandatory read-only /
    // read-write / inbound policy that goes with it.
    assert_eq!(evidence["sandbox"], "seatbelt");
    assert!(
        evidence["materialized_tree_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "{}",
        evidence["materialized_tree_digest"]
    );
    let inbound: Vec<u64> = evidence["enforced_inbound_ports"]
        .as_array()
        .expect("the enforced inbound ports")
        .iter()
        .map(|port| port.as_u64().expect("a port number"))
        .collect();
    assert_eq!(
        inbound,
        native
            .ports
            .iter()
            .map(|port| port.guest_port as u64)
            .collect::<Vec<_>>()
    );
    assert!(
        evidence["enforced_read_only"]
            .as_array()
            .is_some_and(|paths| paths.len() >= 2),
        "the artifact and the workspace are read-only: {}",
        evidence["enforced_read_only"]
    );
    assert!(
        evidence["process_group"]
            .as_u64()
            .is_some_and(|pgid| pgid > 1),
        "the Run records the process group it owned: {}",
        evidence["process_group"]
    );
    assert_eq!(
        evidence["window_containment"], "not enforced in this increment",
        "the receipt states what was not enforced instead of implying it"
    );
}

// ---------------------------------------------------------------------------
// Row 2 (admission scoping) — the unsigned-native override is per selected D
// ---------------------------------------------------------------------------

#[test]
fn the_unsigned_native_override_is_scoped_to_the_selected_route() {
    // `--allow-unsigned-native` is a decision about the ONE native Derivation a
    // Run selects. It is not a bundle-wide relaxation: it changes no other
    // route's admission and adds nothing to a route that never had a signature
    // policy to begin with.
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) =
        validate_bytes_all(&bytes).expect("the bundle validates with every declared D");
    let web_ref = web_route(&routes).derivation_ref.as_str().to_owned();
    let windows_ref = native_route(&routes, "windows")
        .derivation_ref
        .as_str()
        .to_owned();
    let contract_ref = packed_value(&packed, "contract_ref");

    // The static-web route with the override flag: a normal web Run. The flag
    // is irrelevant and records no native evidence.
    let plain_receipt = scratch.path().join("web-plain.json");
    let plain = run_bundle(&home, &capsule, Some(&web_ref), false, Some(&plain_receipt));
    assert_success(&plain, "the static web Run without the flag");
    let flagged_receipt = scratch.path().join("web-flagged.json");
    let flagged = run_bundle(
        &home,
        &capsule,
        Some(&web_ref),
        true,
        Some(&flagged_receipt),
    );
    assert_success(&flagged, "the static web Run with the flag");
    let plain = read_receipt(&plain_receipt);
    let flagged = read_receipt(&flagged_receipt);
    assert_eq!(plain["fully_satisfied"], true);
    assert_eq!(flagged["fully_satisfied"], true);
    assert_eq!(plain["observations"], flagged["observations"]);
    assert_eq!(plain["derivation_ref"], flagged["derivation_ref"]);
    assert!(
        flagged["execution"]["native"].is_null(),
        "a static-web Run records no native evidence: {}",
        flagged["execution"]
    );
    assert_receipt_names_route(&flagged, &contract_ref, &web_ref);

    // The Windows Derivation with the override flag: the flag cannot satisfy a
    // platform requirement. On a macOS host the D is still refused, by name,
    // and the refusal does not invalidate the bundle or the other routes (the
    // same bytes are validated above and the web route just ran from them).
    let windows_receipt = scratch.path().join("windows-flagged.json");
    let windows = run_bundle(
        &home,
        &capsule,
        Some(&windows_ref),
        true,
        Some(&windows_receipt),
    );
    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        assert!(
            !windows.status.success(),
            "a Windows D is not admitted on this host because of a flag"
        );
        assert!(
            stderr(&windows).contains("built for windows")
                || stderr(&windows).contains("native_platform_mismatch"),
            "the platform boundary must be named: {}",
            stderr(&windows)
        );
        assert!(
            !windows_receipt.exists(),
            "the override flag never turns a refusal into a receipt"
        );
        // A second static-web Run still succeeds: another route's refusal did
        // not poison the bundle.
        let again = run_bundle(&home, &capsule, Some(&web_ref), false, None);
        assert_success(&again, "the web route still runs after a native refusal");
    }
    assert_no_owned_process(&home);
}

// ---------------------------------------------------------------------------
// Row 3 — the same requirement set, from observation only
// ---------------------------------------------------------------------------

#[test]
fn the_same_requirement_set_is_verified_for_web_and_native() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let web = web_route(&routes);
    let macos = native_route(&routes, "macos");
    assert_eq!(
        web.contract, macos.contract,
        "selecting the native D does not change the requirement set"
    );

    // Static web: the real Run observes exactly K's requirement set.
    let web_receipt_path = scratch.path().join("web.json");
    let web_run = run_bundle(
        &home,
        &capsule,
        Some(web.derivation_ref.as_str()),
        false,
        Some(&web_receipt_path),
    );
    assert_success(&web_run, "the static web Run");
    let web_receipt = read_receipt(&web_receipt_path);
    assert_eq!(web_receipt["fully_satisfied"], true);
    assert_eq!(
        receipt_observations(&web_receipt),
        contract_observation_ids(web)
            .into_iter()
            .map(|id| (id, "satisfied".to_owned()))
            .collect::<Vec<_>>()
    );

    // Native macOS: the same observations, from the same common verifier.
    let native_receipt_path = scratch.path().join("native.json");
    let native_run = run_bundle(
        &home,
        &capsule,
        Some(macos.derivation_ref.as_str()),
        true,
        Some(&native_receipt_path),
    );
    if host_can_execute_native() {
        assert_success(&native_run, "the macOS native Run");
        let native_receipt = read_receipt(&native_receipt_path);
        assert_eq!(native_receipt["fully_satisfied"], true);
        assert_eq!(
            receipt_observations(&native_receipt),
            contract_observation_ids(macos)
                .into_iter()
                .map(|id| (id, "satisfied".to_owned()))
                .collect::<Vec<_>>()
        );
        // The native receipt carries the same ContractRef and the extra
        // execution evidence; the evidence is not the verdict.
        assert_receipt_names_route(
            &native_receipt,
            macos.contract_ref.as_str(),
            macos.derivation_ref.as_str(),
        );
        assert_eq!(native_receipt["execution"]["realization"], "native_desktop");
        assert_native_evidence(&native_receipt, macos, true);
        assert_no_owned_process(&home);
    } else {
        eprintln!(
            "SKIP (host capability): native launch is unavailable here: {}",
            native_capability_detail()
        );
        assert!(
            native_admission_refusal(&native_run).is_some(),
            "the native Run must fail closed with a named admission refusal: {}",
            stderr(&native_run)
        );
        assert!(
            !native_receipt_path.exists(),
            "a refused Run publishes no receipt"
        );
        assert_no_owned_process(&home);
    }

    // Negative: a bundle that validates and launches but whose K observation
    // fails is refused. Artifact integrity and a successful launch satisfy
    // nothing by themselves.
    let bad_source = scratch.path().join("bad-source");
    copy_tree(&multi_d_root(), &bad_source);
    fs::write(bad_source.join("proof.txt"), b"not-the-observed-body\n").unwrap();
    let bad_capsule = scratch.path().join("bad.capsule");
    let bad_packed = pack(&home, &bad_source, &bad_capsule);
    assert_success(&bad_packed, "ato pack of the tampered source");
    let bad_web_ref = packed_route(&bad_packed, "Static Web");
    let bad_receipt_path = scratch.path().join("bad.json");
    let bad_run = run_bundle(
        &home,
        &bad_capsule,
        Some(&bad_web_ref),
        false,
        Some(&bad_receipt_path),
    );
    assert!(
        !bad_run.status.success(),
        "an unsatisfied K observation must refuse the Run"
    );
    assert!(
        stderr(&bad_run).contains(CONTRACT_OBSERVATION_ID)
            && stderr(&bad_run).contains("http_body_digest_mismatch"),
        "the refusal must name the failed observation and the evidence: {}",
        stderr(&bad_run)
    );
    assert!(
        !bad_receipt_path.exists(),
        "a Run that fails verification publishes no accepted receipt"
    );
    assert_no_owned_process(&home);
}

// ---------------------------------------------------------------------------
// Row 4 — an invalid native route fails closed at the named boundary
// ---------------------------------------------------------------------------

#[test]
fn an_artifact_digest_mismatch_is_refused_before_admission() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let macos = native_route(&routes, "macos");
    let artifact = macos.native.as_ref().unwrap().descriptor.artifact.clone();

    let tampered = tamper_payload(&bytes, &artifact);
    let tampered_path = scratch.path().join("tampered-artifact.capsule");
    fs::write(&tampered_path, &tampered).unwrap();

    // The validator is the authority: the payload no longer hashes to what the
    // descriptor pins, so the bundle is refused before any host question.
    let error = validate_bytes_all(&tampered).expect_err("a tampered artifact must be refused");
    let message = error.to_string();
    assert!(
        message.contains("object digest mismatch") && message.contains(&artifact),
        "the digest boundary must be named: {message}"
    );

    // The CLI refuses it too, with no receipt and nothing launched.
    let receipt = scratch.path().join("receipt.json");
    let run = run_bundle(
        &home,
        &tampered_path,
        Some(macos.derivation_ref.as_str()),
        true,
        Some(&receipt),
    );
    assert!(!run.status.success());
    assert!(
        stderr(&run).contains("object digest mismatch") && stderr(&run).contains(&artifact),
        "{}",
        stderr(&run)
    );
    assert!(!receipt.exists(), "a refused bundle publishes no receipt");
    assert_no_owned_process(&home);
}

#[test]
fn an_unsigned_artifact_needs_the_declared_local_override() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let macos_ref = native_route(&routes, "macos")
        .derivation_ref
        .as_str()
        .to_owned();

    // `require-valid` (the default policy) must never accept this unsigned
    // artifact, with or without the local override flag.
    let strict_source = scratch.path().join("strict-source");
    copy_tree(&multi_d_root(), &strict_source);
    let manifest = fs::read_to_string(strict_source.join("capsule.toml"))
        .unwrap()
        .replace(
            "signature = \"require-valid-or-local-override\"",
            "signature = \"require-valid\"",
        );
    fs::write(strict_source.join("capsule.toml"), manifest).unwrap();
    let strict_capsule = scratch.path().join("strict.capsule");
    let strict_packed = pack(&home, &strict_source, &strict_capsule);
    assert_success(&strict_packed, "ato pack of the strict-signature route");
    let strict_bytes = fs::read(&strict_capsule).unwrap();
    let (_, strict_routes) = validate_bytes_all(&strict_bytes)
        .expect("an unsigned artifact is format-valid: the policy is applied at admission");
    let strict_macos = native_route(&strict_routes, "macos");
    assert!(
        !strict_macos
            .native
            .as_ref()
            .unwrap()
            .descriptor
            .signature
            .allows_local_override()
    );

    let strict_receipt = scratch.path().join("strict.json");
    let strict_run = run_bundle(
        &home,
        &strict_capsule,
        Some(strict_macos.derivation_ref.as_str()),
        true,
        Some(&strict_receipt),
    );
    assert!(
        !strict_run.status.success(),
        "require-valid must fail closed"
    );
    assert!(
        !strict_receipt.exists(),
        "a refused Run publishes no receipt"
    );
    if host_can_execute_native() {
        assert!(
            stderr(&strict_run).contains("require-valid"),
            "the signature boundary must be named: {}",
            stderr(&strict_run)
        );
    } else {
        eprintln!(
            "SKIP (host capability): the signature boundary is behind the sandbox \
             admission this host cannot pass: {}",
            native_capability_detail()
        );
        assert!(
            native_admission_refusal(&strict_run).is_some(),
            "{}",
            stderr(&strict_run)
        );
    }

    // The fixture's `require-valid-or-local-override` route: without the
    // explicit flag it is refused; with it, a capable host runs it and records
    // that the decision was a local override.
    let no_flag_receipt = scratch.path().join("no-flag.json");
    let no_flag = run_bundle(
        &home,
        &capsule,
        Some(macos_ref.as_str()),
        false,
        Some(&no_flag_receipt),
    );
    assert!(
        !no_flag.status.success(),
        "an unsigned artifact without the explicit override must not run"
    );
    assert!(!no_flag_receipt.exists());
    if host_can_execute_native() {
        assert!(
            stderr(&no_flag).contains("local override"),
            "the explicit override must be required by name: {}",
            stderr(&no_flag)
        );
    } else {
        eprintln!(
            "SKIP (host capability): the override-recording leg needs a native launch: {}",
            native_capability_detail()
        );
        assert!(native_admission_refusal(&no_flag).is_some());
    }
    assert_no_owned_process(&home);
}

#[test]
fn a_host_that_cannot_enforce_the_native_policy_refuses_admission() {
    // This is the fail-closed rule that is observable on every host: when the
    // mandatory sandbox/egress/lifecycle policy cannot be enforced before
    // launch, admission refuses and records nothing as satisfied.
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let macos_ref = native_route(&routes, "macos")
        .derivation_ref
        .as_str()
        .to_owned();

    let receipt = scratch.path().join("receipt.json");
    let run = run_bundle(&home, &capsule, Some(&macos_ref), true, Some(&receipt));
    if host_can_execute_native() {
        // A capable host applies the policy and runs the route; the mandatory
        // policies are then recorded as enforced, not claimed.
        assert_success(&run, "the native Run on a capable host");
        let receipt = read_receipt(&receipt);
        assert_eq!(receipt["execution"]["native"]["sandbox"], "seatbelt");
        assert_eq!(
            receipt["execution"]["native"]["window_containment"],
            "not enforced in this increment"
        );
        assert_no_owned_process(&home);
        return;
    }
    eprintln!(
        "SKIP (host capability): asserting the enforced-policy evidence needs a launch: {}",
        native_capability_detail()
    );
    assert!(
        native_admission_refusal(&run).is_some(),
        "the mandatory policy that cannot be enforced must refuse admission: {}",
        stderr(&run)
    );
    assert!(!receipt.exists(), "admission refusal publishes no receipt");
    assert_no_owned_process(&home);
}

// ---------------------------------------------------------------------------
// Row 5 — lifecycle: no owned process outlives the Run
// ---------------------------------------------------------------------------
//
// The three triggers the design names are separate tests so a failure names the
// trigger. Each needs a real launch, so each is gated on the measured
// capability; the refusal path is asserted on every host.

/// Build a fixture source whose macOS artifact is `script`.
///
/// The artifact is a real `tar.gz` built with the host's own `tar`: the
/// workspace declares no build step for it, and acceptance has to launch a
/// digest-pinned blob, not a script written next to one.
#[cfg(unix)]
fn macos_artifact_source(scratch: &Path, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let source = scratch.join(name);
    copy_tree(&multi_d_root(), &source);
    let staging = scratch.join(format!("{name}-staging"));
    let app = staging.join("app");
    fs::create_dir_all(&app).expect("create the artifact tree");
    let wrapper = app.join("run.sh");
    fs::write(&wrapper, script).expect("write the app wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
        .expect("make the wrapper executable");
    // The body K observes, identical to the fixture's own proof document.
    fs::write(app.join("proof.txt"), b"native-desktop-proof\n").expect("write the proof document");
    let archive = staging.join("app-macos.tar.gz");
    let status = Command::new("tar")
        .arg("czf")
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .arg("app")
        .env("COPYFILE_DISABLE", "1")
        .status()
        .expect("the host has tar");
    assert!(status.success(), "tar must build the artifact");
    fs::copy(&archive, source.join("artifacts/app-macos.tar.gz"))
        .expect("install the artifact in the fixture");
    source
}

/// `pgrep -f <marker>` finds nothing: nothing that Run started is still alive.
fn assert_no_process_matching(marker: &str) {
    #[cfg(unix)]
    {
        thread::sleep(Duration::from_millis(300));
        let listing = Command::new("pgrep")
            .arg("-f")
            .arg(marker)
            .output()
            .expect("pgrep runs");
        assert!(
            listing.status.code() == Some(1) || listing.stdout.is_empty(),
            "a process matching `{marker}` outlived its Run:\n{}",
            String::from_utf8_lossy(&listing.stdout)
        );
    }
    #[cfg(not(unix))]
    let _ = marker;
}

#[test]
fn a_native_run_leaves_no_owned_process() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let macos_ref = native_route(&routes, "macos")
        .derivation_ref
        .as_str()
        .to_owned();

    let receipt = scratch.path().join("receipt.json");
    if !host_can_execute_native() {
        eprintln!(
            "SKIP (host capability): the process-group reap assertion needs a launch: {}",
            native_capability_detail()
        );
        let run = run_bundle(&home, &capsule, Some(&macos_ref), true, Some(&receipt));
        assert!(native_admission_refusal(&run).is_some(), "{}", stderr(&run));
        assert!(!receipt.exists());
        assert_no_owned_process(&home);
        return;
    }

    // Stop: the wrapper leaves a grandchild in the Run's process group, and the
    // Run's stop must reap the whole group.
    #[cfg(unix)]
    {
        let source = macos_artifact_source(
            scratch.path(),
            "stop-source",
            r#"#!/bin/sh
/usr/bin/python3 -m http.server 18080 --bind 127.0.0.1 --directory "$(dirname "$0")" &
/usr/bin/python3 -c 'import time; time.sleep(300)' pr8-lifecycle-grandchild &
wait
"#,
        );
        let capsule = scratch.path().join("stop.capsule");
        let packed = pack(&home, &source, &capsule);
        assert_success(&packed, "ato pack of the grandchild fixture");
        let bytes = fs::read(&capsule).unwrap();
        let (_, routes) = validate_bytes_all(&bytes).unwrap();
        let macos_ref = native_route(&routes, "macos")
            .derivation_ref
            .as_str()
            .to_owned();

        let receipt = scratch.path().join("stop.json");
        let run = run_bundle(&home, &capsule, Some(&macos_ref), true, Some(&receipt));
        assert_success(&run, "the native Run whose app leaves a grandchild");
        let receipt_body = read_receipt(&receipt);
        assert_eq!(receipt_body["fully_satisfied"], true);
        let macos_route = native_route(&routes, "macos");
        assert_native_evidence(&receipt_body, macos_route, true);
        let process_group = receipt_body["execution"]["native"]["process_group"]
            .as_u64()
            .expect("the native receipt records the owned process group");
        assert_no_process_matching("pr8-lifecycle-grandchild");
        assert_no_owned_process(&home);
        let alive = Command::new("kill")
            .args(["-0", &process_group.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!alive, "the recorded process group outlived its Run");
    }
    #[cfg(not(unix))]
    let _ = &receipt;
}

#[test]
fn a_crashed_native_leader_leaves_no_owned_process() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let macos_ref = native_route(&routes, "macos")
        .derivation_ref
        .as_str()
        .to_owned();

    if !host_can_execute_native() {
        eprintln!(
            "SKIP (host capability): the crash-reap assertion needs a launch: {}",
            native_capability_detail()
        );
        let receipt = scratch.path().join("refused.json");
        let run = run_bundle(&home, &capsule, Some(&macos_ref), true, Some(&receipt));
        assert!(native_admission_refusal(&run).is_some(), "{}", stderr(&run));
        assert!(!receipt.exists());
        assert_no_owned_process(&home);
        return;
    }

    // Crash: the leader exits while its children keep serving. The Run must
    // still own and reap the group; a crashed leader is not a reason to leave
    // its children running.
    #[cfg(unix)]
    {
        let source = macos_artifact_source(
            scratch.path(),
            "crash-source",
            r#"#!/bin/sh
/usr/bin/python3 -m http.server 18080 --bind 127.0.0.1 --directory "$(dirname "$0")" &
/usr/bin/python3 -c 'import time; time.sleep(300)' pr8-crash-grandchild &
sleep 1
exit 1
"#,
        );
        let capsule = scratch.path().join("crash.capsule");
        let packed = pack(&home, &source, &capsule);
        assert_success(&packed, "ato pack of the crashing fixture");
        let bytes = fs::read(&capsule).unwrap();
        let (_, routes) = validate_bytes_all(&bytes).unwrap();
        let macos_ref = native_route(&routes, "macos")
            .derivation_ref
            .as_str()
            .to_owned();

        let receipt = scratch.path().join("crash.json");
        let run = run_bundle(&home, &capsule, Some(&macos_ref), true, Some(&receipt));
        // Whether the Run accepted the observation before the leader exited is
        // not this row's question: the owned group must be gone either way.
        let _ = run.status;
        assert_no_process_matching("pr8-crash-grandchild");
        assert_no_owned_process(&home);
        if receipt.exists() {
            assert_eq!(read_receipt(&receipt)["fully_satisfied"], true);
        }
    }
}

#[test]
fn a_native_run_that_never_serves_is_reaped_at_the_readiness_timeout() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let macos_ref = native_route(&routes, "macos")
        .derivation_ref
        .as_str()
        .to_owned();

    if !host_can_execute_native() {
        eprintln!(
            "SKIP (host capability): the readiness-timeout reap needs a launch: {}",
            native_capability_detail()
        );
        let receipt = scratch.path().join("refused.json");
        let run = run_bundle(&home, &capsule, Some(&macos_ref), true, Some(&receipt));
        assert!(native_admission_refusal(&run).is_some(), "{}", stderr(&run));
        assert!(!receipt.exists());
        assert_no_owned_process(&home);
        return;
    }

    // Timeout: the app never accepts TCP, so the Run fails at its bounded
    // readiness gate and must reap what it started. This leg is slower by the
    // readiness timeout; the assertion is that it is bounded and leaves nothing.
    #[cfg(unix)]
    {
        let source = macos_artifact_source(
            scratch.path(),
            "timeout-source",
            r#"#!/bin/sh
/usr/bin/python3 -c 'import time; time.sleep(300)' pr8-timeout-child &
wait
"#,
        );
        let capsule = scratch.path().join("timeout.capsule");
        let packed = pack(&home, &source, &capsule);
        assert_success(&packed, "ato pack of the silent fixture");
        let bytes = fs::read(&capsule).unwrap();
        let (_, routes) = validate_bytes_all(&bytes).unwrap();
        let macos_ref = native_route(&routes, "macos")
            .derivation_ref
            .as_str()
            .to_owned();

        let receipt = scratch.path().join("timeout.json");
        let run = run_bundle(&home, &capsule, Some(&macos_ref), true, Some(&receipt));
        assert!(
            !run.status.success(),
            "a native app that never serves must not be accepted"
        );
        assert!(
            stderr(&run).contains("did not accept TCP"),
            "the readiness gate must be named: {}",
            stderr(&run)
        );
        assert!(!receipt.exists(), "a failed Run publishes no receipt");
        assert_no_process_matching("pr8-timeout-child");
        assert_no_owned_process(&home);
    }
}

// ---------------------------------------------------------------------------
// Row 6 — Instance state continuity between Derivations
// ---------------------------------------------------------------------------

#[test]
fn instance_state_survives_a_derivation_switch_when_both_declare_the_slot() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("stateful.capsule");
    let packed = pack(&home, &stateful_root(), &capsule);
    assert_success(&packed, "ato pack");
    let bytes = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&bytes).unwrap();
    let macos_ref = native_route(&routes, "macos")
        .derivation_ref
        .as_str()
        .to_owned();
    let windows_ref = native_route(&routes, "windows")
        .derivation_ref
        .as_str()
        .to_owned();
    for route in &routes {
        let slots = &route.derivation.state;
        assert_eq!(slots.len(), 1, "both routes declare the shared slot");
        assert_eq!(slots[0].id, "save");
        assert_eq!(slots[0].protocol, "ato.state.filesystem@1");
        assert_eq!(
            slots[0].mount_required,
            Some(false),
            "the native Adapter delivers the slot by path"
        );
    }

    // Import selects a D explicitly; a native D is format-valid everywhere, so
    // import succeeds even on a host that cannot launch it.
    let imported = ato(&home)
        .args(["app", "import"])
        .arg(&capsule)
        .args(["--derivation", &macos_ref])
        .output()
        .expect("ato app import runs");
    assert_success(&imported, "app import selecting the macOS Derivation");
    let instance: serde_json::Value = serde_json::from_slice(&imported.stdout).unwrap();
    let instance_id = instance["instance_id"].as_str().unwrap().to_owned();
    assert_eq!(instance["selected_derivation_ref"], macos_ref);

    // A durable start of the unsigned native route fails closed: no receipt,
    // and no saved state is invented for it.
    let start = ato(&home)
        .args(["app", "start", &instance_id, "--no-open"])
        .output()
        .expect("ato app start runs");
    assert!(
        !start.status.success(),
        "the unsigned native route must not start through a durable Instance"
    );
    assert_no_owned_process(&home);

    // Live: the selected native Derivation really receives the declared slot at
    // run time. `ato run` is ephemeral, so the wrapper's write lands in the
    // Run's own scratch state directory — that is the delivery this asserts,
    // and it is real runtime evidence, not an API-level claim.
    if host_can_execute_native() {
        let receipt_path = scratch.path().join("native-state.json");
        let live = run_bundle(
            &home,
            &capsule,
            Some(macos_ref.as_str()),
            true,
            Some(&receipt_path),
        );
        assert_success(&live, "the native Run of the stateful fixture");
        let receipt = read_receipt(&receipt_path);
        assert_eq!(receipt["fully_satisfied"], true);
        let state_file = run_scratch_state_file(&home, "save");
        assert_eq!(
            fs::read(&state_file).expect("the workload wrote its declared state slot"),
            b"{\"progress\":\"native-run\",\"source\":\"native-macos\"}\n",
            "the native route wrote the slot it declares, at the path it was given"
        );
        let enforced: Vec<String> = receipt["execution"]["native"]["enforced_read_write"]
            .as_array()
            .expect("the enforced writable paths")
            .iter()
            .map(|path| path.as_str().expect("a path").to_owned())
            .collect();
        assert!(
            enforced.iter().any(|path| path.ends_with("/state/save")),
            "the state slot was writable inside the sandbox: {enforced:?}"
        );
    } else {
        eprintln!(
            "SKIP (host capability): the native slot-delivery leg needs a launch: {}",
            native_capability_detail()
        );
    }

    // Drive the save/restore path through the real Instance APIs.
    let store = LocalApplicationStore::open(&home).expect("open the Instance store");
    let state_dir = home
        .join("instances")
        .join(&instance_id)
        .join("state")
        .join("save");
    assert!(
        state_dir.is_dir(),
        "import prepared the declared slot directory"
    );
    // A workload cannot be launched on every host (see the module docs), so the
    // bytes the native route would write into its declared Run state directory
    // are placed here. Everything that decides save/restore compatibility,
    // snapshot capture and digest observation below goes through the runtime.
    let workload_state = br#"{"progress":"chapter-2","score":4200}"#.to_vec();
    fs::write(state_dir.join("save.json"), &workload_state).unwrap();

    let run = store.claim_run(&instance_id).expect("claim a Run lease");
    let saved = store
        .save_filesystem_state_for_run(&instance_id, &run.token)
        .expect("save the Run-scoped filesystem state")
        .expect("the selected Derivation declares a slot");
    assert!(
        saved.data_snapshot_ref.is_some(),
        "saving produced saved data on the Instance"
    );
    store
        .release_run(&instance_id, &run.token)
        .expect("release the Run");

    // The saved data declares the slot and the format the route implies.
    let restored = store
        .restored_snapshot(&instance_id)
        .expect("read the restored snapshot")
        .expect("the Instance carries restored saved data");
    assert_eq!(restored.resources.len(), 1);
    assert_eq!(restored.resources[0].slot, "save");
    assert_eq!(restored.resources[0].protocol, "ato.state.filesystem@1");
    assert_eq!(
        restored.resources[0].save, None,
        "a route that declares no save format keeps the legacy declaration"
    );
    // Restore is observed from the bytes the Run will import, not from an index.
    store
        .observe_restored_digests(&instance_id)
        .expect("the restored state observes the saved digest");

    // The PR7 cross-D gate: the other Derivation declares the same slot and
    // format, so a switch is compatible — no migration, no silent drop.
    assert_eq!(
        store
            .state_compatibility_for_derivation(&instance_id, &windows_ref)
            .expect("compare the saved data with the other Derivation"),
        StateCompatibility::Compatible,
        "both Derivations declare the same slot and save format"
    );

    // A Restore that does not observe its saved digest is a failure, never a
    // success on the strength of the index.
    fs::write(state_dir.join("save.json"), b"{}").unwrap();
    let error = store
        .observe_restored_digests(&instance_id)
        .expect_err("a changed restored state must not observe the saved digest");
    assert_eq!(
        error.state_continuity_code(),
        Some("state-restore-digest-mismatch"),
        "{error}"
    );
    // Put the Run's state back so the export below carries what was saved.
    fs::write(state_dir.join("save.json"), &workload_state).unwrap();

    // Export the saved Instance and restore it by importing while selecting the
    // OTHER Derivation: the saved bytes are restored through a different D.
    let exported = store
        .export_instance(&instance_id)
        .expect("export the Instance with its saved data");
    let exported_path = scratch.path().join("exported.capsule");
    fs::write(&exported_path, &exported).unwrap();
    let (exported_bundle, exported_routes) =
        ato_portable_application::validate_bytes_all(&exported)
            .expect("the exported snapshot bundle validates");
    assert_eq!(exported_bundle.index.version, 4, "saved data lives in v4");
    // The v4 saved-data Contract: saving does not rewrite the source K, it
    // forms `K_saved` — the same Identity plus the `instance-snapshot@1`
    // requirement bound to the saved digest. The source bundle on disk is
    // byte-for-byte untouched and still declares the original K.
    assert_eq!(
        fs::read(&capsule).unwrap(),
        bytes,
        "saving data never rewrites the source bundle"
    );
    assert_eq!(
        exported_bundle.index.application_ref, instance["application_ref"],
        "the Application identity is unchanged by saving data"
    );
    assert_ne!(
        exported_bundle.index.root_contract_ref, instance["contract_ref"],
        "K_saved adds the snapshot requirement, so it is a new Contract point"
    );
    let saved_contract_ids: Vec<String> = exported_routes[0]
        .contract
        .requirements
        .iter()
        .map(|requirement| requirement.id.clone())
        .collect();
    assert!(
        saved_contract_ids.contains(&CONTRACT_OBSERVATION_ID.to_owned())
            && saved_contract_ids
                .iter()
                .any(|id| id == "instance-snapshot"),
        "K_saved keeps every original requirement and binds the snapshot: {saved_contract_ids:?}"
    );

    let switched = ato(&home)
        .args(["app", "import"])
        .arg(&exported_path)
        .args(["--derivation", &windows_ref])
        .output()
        .expect("ato app import runs");
    assert_success(&switched, "importing the saved Instance under the other D");
    let switched_instance: serde_json::Value = serde_json::from_slice(&switched.stdout).unwrap();
    let switched_id = switched_instance["instance_id"].as_str().unwrap();
    assert_eq!(
        switched_instance["selected_derivation_ref"], windows_ref,
        "the restore was performed by a different Derivation"
    );
    assert_ne!(
        switched_instance["instance_id"], instance_id,
        "a restore creates an independent Instance"
    );
    let restored_state = home
        .join("instances")
        .join(switched_id)
        .join("state")
        .join("save")
        .join("save.json");
    assert_eq!(
        fs::read(&restored_state).expect("the saved state was restored through the other D"),
        workload_state,
        "the bytes another Derivation saved are what this one restored"
    );
    let switched_store = LocalApplicationStore::open(&home).expect("open the Instance store");
    switched_store
        .observe_restored_digests(switched_id)
        .expect("the importing Instance observes the digests of what it restored");
    assert_no_owned_process(&home);

    // Partiality, stated where the gap is: the same round trip through the
    // static-web route is not implementable in v0 (a web route declares no
    // filesystem state and its durable state is browser localStorage), and no
    // native workload was executed on this host.
    let live = if host_can_execute_native() {
        "a live native Run executed and wrote the declared slot at run time"
    } else {
        "no native workload executed on this host"
    };
    eprintln!(
        "state row: save/restore/switch verified at the Instance level ({live}); \
         the web -> native -> web runtime round trip is NOT met in v0: a \
         static-web route has no filesystem-state delivery or shared save \
         format, and no durable Instance start accepts an unsigned native \
         artifact, so a restored snapshot is not consumed by a native workload"
    );
}

// ---------------------------------------------------------------------------
// Rows 7, 8, 10 — distribution through the self-hosted coordinator
// ---------------------------------------------------------------------------

#[test]
fn a_distributed_three_route_bundle_keeps_its_bytes_contract_and_routes() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let original = fs::read(&capsule).unwrap();
    let (_, routes) = validate_bytes_all(&original).unwrap();
    let web_ref = web_route(&routes).derivation_ref.as_str().to_owned();
    let macos_ref = native_route(&routes, "macos")
        .derivation_ref
        .as_str()
        .to_owned();
    let contract_ref = packed_value(&packed, "contract_ref");

    let coordinator = Coordinator::start();
    eprintln!(
        "coordinator: {} ({})",
        coordinator.base_url(),
        if coordinator.spawned_binary() {
            "spawned ato-capsulehub binary"
        } else {
            "in-process capsulehub library (same code)"
        }
    );

    let pushed = hub_push(&home, &capsule, &coordinator, Some(PUT_TOKEN));
    assert_success(&pushed, "ato hub push");
    let link = shared_link(&pushed);
    assert_eq!(link.cid, contract_ref, "the link claims the bundle's K");

    // The coordinator stored ciphertext only, addressed by its digest.
    let objects = coordinator.object_files();
    assert_eq!(objects.len(), 1);
    let stored = fs::read(&objects[0]).unwrap();
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(&stored)),
        link.object_id.as_str()
    );
    assert!(stored.starts_with(b"ATOHUB"));
    assert_ne!(stored, original, "the coordinator never stores the bundle");
    assert!(coordinator.partial_files().is_empty());

    let pulled_path = scratch.path().join("pulled.capsule");
    let link_text = stdout(&pushed)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .unwrap()
        .to_owned();
    let pulled = hub_pull(&home, &link_text, &pulled_path, Some(PUT_TOKEN));
    assert_success(&pulled, "ato hub pull");
    let pulled_bytes = fs::read(&pulled_path).unwrap();
    assert_eq!(pulled_bytes, original, "byte-identical transport equality");
    assert_eq!(
        bundle_sha256(&pulled_bytes),
        bundle_sha256(&original),
        "the same bundle bytes"
    );
    assert!(
        stdout(&pulled).contains(&format!("contract_ref={contract_ref}")),
        "pull reports the same ContractRef: {}",
        stdout(&pulled)
    );

    // The pulled capsule still validates all three Derivations...
    let (_, pulled_routes) = validate_bytes_all(&pulled_bytes)
        .expect("the pulled capsule validates every declared Derivation");
    assert_eq!(pulled_routes.len(), 3);
    assert_eq!(
        pulled_routes
            .iter()
            .map(|route| route.derivation_ref.clone())
            .collect::<Vec<_>>(),
        routes
            .iter()
            .map(|route| route.derivation_ref.clone())
            .collect::<Vec<_>>()
    );

    // ...and the macOS Derivation is admitted exactly as before distribution:
    // the static route runs, the native route's host question is unchanged.
    let web_receipt_path = scratch.path().join("pulled-web.json");
    let web_receipt = run_bundle(
        &home,
        &pulled_path,
        Some(&web_ref),
        false,
        Some(&web_receipt_path),
    );
    assert_success(&web_receipt, "the pulled capsule runs its static route");
    let receipt = read_receipt(&web_receipt_path);
    assert_receipt_names_route(&receipt, &contract_ref, &web_ref);
    assert_eq!(receipt["fully_satisfied"], true);

    let native_receipt = scratch.path().join("pulled-native.json");
    let native_run = run_bundle(
        &home,
        &pulled_path,
        Some(&macos_ref),
        true,
        Some(&native_receipt),
    );
    if host_can_execute_native() {
        assert_success(&native_run, "the pulled capsule runs its native route");
        let receipt = read_receipt(&native_receipt);
        assert_receipt_names_route(&receipt, &contract_ref, &macos_ref);
        assert_eq!(receipt["fully_satisfied"], true);
        let pulled_macos = native_route(&pulled_routes, "macos");
        assert_eq!(
            receipt_observations(&receipt),
            contract_observation_ids(pulled_macos)
                .into_iter()
                .map(|id| (id, "satisfied".to_owned()))
                .collect::<Vec<_>>()
        );
        assert_native_evidence(&receipt, pulled_macos, true);
    } else {
        eprintln!(
            "SKIP (host capability): the pulled native route still cannot launch here: {}",
            native_capability_detail()
        );
        assert!(native_admission_refusal(&native_run).is_some());
        assert!(!native_receipt.exists());
    }
    assert_no_owned_process(&home);

    // Hosted operation: the distribution above ran against the same
    // coordinator the hosted path runs. When the binary could not be built the
    // in-process library is that same code, and the run records which ran.
    if !coordinator.spawned_binary() {
        eprintln!(
            "hosted path: exercised through the in-process `capsulehub` library; \
             the `ato-capsulehub` binary is a thin `main` over the same `serve`"
        );
    }
}

#[test]
fn a_tampered_distribution_is_refused_at_each_boundary() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");

    let coordinator = Coordinator::start();
    let pushed = hub_push(&home, &capsule, &coordinator, Some(PUT_TOKEN));
    assert_success(&pushed, "ato hub push");
    let link_text = stdout(&pushed)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .unwrap()
        .to_owned();
    let link = SharedLink::parse(&link_text).unwrap();

    // 1. Ciphertext changed under the declared object id.
    let object = coordinator.object_file(link.object_id.as_str());
    let mut tampered = fs::read(&object).unwrap();
    for byte in &mut tampered {
        *byte ^= 0xa5;
    }
    fs::write(&object, &tampered).unwrap();
    let ciphertext_output = scratch.path().join("ciphertext.capsule");
    let ciphertext = hub_pull(&home, &link_text, &ciphertext_output, Some(PUT_TOKEN));
    assert!(!ciphertext.status.success());
    assert!(
        stderr(&ciphertext).contains("ciphertext digest mismatch"),
        "the ciphertext digest boundary must be named: {}",
        stderr(&ciphertext)
    );
    assert!(!ciphertext_output.exists());
    // A fresh push (new link key, so a new ciphertext object) covers the other
    // boundaries; the corrupted object stays corrupted.
    let fresh = hub_push(&home, &capsule, &coordinator, Some(PUT_TOKEN));
    assert_success(&fresh, "ato hub push");
    let fresh_link = shared_link(&fresh);

    // 2. Wrong key: authentication fails and no plaintext is written.
    let replacement = HubKey::generate().unwrap();
    let replacement_secret = replacement.to_fragment_secret();
    let wrong_key_link = retarget(&fresh_link, replacement, fresh_link.cid.clone());
    let key_output = scratch.path().join("wrong-key.capsule");
    let wrong_key = hub_pull(&home, &wrong_key_link, &key_output, Some(PUT_TOKEN));
    assert!(!wrong_key.status.success());
    let key_message = stderr(&wrong_key);
    assert!(
        key_message.contains("decrypt") || key_message.contains("authentication"),
        "the authentication boundary must be named: {key_message}"
    );
    assert!(!key_output.exists());
    assert!(
        !key_message.contains(&replacement_secret)
            && !key_message.contains(&fresh_link.key.to_fragment_secret()),
        "key material must never appear in an error message"
    );

    // 3. Wrong CID: the recomputed ContractRef is the authority.
    let wrong_cid_link = retarget(
        &fresh_link,
        HubKey::from_fragment_secret(&fresh_link.key.to_fragment_secret()).unwrap(),
        format!("sha256:{}", "b".repeat(64)),
    );
    let cid_output = scratch.path().join("wrong-cid.capsule");
    let wrong_cid = hub_pull(&home, &wrong_cid_link, &cid_output, Some(PUT_TOKEN));
    assert!(!wrong_cid.status.success());
    assert!(
        stderr(&wrong_cid).contains("capsule identity mismatch"),
        "the identity boundary must be named: {}",
        stderr(&wrong_cid)
    );
    assert!(!cid_output.exists());

    // 4. Operator takedown by object id is the only removal path, and it is
    //    credential-gated. The CLI has no delete command by design.
    let target = format!("/v1/capsulehub/objects/{}", fresh_link.object_id);
    assert_eq!(
        raw_request(coordinator.base_url(), "DELETE", &target, &[]).0,
        403
    );
    let operator = format!("Bearer {DELETE_TOKEN}");
    assert_eq!(
        raw_request(
            coordinator.base_url(),
            "DELETE",
            &target,
            &[("Authorization", &operator)]
        )
        .0,
        204
    );
    let removed_output = scratch.path().join("removed.capsule");
    let fresh_link_text = stdout(&fresh)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .unwrap()
        .to_owned();
    let removed = hub_pull(&home, &fresh_link_text, &removed_output, Some(PUT_TOKEN));
    assert!(!removed.status.success());
    assert!(
        stderr(&removed).contains("does not have object"),
        "a taken-down object must be reported by name: {}",
        stderr(&removed)
    );
    assert!(!removed_output.exists());
}

// ---------------------------------------------------------------------------
// Row 9 — only the declared portable profiles are accepted
// ---------------------------------------------------------------------------

#[test]
fn only_the_declared_portable_profiles_are_distributed() {
    let scratch = tempfile::tempdir().unwrap();
    let home = home(scratch.path());
    let capsule = scratch.path().join("multi-d.capsule");
    let packed = pack(&home, &multi_d_root(), &capsule);
    assert_success(&packed, "ato pack");
    let coordinator = Coordinator::start();

    // v2 (computation-root) is rejected by name and stores nothing.
    let v2 = scratch.path().join("v2.capsule");
    fs::write(
        &v2,
        format!(
            "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
            "0".repeat(64)
        ),
    )
    .unwrap();
    let v2_push = hub_push(&home, &v2, &coordinator, Some(PUT_TOKEN));
    assert!(!v2_push.status.success());
    assert!(
        stderr(&v2_push).contains("computation-root wire v2"),
        "{}",
        stderr(&v2_push)
    );

    // An unknown profile is rejected by name and stores nothing.
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&capsule).unwrap()).unwrap();
    document["index"]["profile"] =
        serde_json::Value::String("ato.portable-application/999".to_owned());
    let unknown = scratch.path().join("unknown.capsule");
    fs::write(&unknown, serde_json::to_vec(&document).unwrap()).unwrap();
    let unknown_push = hub_push(&home, &unknown, &coordinator, Some(PUT_TOKEN));
    assert!(!unknown_push.status.success());
    assert!(
        stderr(&unknown_push).contains("profile"),
        "{}",
        stderr(&unknown_push)
    );
    assert!(
        coordinator.object_files().is_empty(),
        "a refused push stores no object"
    );
    assert!(coordinator.partial_files().is_empty());

    // v3 is accepted, and the whole distribution round trip works.
    let v3_push = hub_push(&home, &capsule, &coordinator, Some(PUT_TOKEN));
    assert_success(&v3_push, "v3 push");
    let v3_link = shared_link(&v3_push);
    assert_eq!(v3_link.cid, packed_value(&packed, "contract_ref"));

    // v4 (portable application /2, saved-data transport) is accepted too, with
    // the same ContractRef and the same routes.
    let v4_path = scratch.path().join("v4.capsule");
    let exported = ato(&home)
        .arg("export")
        .arg(&capsule)
        .args(["--portability", "cached", "--output"])
        .arg(&v4_path)
        .output()
        .expect("ato export runs");
    assert_success(&exported, "ato export to the v4 transport");
    let v4_bytes = fs::read(&v4_path).unwrap();
    let (v4_bundle, v4_routes) =
        validate_bytes_all(&v4_bytes).expect("a v4 bundle validates every Derivation");
    assert_eq!(v4_bundle.index.version, 4);
    assert_eq!(
        v4_bundle.index.profile.as_str(),
        "ato.portable-application/2"
    );
    assert_eq!(v4_routes.len(), 3);
    assert_eq!(
        v4_bundle.index.root_contract_ref,
        packed_value(&packed, "contract_ref")
    );
    let v4_push = hub_push(&home, &v4_path, &coordinator, Some(PUT_TOKEN));
    assert_success(&v4_push, "v4 push");
    assert_eq!(
        shared_link(&v4_push).cid,
        packed_value(&packed, "contract_ref")
    );

    // The v4 bytes distributed above still run their static route.
    let v4_pulled_path = scratch.path().join("v4-pulled.capsule");
    let v4_link_text = stdout(&v4_push)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .unwrap()
        .to_owned();
    let v4_pulled = hub_pull(&home, &v4_link_text, &v4_pulled_path, Some(PUT_TOKEN));
    assert_success(&v4_pulled, "v4 pull");
    assert_eq!(fs::read(&v4_pulled_path).unwrap(), v4_bytes);
    let web_ref = packed_route(&packed, "Static Web");
    let receipt = scratch.path().join("v4-web.json");
    let v4_run = run_bundle(
        &home,
        &v4_pulled_path,
        Some(&web_ref),
        false,
        Some(&receipt),
    );
    assert_success(&v4_run, "the v4 capsule runs its static route");
    assert_eq!(read_receipt(&receipt)["fully_satisfied"], true);
}
