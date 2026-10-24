//! Test-only browser coordination around the real owned Native lifecycle.
//! No tokens/keys enter files, no caller-provided expected values authorize work.
use super::*;
use std::{
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    sync::{
        Mutex, OnceLock,
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
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    action: String,
    expected_clipboard_latin1_hex: String,
}
fn validate_checkpoint(bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(bytes.len() <= 1024, "fixture checkpoint exceeds bound");
    let value: Checkpoint = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        value.version == 1
            && value.action == "verify-clipboard"
            && value.expected_clipboard_latin1_hex == CLIPBOARD_HEX,
        "fixture checkpoint must use fixed expected bytes"
    );
    Ok(())
}
#[derive(Default, Clone, serde::Serialize)]
struct Facts {
    actual_clipboard_latin1_hex: Option<String>,
    clipboard_checkpoint_verified: Option<bool>,
    paste_permission_denial_verified: Option<bool>,
    post_disconnect_clipboard_observed: Option<bool>,
    post_disconnect_clipboard_latin1_hex: Option<String>,
    actual_width: Option<u16>,
    actual_height: Option<u16>,
    resize_permission_denial_verified: Option<bool>,
    runtime_reconnect_verified: Option<bool>,
    runtime_full_frame_verified: Option<bool>,
    original_contract_satisfied: Option<bool>,
    frozen_probe_unchanged: Option<bool>,
    output_saved: Option<bool>,
}
impl Facts {
    fn checkpoint_passed(&self) -> bool {
        self.actual_clipboard_latin1_hex.as_deref() == Some(CLIPBOARD_HEX)
            && self.clipboard_checkpoint_verified == Some(true)
            && self.paste_permission_denial_verified == Some(true)
    }
    fn complete(&self) -> bool {
        self.checkpoint_passed()
            && self.actual_width == Some(WIDTH)
            && self.actual_height == Some(HEIGHT)
            && self
                .post_disconnect_clipboard_latin1_hex
                .as_deref()
                .is_none_or(|value| value == CLIPBOARD_HEX)
            && [
                self.post_disconnect_clipboard_observed,
                self.resize_permission_denial_verified,
                self.runtime_reconnect_verified,
                self.runtime_full_frame_verified,
                self.original_contract_satisfied,
                self.frozen_probe_unchanged,
                self.output_saved,
            ]
            .into_iter()
            .all(|fact| fact == Some(true))
    }
    fn denials(&self) -> Option<bool> {
        match (
            self.paste_permission_denial_verified,
            self.resize_permission_denial_verified,
        ) {
            (Some(true), Some(true)) => Some(true),
            (Some(false), _) | (_, Some(false)) => Some(false),
            _ => None,
        }
    }
}
pub(super) enum Fact {
    Contract,
    Frozen,
    Output,
    PasteDenial,
    ResizeDenial,
    Reconnect,
    FullFrame,
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
    facts: Mutex<Facts>,
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
            facts: Mutex::new(Facts::default()),
            identity: OnceLock::new(),
        }
    }
    pub fn observe(&self, fact: Fact, value: bool) {
        let mut facts = self.facts.lock().unwrap();
        *match fact {
            Fact::Contract => &mut facts.original_contract_satisfied,
            Fact::Frozen => &mut facts.frozen_probe_unchanged,
            Fact::Output => &mut facts.output_saved,
            Fact::PasteDenial => &mut facts.paste_permission_denial_verified,
            Fact::ResizeDenial => &mut facts.resize_permission_denial_verified,
            Fact::Reconnect => &mut facts.runtime_reconnect_verified,
            Fact::FullFrame => &mut facts.runtime_full_frame_verified,
        } = Some(value);
    }
    pub fn observe_clipboard(&self, value: &Option<Vec<u8>>, post_disconnect: bool) -> bool {
        let mut facts = self.facts.lock().unwrap();
        let actual = value
            .as_ref()
            .filter(|value| value.len() <= 256)
            .map(hex::encode);
        let matches = value.as_deref() == Some(b"caf\xe9\t\n".as_slice());
        if post_disconnect {
            facts.post_disconnect_clipboard_observed = Some(true);
            facts.post_disconnect_clipboard_latin1_hex = actual;
            value.is_none() || matches
        } else {
            facts.actual_clipboard_latin1_hex = actual;
            facts.clipboard_checkpoint_verified = Some(matches);
            matches
        }
    }
    pub fn observe_size(&self, width: u16, height: u16) {
        let mut facts = self.facts.lock().unwrap();
        facts.actual_width = Some(width);
        facts.actual_height = Some(height);
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
            "initial_width":640,"initial_height":480,"checkpoint_file":self.root.join("checkpoint.json"),"checkpoint_result_file":self.root.join("checkpoint-result.json"),"done_file":self.root.join("done.json"),"result_file":self.root.join("result.json")})).unwrap();
    }
    fn read_record(&self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.root.join(name))
        {
            Ok(mut file) => {
                let metadata = file.metadata()?;
                anyhow::ensure!(
                    metadata.is_file()
                        && metadata.len() <= 1024
                        && metadata.uid() == std::fs::metadata(&self.root)?.uid()
                        && metadata.permissions().mode() & 0o777 == 0o600,
                    "fixture request must be an owned private bounded regular file"
                );
                let mut bytes = Vec::new();
                Read::by_ref(&mut file).take(1025).read_to_end(&mut bytes)?;
                Ok(Some(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn checkpoint_result(&self, success: bool) -> anyhow::Result<()> {
        let identity = self.identity.get();
        let facts = self.facts.lock().unwrap().clone();
        self.atomic_json("checkpoint-result.json",&json!({"version":1,"status":if success {"passed"} else {"failed"},"session_id":identity.map(|(session,_,_)|session),"surface_id":identity.map(|(_,surface,_)|surface),"run_id":identity.map(|(_,_,scope)|&scope.run_id),"lease_id":identity.map(|(_,_,scope)|&scope.lease_id),"runner_id":identity.map(|(_,_,scope)|&scope.runner_id),"exp_unix_sec":self.clock.get().unwrap().1,"actual_clipboard_latin1_hex":facts.actual_clipboard_latin1_hex,"clipboard_checkpoint_verified":facts.clipboard_checkpoint_verified,"paste_permission_denial_verified":facts.paste_permission_denial_verified,"error":if success {serde_json::Value::Null} else {json!("fixture_checkpoint_failed")}}))
    }
    pub fn wait_done(
        &self,
        mut checkpoint: impl FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<u64> {
        let mut checkpoint_complete = false;
        loop {
            anyhow::ensure!(
                !self.expired(),
                "browser fixture absolute 90-second deadline expired"
            );
            if let Some(bytes) = self.read_record("done.json")? {
                validate_done(&bytes)?;
                anyhow::ensure!(
                    checkpoint_complete && self.facts.lock().unwrap().checkpoint_passed(),
                    "normal done requires prior actual clipboard checkpoint"
                );
                return Ok(self.clock.get().unwrap().1);
            }
            if !checkpoint_complete && let Some(bytes) = self.read_record("checkpoint.json")? {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    validate_checkpoint(&bytes)?;
                    checkpoint(self.clock.get().unwrap().1)?;
                    anyhow::ensure!(
                        self.facts.lock().unwrap().checkpoint_passed(),
                        "actual clipboard checkpoint is incomplete"
                    );
                    Ok::<(), anyhow::Error>(())
                }))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!("fixture checkpoint verification panicked"))
                });
                self.checkpoint_result(outcome.is_ok())?;
                outcome?;
                checkpoint_complete = true;
            }
            std::thread::sleep(Duration::from_millis(25));
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
        let facts = self.facts.lock().unwrap().clone();
        let success = success && cleanup && identity.is_some() && facts.complete();
        let mut result = serde_json::to_value(&facts).unwrap();
        result.as_object_mut().unwrap().extend(json!({"version":1,"status":if success {"passed"} else {"failed"},"heartbeats_acked":self.heartbeats_acked.load(AtomicOrdering::Acquire),"session_id":identity.map(|(session,_,_)|session),"surface_id":identity.map(|(_,surface,_)|surface),"run_id":identity.map(|(_,_,scope)|&scope.run_id),"lease_id":identity.map(|(_,_,scope)|&scope.lease_id),"runner_id":identity.map(|(_,_,scope)|&scope.runner_id),"permission_denials_verified":facts.denials(),"cleanup_confirmed":cleanup,"oci_resources_cleanup_confirmed":oci_cleanup,"error":if success {serde_json::Value::Null} else {json!("fixture_verification_failed")}}).as_object().unwrap().clone());
        self.atomic_json("result.json", &result).unwrap();
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
        facts: Mutex::new(Facts::default()),
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
    assert!(fixture.wait_done(|_| Ok(())).is_err());
    std::fs::remove_file(temp.path().join("done.json")).unwrap();
    fixture.atomic_json("done.json",&json!({"version":1,"action":"verify-and-stop","expected_clipboard_latin1_hex":CLIPBOARD_HEX,"expected_width":WIDTH,"expected_height":HEIGHT})).unwrap();
    assert!(validate_done(&fixture.read_record("done.json").unwrap().unwrap()).is_ok());
    assert!(
        fixture
            .wait_done(|_| panic!("done must not skip checkpoint"))
            .is_err()
    );
    std::fs::set_permissions(
        temp.path().join("done.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(fixture.wait_done(|_| Ok(())).is_err());
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
        facts: Mutex::new(Facts::default()),
        identity: OnceLock::new(),
    };
    fixture
        .clock
        .set((Instant::now() - Duration::from_secs(1), 1))
        .unwrap();
    assert!(fixture.wait_done(|_| Ok(())).is_err());
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

#[test]
fn checkpoint_is_strict_and_cannot_supply_or_skip_actual_observation() {
    let valid = json!({"version":1,"action":"verify-clipboard","expected_clipboard_latin1_hex":CLIPBOARD_HEX});
    assert!(validate_checkpoint(&serde_json::to_vec(&valid).unwrap()).is_ok());
    for field in [
        "version",
        "action",
        "expected_clipboard_latin1_hex",
        "unknown",
    ] {
        let mut invalid = valid.clone();
        invalid[field] = json!("untrusted");
        assert!(validate_checkpoint(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    assert!(validate_checkpoint(&[b' '; 1025]).is_err());
    assert!(validate_checkpoint(br#"{"version":1,"version":1}"#).is_err());
    let partial = Facts {
        original_contract_satisfied: Some(true),
        output_saved: Some(true),
        ..Facts::default()
    };
    let json = serde_json::to_value(&partial).unwrap();
    assert_eq!(json["original_contract_satisfied"], true);
    assert_eq!(json["output_saved"], true);
    assert!(json["clipboard_checkpoint_verified"].is_null());
    assert!(!partial.complete());
}
#[test]
fn overall_requires_all_observed_facts_even_when_post_disconnect_selection_is_none() {
    let mut facts = Facts {
        actual_clipboard_latin1_hex: Some(CLIPBOARD_HEX.into()),
        clipboard_checkpoint_verified: Some(true),
        paste_permission_denial_verified: Some(true),
        post_disconnect_clipboard_observed: Some(true),
        post_disconnect_clipboard_latin1_hex: None,
        actual_width: Some(WIDTH),
        actual_height: Some(HEIGHT),
        resize_permission_denial_verified: Some(true),
        runtime_reconnect_verified: Some(true),
        runtime_full_frame_verified: Some(true),
        original_contract_satisfied: Some(true),
        frozen_probe_unchanged: Some(true),
        output_saved: Some(true),
    };
    assert!(facts.complete());
    assert_eq!(facts.denials(), Some(true));
    facts.clipboard_checkpoint_verified = None;
    assert!(!facts.complete());
    facts.clipboard_checkpoint_verified = Some(true);
    facts.actual_clipboard_latin1_hex = None;
    assert!(!facts.complete());
    facts.actual_clipboard_latin1_hex = Some(CLIPBOARD_HEX.into());
    facts.paste_permission_denial_verified = Some(false);
    assert!(!facts.complete());
    assert_eq!(facts.denials(), Some(false));
    facts.paste_permission_denial_verified = Some(true);
    facts.output_saved = None;
    assert!(!facts.complete());
    facts.output_saved = Some(true);
    facts.actual_width = Some(800);
    assert!(!facts.complete());
    facts.actual_width = Some(WIDTH);
    facts.post_disconnect_clipboard_latin1_hex = Some("unexpected".into());
    assert!(!facts.complete());
}
