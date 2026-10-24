//! Explicit browser coordination only, around the existing owned Terminal Run.
//! Never connects a new PTY, sends input, resizes, or recomputes K.
use super::*;
use std::{
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
pub(super) const KEY: &str = "native-browser-joint-fixture-key-not-an-operator-secret-0001";
const SECONDS: u64 = 90;
pub(super) const WORKLOAD: &str = r#"import sys,os,json,pathlib
n=0
def snapshot():
 size=os.get_terminal_size(0)
 data={'counter':n,'cols':size.columns,'rows':size.lines,'pid':os.getpid()}
 root=pathlib.Path(os.environ['ATO_OUTPUT_DIR'])
 pending=root/'.status.pending'
 pending.write_text(json.dumps(data,sort_keys=True,separators=(',',':')))
 pending.replace(root/'status.json')
snapshot()
for line in sys.stdin:
 if line.strip()=='exit': break
 if line.strip()=='size':
  size=os.get_terminal_size(0)
  snapshot()
  print('size:'+str(size.columns)+'x'+str(size.lines)+'\nEND',end='',flush=True)
 else:
  n+=1
  snapshot()
  print(str(n)+'\nEND',end='',flush=True)
"#;
#[derive(serde::Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct Status {
    counter: u32,
    cols: u16,
    rows: u16,
    pid: u32,
}
#[derive(serde::Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
enum Request {
    #[serde(rename = "abort")]
    Abort { version: u8 },
    #[serde(rename = "verify-and-stop")]
    Verify {
        version: u8,
        expected_counter: u32,
        expected_cols: u16,
        expected_rows: u16,
    },
}
fn request(bytes: &[u8]) -> anyhow::Result<bool> {
    anyhow::ensure!(bytes.len() <= 1024, "Terminal done exceeds bound");
    match serde_json::from_slice::<Request>(bytes)? {
        Request::Abort { version } => {
            anyhow::ensure!(version == 1, "unknown abort version");
            Ok(false)
        }
        Request::Verify {
            version,
            expected_counter,
            expected_cols,
            expected_rows,
        } => {
            anyhow::ensure!(
                version == 1 && (expected_counter, expected_cols, expected_rows) == (3, 120, 40),
                "Terminal done must match fixed expectations"
            );
            Ok(true)
        }
    }
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
        "explicit HTTPS loopback origin required"
    );
    Ok(url.origin().ascii_serialization())
}
#[derive(Default)]
struct Facts {
    counter: Option<u32>,
    cols: Option<u16>,
    rows: Option<u16>,
    reconnect: Option<bool>,
    contract: Option<bool>,
    frozen: Option<bool>,
    output: Option<bool>,
}
struct Context {
    public: SocketAddr,
    hidden: SocketAddr,
    work_root: PathBuf,
    lease_root: PathBuf,
    runner_id: String,
}
pub(super) struct TerminalBrowserFixture {
    root: PathBuf,
    pub origin: String,
    clock: OnceLock<(Instant, u64)>,
    context: OnceLock<Context>,
    identity: OnceLock<(
        String,
        String,
        ato_ipc::session_surface::SurfaceExecutionScope,
    )>,
    initial_pid: OnceLock<u32>,
    facts: Mutex<Facts>,
}
impl TerminalBrowserFixture {
    pub fn create() -> Self {
        let root = PathBuf::from(
            std::env::var_os("ATO_NATIVE_TERMINAL_BROWSER_FIXTURE_DIR")
                .expect("explicit own new fixture directory"),
        );
        assert!(root.is_absolute());
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let origin = origin(
            &std::env::var("ATO_NATIVE_TERMINAL_BROWSER_ORIGIN")
                .unwrap_or_else(|_| "https://127.0.0.1:4181".into()),
        )
        .unwrap();
        Self {
            root,
            origin,
            clock: OnceLock::new(),
            context: OnceLock::new(),
            identity: OnceLock::new(),
            initial_pid: OnceLock::new(),
            facts: Mutex::new(Facts::default()),
        }
    }
    pub fn configure(&self, command: &mut RuntimeLaunchLeaseCommand) {
        command.surface_origin = Some(self.origin.clone());
        command.max_duration_secs = Some(SECONDS);
    }
    pub fn register(
        &self,
        public: SocketAddr,
        hidden: SocketAddr,
        work_root: PathBuf,
        lease_root: PathBuf,
        runner_id: String,
    ) {
        assert!(
            self.context
                .set(Context {
                    public,
                    hidden,
                    work_root,
                    lease_root,
                    runner_id
                })
                .is_ok()
        );
    }
    pub fn arm(&self) {
        self.clock
            .set((
                Instant::now() + Duration::from_secs(SECONDS),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + SECONDS,
            ))
            .unwrap();
    }
    pub fn expired(&self) -> bool {
        self.clock
            .get()
            .is_some_and(|(until, _)| Instant::now() >= *until)
    }
    fn atomic_json(&self, name: &str, value: &serde_json::Value) -> anyhow::Result<()> {
        let pending = self.root.join(format!(".{name}.pending"));
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&pending)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        fs::hard_link(&pending, self.root.join(name))?;
        fs::remove_file(pending)?;
        Ok(())
    }
    pub fn status_bytes(&self) -> Vec<u8> {
        let path = self
            .context
            .get()
            .unwrap()
            .lease_root
            .join("outputs/status.json");
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .unwrap();
        let meta = file.metadata().unwrap();
        assert!(meta.is_file() && meta.len() <= 1024);
        let mut bytes = vec![];
        Read::by_ref(&mut file)
            .take(1025)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= 1024);
        let _: Status = serde_json::from_slice(&bytes).unwrap();
        bytes
    }
    pub fn initial(&self) {
        let state: Status = serde_json::from_slice(&self.status_bytes()).unwrap();
        assert_eq!((state.counter, state.cols, state.rows), (1, 80, 24));
        assert!(state.pid > 0);
        self.initial_pid.set(state.pid).unwrap();
    }
    pub fn contract(&self, value: bool) {
        self.facts.lock().unwrap().contract = Some(value);
    }
    pub fn frozen(&self, value: bool) {
        self.facts.lock().unwrap().frozen = Some(value);
    }
    pub fn output(&self) {
        self.facts.lock().unwrap().output = Some(true);
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
            .unwrap();
        self.atomic_json("ready.json",&json!({"version":1,"origin":self.origin,"gateway_ws_url":format!("ws://{public}/__ato/surface"),"session_id":scope.session_id,"surface_id":scope.surface_id,"execution":execution,"principal":{"kind":"user","id":"fixture-owner"},"initial_cols":80,"initial_rows":24,"exp_unix_sec":self.clock.get().unwrap().1,"done_file":self.root.join("done.json"),"result_file":self.root.join("result.json")})).unwrap();
    }
    pub fn wait_done(&self) -> anyhow::Result<()> {
        loop {
            anyhow::ensure!(!self.expired(), "absolute90s Terminal fixture expired");
            match fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(self.root.join("done.json"))
            {
                Ok(mut file) => {
                    let meta = file.metadata()?;
                    anyhow::ensure!(
                        meta.is_file()
                            && meta.len() <= 1024
                            && meta.uid() == fs::metadata(&self.root)?.uid()
                            && meta.permissions().mode() & 0o777 == 0o600,
                        "done must be owned regular private bounded file"
                    );
                    let mut bytes = vec![];
                    Read::by_ref(&mut file).take(1025).read_to_end(&mut bytes)?;
                    anyhow::ensure!(request(&bytes)?, "browser aborted Terminal fixture");
                    return Ok(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn verify_passive(&self) {
        let state: Status = serde_json::from_slice(&self.status_bytes()).unwrap();
        let same = Some(&state.pid) == self.initial_pid.get()
            && (state.counter, state.cols, state.rows) == (3, 120, 40);
        {
            let mut facts = self.facts.lock().unwrap();
            facts.counter = Some(state.counter);
            facts.cols = Some(state.cols);
            facts.rows = Some(state.rows);
            facts.reconnect = Some(same);
        }
        eprintln!(
            "TERMINAL_BROWSER_CHILD_EVIDENCE {}",
            json!({"initial_pid":self.initial_pid.get(),"final_pid":state.pid,"counter":state.counter,"cols":state.cols,"rows":state.rows})
        );
        assert!(
            same,
            "Browser must change the same owned PTY; verifier never sends input or resize"
        );
    }
    pub fn finish(&self, ok: bool) {
        let context = self.context.get();
        let oci = context.is_some_and(|c| {
            ato_adapter_oci::OwnedResourceScanner::new(&c.runner_id, "0")
                .unwrap()
                .scan()
                .is_ok_and(|s| s.containers.is_empty() && s.networks.is_empty())
        });
        let cleanup = oci
            && context.is_some_and(|c| {
                [c.public, c.hidden].into_iter().all(|a| {
                    std::net::TcpStream::connect_timeout(&a, Duration::from_millis(100)).is_err()
                }) && !c.lease_root.exists()
                    && (!c.work_root.exists()
                        || runtime_launch::recovery::RunJournal::new(
                            &c.work_root,
                            &c.runner_id,
                            "0",
                        )
                        .and_then(|j| j.load())
                        .is_ok_and(|j| j.is_empty()))
            });
        let id = self.identity.get();
        let f = self.facts.lock().unwrap();
        let passed = ok
            && cleanup
            && id.is_some()
            && f.counter == Some(3)
            && f.cols == Some(120)
            && f.rows == Some(40)
            && [f.reconnect, f.contract, f.frozen, f.output]
                .into_iter()
                .all(|f| f == Some(true));
        self.atomic_json("result.json",&json!({"version":1,"status":if passed{"passed"}else{"failed"},"session_id":id.map(|(s,_,_)|s),"surface_id":id.map(|(_,s,_)|s),"run_id":id.map(|(_,_,e)|&e.run_id),"lease_id":id.map(|(_,_,e)|&e.lease_id),"runner_id":id.map(|(_,_,e)|&e.runner_id),"exp_unix_sec":self.clock.get().map(|(_,e)|e),"actual_counter":f.counter,"actual_cols":f.cols,"actual_rows":f.rows,"runtime_reconnect_verified":f.reconnect,"original_contract_satisfied":f.contract,"frozen_probe_unchanged":f.frozen,"output_saved":f.output,"cleanup_confirmed":cleanup,"oci_resources_cleanup_confirmed":oci,"error":if passed{serde_json::Value::Null}else{json!("terminal_fixture_verification_failed")}})).unwrap();
        assert!(passed, "Terminal fixture failed; see result and log");
    }
}
#[test]
fn done_expectations_are_fixed_and_abort_never_passes() {
    assert!(request(br#"{"version":1,"action":"verify-and-stop","expected_counter":3,"expected_cols":120,"expected_rows":40}"#).unwrap());
    assert!(!request(br#"{"version":1,"action":"abort"}"#).unwrap());
    for invalid in [br#"{"version":1,"action":"verify-and-stop","expected_counter":4,"expected_cols":120,"expected_rows":40}"#.as_slice(),br#"{"version":1,"action":"abort","extra":true}"#,br#"{"version":1,"action":"abort","version":1}"#]{assert!(request(invalid).is_err());}
    assert!(request(&[b' '; 1025]).is_err());
}
#[test]
fn terminal_origin_is_explicit_https_loopback() {
    assert!(origin("https://127.0.0.1:4181").is_ok());
    for value in [
        "http://127.0.0.1:4181",
        "https://localhost:4181",
        "https://127.0.0.1:4181/path",
    ] {
        assert!(origin(value).is_err());
    }
}

#[test]
fn handoff_is_private_create_new_and_rejects_links_or_public_done() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = TerminalBrowserFixture {
        root: root.path().to_path_buf(),
        origin: "https://127.0.0.1:4181".into(),
        clock: OnceLock::new(),
        context: OnceLock::new(),
        identity: OnceLock::new(),
        initial_pid: OnceLock::new(),
        facts: Mutex::new(Facts::default()),
    };
    fixture.arm();
    fixture
        .atomic_json("ready.json", &json!({"version":1}))
        .unwrap();
    assert_eq!(
        fs::metadata(root.path().join("ready.json"))
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
        root.path().join("ready.json"),
        root.path().join("done.json"),
    )
    .unwrap();
    assert!(fixture.wait_done().is_err());
    fs::remove_file(root.path().join("done.json")).unwrap();
    fs::write(
        root.path().join("done.json"),
        br#"{"version":1,"action":"abort"}"#,
    )
    .unwrap();
    fs::set_permissions(
        root.path().join("done.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(fixture.wait_done().is_err());
    fs::set_permissions(
        root.path().join("done.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(
        fixture
            .wait_done()
            .unwrap_err()
            .to_string()
            .contains("aborted")
    );
}

/// Authenticated readiness only, before browser interaction: no input or resize.
pub(super) fn ready_probe(address: SocketAddr, token: &str, origin: &str) -> anyhow::Result<()> {
    use tungstenite::{Message, client::IntoClientRequest};
    let stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut request = format!("ws://{address}/__ato/surface").into_client_request()?;
    request.headers_mut().insert("origin", origin.parse()?);
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "ato.terminal.v1".parse()?);
    request.headers_mut().insert(
        netd::pixel_gateway::SURFACE_ASSERTION_HEADER,
        token.parse()?,
    );
    let (mut ws, response) = tungstenite::client(request, stream)?;
    anyhow::ensure!(
        response
            .headers()
            .get("sec-websocket-protocol")
            .is_some_and(|value| value == "ato.terminal.v1"),
        "exact Terminal subprotocol required"
    );
    let Message::Text(text) = ws.read()? else {
        anyhow::bail!("Terminal readiness must be typed control")
    };
    let ready: ato_ipc::terminal_surface::TerminalServerControl = serde_json::from_str(&text)?;
    anyhow::ensure!(
        matches!(
            ready,
            ato_ipc::terminal_surface::TerminalServerControl::Ready { cols: 80, rows: 24 }
        ),
        "Terminal declared initial size differs"
    );
    let _ = ws.close(None);
    Ok(())
}
