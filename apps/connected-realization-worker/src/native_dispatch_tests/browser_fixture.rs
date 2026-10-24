//! Test-only browser coordination around the real owned Native lifecycle.
//! No tokens/keys enter files, no caller-provided expected values authorize work.
use super::*;
use std::{
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub(super) const KEY: &str = "native-browser-joint-fixture-key-not-an-operator-secret-0001";
const MAX_SECONDS: u64 = 90;
pub(super) const CLIPBOARD_HEX: &str = "636166e9090a";
pub(super) const WIDTH: u16 = 1024;
pub(super) const HEIGHT: u16 = 768;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Done {
    version: u8,
    action: String,
    expected_clipboard_latin1_hex: String,
    expected_width: u16,
    expected_height: u16,
}
fn validate_done(bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(bytes.len() <= 1024, "fixture done exceeds bound");
    let done: Done = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        done.version == 1
            && done.action == "verify-and-stop"
            && done.expected_clipboard_latin1_hex == CLIPBOARD_HEX
            && done.expected_width == WIDTH
            && done.expected_height == HEIGHT,
        "fixture done does not match fixed verification request"
    );
    Ok(())
}
fn origin(value: &str) -> anyhow::Result<String> {
    let url = url::Url::parse(value)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "fixture origin must be an explicit HTTPS IPv4 loopback port"
    );
    Ok(url.origin().ascii_serialization())
}

struct CleanupContext {
    public: SocketAddr,
    hidden: SocketAddr,
    work_root: PathBuf,
    lease_root: PathBuf,
    runner_id: String,
}
pub(super) fn validate_heartbeat(
    method: &str,
    path: &str,
    body: &serde_json::Value,
    runner: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        method == "POST" && path == format!("/v1/runners/{runner}/heartbeat"),
        "fixture heartbeat must belong to this Runner and use POST"
    );
    anyhow::ensure!(
        body["active_slots"] == 1 && body["max_slots"] == 1 && body["slot_recovered"] == true,
        "fixture heartbeat must describe one active recovered slot"
    );
    anyhow::ensure!(
        body["os"] == std::env::consts::OS
            && body["arch"] == std::env::consts::ARCH
            && body["agent_version"] == env!("CARGO_PKG_VERSION")
            && body["public_base_url"] == "https://native-runner.example",
        "fixture heartbeat host facts differ"
    );
    anyhow::ensure!(
        body["capabilities"]
            .as_array()
            .is_some_and(|values| values.iter().all(|v| v.is_string()))
            && body["supported_lease_kinds"].is_array()
            && body["supported_session_surfaces"].is_array()
            && body["host_resources"].is_object(),
        "fixture heartbeat collections are malformed"
    );
    Ok(())
}
pub(super) struct BrowserFixture {
    root: PathBuf,
    pub origin: String,
    clock: OnceLock<(Instant, u64)>,
    cleanup: OnceLock<CleanupContext>,
    heartbeats_acked: AtomicUsize,
    identity: OnceLock<(
        String,
        String,
        ato_ipc::session_surface::SurfaceExecutionScope,
    )>,
}
impl BrowserFixture {
    pub fn create() -> Self {
        let root = PathBuf::from(
            std::env::var_os("ATO_NATIVE_BROWSER_FIXTURE_DIR")
                .expect("explicit new own fixture directory"),
        );
        assert!(root.is_absolute());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("create new private fixture directory");
        let origin = origin(
            &std::env::var("ATO_NATIVE_BROWSER_ORIGIN")
                .unwrap_or_else(|_| "https://127.0.0.1:4179".into()),
        )
        .unwrap();
        Self {
            root,
            origin,
            clock: OnceLock::new(),
            cleanup: OnceLock::new(),
            heartbeats_acked: AtomicUsize::new(0),
            identity: OnceLock::new(),
        }
    }
    pub fn register_cleanup(
        &self,
        public: SocketAddr,
        hidden: SocketAddr,
        work_root: PathBuf,
        lease_root: PathBuf,
        runner_id: String,
    ) {
        assert!(
            self.cleanup
                .set(CleanupContext {
                    public,
                    hidden,
                    work_root,
                    lease_root,
                    runner_id
                })
                .is_ok()
        );
    }
    pub fn ack_heartbeat(&self) {
        self.heartbeats_acked.fetch_add(1, AtomicOrdering::AcqRel);
    }
    pub fn arm(&self) {
        let exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + MAX_SECONDS;
        self.clock
            .set((Instant::now() + Duration::from_secs(MAX_SECONDS), exp))
            .expect("fixture clock is armed once");
    }
    pub fn remaining(&self) -> Duration {
        self.clock
            .get()
            .unwrap()
            .0
            .saturating_duration_since(Instant::now())
    }
    pub fn expired(&self) -> bool {
        self.clock
            .get()
            .is_some_and(|(deadline, _)| Instant::now() >= *deadline)
    }
    pub fn configure(&self, command: &mut RuntimeLaunchLeaseCommand) {
        command.surface_origin = Some(self.origin.clone());
        // A bounded test-owned Run policy through the existing wire, not a
        // change to any Runner startup/ready/idle/stop deadline.
        command.max_duration_secs = Some(MAX_SECONDS);
    }
    fn atomic_json(&self, name: &str, value: &serde_json::Value) -> anyhow::Result<()> {
        let tmp = self.root.join(format!(".{name}.pending"));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        // Hard link is atomic create-new publication; never replace an old
        // path or follow a browser-controlled destination symlink.
        std::fs::hard_link(&tmp, self.root.join(name))?;
        std::fs::remove_file(tmp)?;
        Ok(())
    }
    pub fn publish(
        &self,
        public: SocketAddr,
        scope: &netd::surface_authorization::SurfaceGatewayScope,
        execution: &ato_ipc::session_surface::SurfaceExecutionScope,
    ) {
        assert!(!self.expired());
        self.identity
            .set((
                scope.session_id.clone(),
                scope.surface_id.clone(),
                execution.clone(),
            ))
            .expect("fixture identity is fixed once");
        self.atomic_json("ready.json", &json!({"version":1,"gateway_ws_url":format!("ws://{public}/__ato/surface"),
            "origin":self.origin,"session_id":scope.session_id,"surface_id":scope.surface_id,"execution":execution,
            "principal":{"kind":"user","id":"fixture-owner"},"exp_unix_sec":self.clock.get().unwrap().1,
            "initial_width":640,"initial_height":480,"done_file":self.root.join("done.json"),"result_file":self.root.join("result.json")})).unwrap();
    }
    pub fn wait_done(&self) -> anyhow::Result<u64> {
        loop {
            anyhow::ensure!(
                !self.expired(),
                "browser fixture absolute 90-second deadline expired"
            );
            match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(self.root.join("done.json"))
            {
                Ok(mut file) => {
                    let metadata = file.metadata()?;
                    anyhow::ensure!(
                        metadata.is_file()
                            && metadata.len() <= 1024
                            && metadata.uid() == std::fs::metadata(&self.root)?.uid()
                            && metadata.permissions().mode() & 0o777 == 0o600,
                        "fixture done must be an owned regular private bounded file"
                    );
                    let mut bytes = Vec::new();
                    Read::by_ref(&mut file).take(1025).read_to_end(&mut bytes)?;
                    validate_done(&bytes)?;
                    return Ok(self.clock.get().unwrap().1);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn finish(&self, success: bool) {
        let oci_cleanup = ato_adapter_oci::OwnedResourceScanner::new(
            &format!("native-dispatch-runner-{}", std::process::id()),
            "0",
        )
        .unwrap()
        .scan()
        .is_ok_and(|scan| scan.containers.is_empty() && scan.networks.is_empty());
        let cleanup = self.cleanup.get().is_some_and(|context| {
            let ports_closed = [context.public, context.hidden].into_iter().all(|addr| {
                std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_err()
            });
            let journal_empty = !context.work_root.exists()
                || runtime_launch::recovery::RunJournal::new(
                    &context.work_root,
                    &context.runner_id,
                    "0",
                )
                .and_then(|journal| journal.load())
                .is_ok_and(|journal| journal.is_empty());
            oci_cleanup && ports_closed && journal_empty && !context.lease_root.exists()
        });
        let identity = self.identity.get();
        let success = success && cleanup && identity.is_some();
        self.atomic_json("result.json", &json!({"version":1,"status":if success {"passed"} else {"failed"},"heartbeats_acked":self.heartbeats_acked.load(AtomicOrdering::Acquire),
            "session_id":identity.map(|(session,_,_)|session),"surface_id":identity.map(|(_,surface,_)|surface),
            "run_id":identity.map(|(_,_,scope)|&scope.run_id),"lease_id":identity.map(|(_,_,scope)|&scope.lease_id),"runner_id":identity.map(|(_,_,scope)|&scope.runner_id),
            "actual_clipboard_latin1_hex":success.then_some(CLIPBOARD_HEX),"actual_width":success.then_some(WIDTH),"actual_height":success.then_some(HEIGHT),
            "runtime_reconnect_verified":success,"runtime_full_frame_verified":success,"permission_denials_verified":success,"original_contract_satisfied":success,"frozen_probe_unchanged":success,"output_saved":success,
            "cleanup_confirmed":cleanup,"oci_resources_cleanup_confirmed":oci_cleanup,"error":if success {serde_json::Value::Null} else {json!("fixture_verification_failed")}})).unwrap();
        assert!(
            success,
            "browser joint fixture failed; see result and test log"
        );
    }
}
#[test]
fn done_is_strict_bounded_and_never_supplies_expected_state() {
    let valid = json!({"version":1,"action":"verify-and-stop","expected_clipboard_latin1_hex":CLIPBOARD_HEX,"expected_width":WIDTH,"expected_height":HEIGHT});
    assert!(validate_done(&serde_json::to_vec(&valid).unwrap()).is_ok());
    for field in [
        "version",
        "action",
        "expected_clipboard_latin1_hex",
        "expected_width",
        "expected_height",
        "unknown",
    ] {
        let mut value = valid.clone();
        value[field] = json!("untrusted");
        assert!(
            validate_done(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{field}"
        );
    }
    assert!(validate_done(&[b' '; 1025]).is_err());
    assert!(validate_done(br#"{"version":1,"version":1}"#).is_err());
}
#[test]
fn browser_origin_is_only_explicit_https_loopback() {
    assert_eq!(
        origin("https://127.0.0.1:4179").unwrap(),
        "https://127.0.0.1:4179"
    );
    for value in [
        "http://127.0.0.1:4179",
        "https://localhost:4179",
        "https://127.0.0.1.evil:4179",
        "https://127.0.0.1:4179/a",
        "https://u:p@127.0.0.1:4179",
        "https://127.0.0.1:4179?x",
    ] {
        assert!(origin(value).is_err());
    }
}

#[test]
fn publication_and_done_files_are_owned_private_atomic_and_refuse_links() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = BrowserFixture {
        root: temp.path().to_path_buf(),
        origin: "https://127.0.0.1:4179".into(),
        clock: OnceLock::new(),
        cleanup: OnceLock::new(),
        heartbeats_acked: AtomicUsize::new(0),
        identity: OnceLock::new(),
    };
    fixture.arm();
    fixture
        .atomic_json("ready.json", &json!({"version":1}))
        .unwrap();
    assert_eq!(
        std::fs::metadata(temp.path().join("ready.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        fixture
            .atomic_json("ready.json", &json!({"version":2}))
            .is_err()
    );
    std::os::unix::fs::symlink(
        temp.path().join("ready.json"),
        temp.path().join("done.json"),
    )
    .unwrap();
    assert!(fixture.wait_done().is_err());
    std::fs::remove_file(temp.path().join("done.json")).unwrap();
    fixture.atomic_json("done.json",&json!({"version":1,"action":"verify-and-stop","expected_clipboard_latin1_hex":CLIPBOARD_HEX,"expected_width":WIDTH,"expected_height":HEIGHT})).unwrap();
    assert!(fixture.wait_done().is_ok());
    std::fs::set_permissions(
        temp.path().join("done.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(fixture.wait_done().is_err());
}
#[test]
fn absent_done_cannot_extend_the_absolute_fixture_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = BrowserFixture {
        root: temp.path().to_path_buf(),
        origin: "https://127.0.0.1:4179".into(),
        clock: OnceLock::new(),
        cleanup: OnceLock::new(),
        heartbeats_acked: AtomicUsize::new(0),
        identity: OnceLock::new(),
    };
    fixture
        .clock
        .set((Instant::now() - Duration::from_secs(1), 1))
        .unwrap();
    assert!(fixture.wait_done().is_err());
    assert!(fixture.expired());
}

#[test]
fn heartbeat_ack_requires_actual_runner_post_and_one_recovered_active_slot() {
    let body = json!({"active_slots":1,"max_slots":1,"slot_recovered":true,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"agent_version":env!("CARGO_PKG_VERSION"),"public_base_url":"https://native-runner.example","capabilities":["execution_abi=oci"],"supported_lease_kinds":["runtime_launch"],"supported_session_surfaces":[],"host_resources":{}});
    assert!(validate_heartbeat("POST", "/v1/runners/owned/heartbeat", &body, "owned").is_ok());
    assert!(validate_heartbeat("GET", "/v1/runners/owned/heartbeat", &body, "owned").is_err());
    assert!(validate_heartbeat("POST", "/v1/runners/foreign/heartbeat", &body, "owned").is_err());
    for (field, value) in [
        ("active_slots", json!(0)),
        ("max_slots", json!(2)),
        ("slot_recovered", json!(false)),
        ("capabilities", json!({})),
        ("host_resources", json!(null)),
    ] {
        let mut invalid = body.clone();
        invalid[field] = value;
        assert!(
            validate_heartbeat("POST", "/v1/runners/owned/heartbeat", &invalid, "owned").is_err()
        );
    }
}
