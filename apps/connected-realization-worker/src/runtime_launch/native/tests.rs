use super::*;
use ato_formation::{
    authoring::{BoundContract, BoundDerivation},
    verify::{RuntimeHttpObservation, VerificationExecutionEvidence, VerificationTargetKind},
};
use ato_runtime_attempt::{
    spec::{AttemptSpec, CandidateShape},
    verification::{ReceiptContext, verify_observed_candidate},
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const EXPECTED: &[u8] = b"go\r\n1\r\nEND";
pub(crate) const WORKLOAD: &str = "import sys,os\nn=0\nfor line in sys.stdin:\n if line.strip()=='exit': break\n if line.strip()=='size':\n  size=os.get_terminal_size(0)\n  print('size:'+str(size.columns)+'x'+str(size.lines)+'\\nEND',end='',flush=True)\n else:\n  n+=1\n  print(str(n)+'\\nEND',end='',flush=True)\n";

pub(crate) fn fixture(
    image: &str,
) -> (
    RuntimeLaunchSpecV1,
    SessionSurfaceDescriptor,
    BoundContract,
    BoundDerivation,
) {
    let mut spec = RuntimeLaunchSpecV1::parse(include_str!(
        "../../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/native-terminal-oci.json"
    ))
    .unwrap();
    spec.endpoints.clear();
    spec.state_attachments.clear();
    spec.public_env.clear();
    spec.lifecycle.graceful_shutdown_ms = 500;
    spec.lifecycle.force_kill_after_ms = 2000;
    spec.readiness = ato_ipc::runtime_launch::ReadinessV1::Process { timeout_ms: 5000 };
    let LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
        unreachable!()
    };
    oci.image_digest_ref = image.split_once('@').unwrap().1.to_owned();
    oci.image_reference = Some(image.to_owned());
    oci.platform = Some("linux/amd64".into());
    oci.user = Some(ato_ipc::runtime_launch::OciUserV1 {
        uid: 65532,
        gid: 65532,
    });
    oci.resource_limits = Some(ato_ipc::runtime_launch::OciResourceLimitsV1 {
        memory_bytes: 128 * 1024 * 1024,
        cpu_limit_millis: 500,
        pids_limit: 32,
    });
    oci.entrypoint = Some("/usr/local/bin/python3".into());
    oci.argv = Some(vec!["-u".into(), "/app/main.py".into()]);
    oci.working_dir = Some("/app".into());
    let mut payload = oci.native_surface.clone().unwrap();
    payload["adapter"]["operations"][0]["input"] = json!(b"go\r".to_vec());
    payload["adapter"]["operations"][0]["timeout_ms"] = json!(2000);
    let derivation: BoundDerivation = serde_json::from_value(json!({
        "schema":"ato.derivation/1","inputs":[],"runtimes":{"oci.image":image,"oci.platform":"linux/amd64","oci.user":"65532:65532","oci.memory_bytes":"134217728","oci.cpu_limit_millis":"500","oci.pids_limit":"32","oci.entrypoint":"/usr/local/bin/python3","oci.working_dir":"/app"},"state":[],"effects":"pure",
        "steps":[{"id":"main","protocol":"ato.oci@1","op":"serve","argv":["-u","/app/main.py"],"cwd":"."}],
        "ports":[{"id":"app.native","protocol":"ato.pty@1","from":"main"},{"id":"app.probe","protocol":"ato.http@1","from":"main"}],
        "native_surface":payload["adapter"]
    })).unwrap();
    payload["derivation_ref"] = json!(derivation.derivation_ref().unwrap());
    payload["ports"] = serde_json::to_value(&derivation.ports).unwrap();
    oci.native_surface = Some(payload);
    let descriptor = serde_json::from_value(json!({"kind":"terminal","profile":"ato.terminal-surface.v1",
        "surface_id":"native-terminal","transport":"terminal_websocket","capabilities":{"cols":80,"rows":24}})).unwrap();
    let contract = serde_json::from_value(json!({"schema":"ato.contract/1","requirements":[{
        "id":"actual-result","verifier":ato_formation::authoring::HTTP_CONTRACT_VERIFIER,"port":"app.probe","method":"GET","path":"/result","status":200,
        "body_digest":RuntimeHttpObservation::from_response("","","",0,EXPECTED).body_digest
    }]})).unwrap();
    (spec, descriptor, contract, derivation)
}
pub(crate) fn owner(spec: &RuntimeLaunchSpecV1) -> OciOwner {
    OciOwner {
        runner_id: "native-test-runner".into(),
        slot_id: "isolated-native".into(),
        lease_id: format!(
            "lease-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ),
        run_id: spec.context.run_id.clone(),
        incarnation: format!("test-{}", std::process::id()),
    }
}
pub(crate) fn config() -> NativeLeaseSurfaceConfig {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    NativeLeaseSurfaceConfig {
        controls: netd::pixel_gateway::PixelGatewayControls::default(),
        scope: SurfaceGatewayScope {
            session_id: "native-test-session".into(),
            surface_id: "native-terminal".into(),
        },
        verification_key: "native-test-key-native-test-key-native-test-key".into(),
        origin: "https://staging.ato.run".into(),
        public_base_origin: None,
        surface_listen: address,
        startup_timeout: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(60),
        maximum_lifetime: Duration::from_secs(30),
    }
}
pub(crate) fn context(root: &std::path::Path, workload: &str) -> ResolvedRuntimeLaunchContext {
    use std::os::unix::fs::PermissionsExt;
    let workspace = root.join("w");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(workspace.join("main.py"), workload).unwrap();
    ResolvedRuntimeLaunchContext::new(workspace, "", BTreeMap::new(), vec![], vec![], vec![])
        .unwrap()
}
async fn get(addr: SocketAddr) -> Vec<u8> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /result HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let (head, body) = bytes.split_at(bytes.windows(4).position(|v| v == b"\r\n\r\n").unwrap() + 4);
    assert!(head.starts_with(b"HTTP/1.1 200"));
    body.to_vec()
}
fn image() -> String {
    std::env::var("ATO_NATIVE_TERMINAL_TEST_IMAGE")
        .expect("explicit already-present digest-pinned CPU test image")
}
pub(crate) fn scratch() -> tempfile::TempDir {
    let root = std::env::var_os("ATO_NATIVE_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp"));
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("nt-")
        .tempdir_in(root)
        .unwrap()
}

async fn closed(addr: SocketAddr) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("native endpoint survived cleanup: {addr}");
}

fn assertion(owner: &OciOwner, scope: &SurfaceGatewayScope) -> String {
    let keys = SurfaceAssertionKeyring::new(BTreeMap::from([(
        "ato.runner-surface@1".into(),
        "native-test-key-native-test-key-native-test-key".into(),
    )]))
    .unwrap();
    let execution = SurfaceExecutionScope {
        run_id: owner.run_id.clone(),
        lease_id: owner.lease_id.clone(),
        runner_id: owner.runner_id.clone(),
    };
    keys.issue_readiness_assertion_for_execution(scope, Some(execution))
        .unwrap()
        .as_str()
        .to_owned()
}
pub(crate) fn viewer(
    addr: SocketAddr,
    assertion: String,
    origin: &str,
    input: &[u8],
    resize: bool,
) -> Result<(u16, u16, Vec<u8>)> {
    use tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
    };
    let mut request = format!("ws://{addr}/__ato/surface").into_client_request()?;
    for (name, value) in [
        ("origin", origin),
        ("sec-websocket-protocol", "ato.terminal.v1"),
        (
            netd::pixel_gateway::SURFACE_ASSERTION_HEADER,
            assertion.as_str(),
        ),
    ] {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    let (mut ws, _) = tungstenite::connect(request)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(socket) = ws.get_mut() {
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    }
    let ready = ws.read()?;
    let value: serde_json::Value = serde_json::from_str(ready.to_text()?)?;
    let cols = value["cols"].as_u64().context("missing ready columns")? as u16;
    let rows = value["rows"].as_u64().context("missing ready rows")? as u16;
    if resize {
        ws.send(Message::text(r#"{"type":"resize","cols":120,"rows":40}"#))?;
    }
    ws.send(Message::binary(input.to_vec()))?;
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"END") {
        match ws.read()? {
            Message::Binary(chunk) => {
                bytes.extend_from_slice(&chunk);
                ensure!(bytes.len() <= 4096, "viewer output bound");
                ws.send(Message::text(format!(
                    r#"{{"type":"ack","bytes":{}}}"#,
                    chunk.len()
                )))?;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = ws.close(None);
    Ok((cols, rows, bytes))
}
fn no_owned_resources(owner: &OciOwner) {
    let resources = ato_adapter_oci::OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)
        .unwrap()
        .scan()
        .unwrap();
    assert!(
        !resources
            .containers
            .iter()
            .any(|resource| resource.lease_id() == Some(&owner.lease_id))
    );
    assert!(
        !resources
            .networks
            .iter()
            .any(|resource| resource.lease_id() == Some(&owner.lease_id))
    );
}

#[tokio::test]
async fn invalid_native_scope_or_port_facts_are_refused_before_oci_or_any_action() {
    for invalid in [
        "key",
        "origin",
        "surface_scope",
        "missing_ports",
        "guest_ports",
        "selection",
        "owner",
        "gui_controls",
    ] {
        let (mut spec, descriptor, _, derivation) = fixture(&format!(
            "example.invalid/missing@sha256:{}",
            "22".repeat(32)
        ));
        let root = scratch();
        let context = context(root.path(), WORKLOAD);
        let mut config = config();
        let mut selected = derivation.derivation_ref().unwrap();
        let mut owner = owner(&spec);
        match invalid {
            "gui_controls" => config.controls.text_paste = true,
            "key" => config.verification_key = "weak".into(),
            "origin" => config.origin = "not-an-origin".into(),
            "surface_scope" => config.scope.surface_id = "wrong-surface".into(),
            "selection" => selected = format!("sha256:{}", "33".repeat(32)),
            "owner" => owner.run_id = "other-run".into(),
            "missing_ports" | "guest_ports" => {
                let LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
                    unreachable!()
                };
                let native = oci.native_surface.as_mut().unwrap();
                if invalid == "missing_ports" {
                    native.as_object_mut().unwrap().remove("ports");
                } else {
                    native["ports"][0]["guest_port"] = json!(1026);
                    native["ports"][1]["guest_port"] = json!(8081);
                }
            }
            _ => unreachable!(),
        }
        let (_owner_tx, owner_rx) = watch::channel(true);
        let error = start_terminal(
            &spec,
            &selected,
            &descriptor,
            &context,
            &owner,
            config,
            owner_rx,
        )
        .await
        .err()
        .unwrap();
        assert!(error.stop.is_confirmed(), "{invalid}: {error}");
        assert!(!root.path().join("oci-runtime").exists(), "{invalid}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires native Linux Docker and explicit already-present pinned Python CPU image"]
async fn real_owned_terminal_freezes_actual_k_bytes_and_cleans_every_endpoint() {
    let (spec, descriptor, contract, derivation) = fixture(&image());
    let root = scratch();
    let context = context(root.path(), WORKLOAD);
    let owner = owner(&spec);
    let (_owner_tx, owner_rx) = watch::channel(true);
    let started = Instant::now();
    let config = config();
    let scope = config.scope.clone();
    let run = start_terminal(
        &spec,
        &derivation.derivation_ref().unwrap(),
        &descriptor,
        &context,
        &owner,
        config,
        owner_rx,
    )
    .await
    .unwrap();
    let ready_ms = started.elapsed().as_millis();
    let probe = run.observation_addr();
    let surface = run.surface_addr();
    let actual = get(probe).await;
    assert_eq!(actual, EXPECTED);
    assert_eq!(get(probe).await, EXPECTED); // GET must not replay input.
    let contract_ref = contract.contract_ref().unwrap();
    let d_ref = derivation.derivation_ref().unwrap();
    let attempt = AttemptSpec {
        contract: &contract,
        contract_ref: &contract_ref,
        derivation: &derivation,
        derivation_ref: &d_ref,
        shape: CandidateShape::Process,
        input_refs: BTreeMap::new(),
        instance_snapshot_ref: None,
        scenarios: Vec::new(),
    };
    let evidence: VerificationExecutionEvidence = serde_json::from_value(
        json!({"realization":"oci","container_id":run.container_id(),"lease_id":owner.lease_id}),
    )
    .unwrap();
    let receipt_context = ReceiptContext {
        target: VerificationTargetKind::AtoRunHosted,
        bundle_sha256: None,
        portability_profile: None,
        run_id: Some(&spec.context.run_id),
    };
    let verify = |port, body: &[u8]| {
        verify_observed_candidate(
            &attempt,
            vec![RuntimeHttpObservation::from_response(
                port, "GET", "/result", 200, body,
            )],
            Vec::new(),
            evidence.clone(),
            &receipt_context,
        )
    };
    assert!(verify("app.probe", &actual).receipt.fully_satisfied);
    assert!(!verify("app.native", &actual).receipt.fully_satisfied);
    assert!(
        !verify("app.probe", b"surface ready")
            .receipt
            .fully_satisfied
    );
    assert_eq!(contract.contract_ref().unwrap(), contract_ref);
    let addr = surface;
    assert!(
        tokio::task::spawn_blocking(move || viewer(
            addr,
            "invalid".into(),
            "https://staging.ato.run",
            b"go\r",
            false
        ))
        .await
        .unwrap()
        .is_err()
    );
    let bad = assertion(&owner, &scope);
    assert!(
        tokio::task::spawn_blocking(move || viewer(
            addr,
            bad,
            "https://example.invalid",
            b"go\r",
            false
        ))
        .await
        .unwrap()
        .is_err()
    );
    let mut wrong = owner.clone();
    wrong.run_id = "other-run".into();
    let bad = assertion(&wrong, &scope);
    assert!(
        tokio::task::spawn_blocking(move || viewer(
            addr,
            bad,
            "https://staging.ato.run",
            b"go\r",
            false
        ))
        .await
        .unwrap()
        .is_err()
    );
    let good = assertion(&owner, &scope);
    let result = tokio::task::spawn_blocking(move || {
        viewer(addr, good, "https://staging.ato.run", b"go\r", true)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, (80, 24, b"go\r\n2\r\nEND".to_vec()));
    let good = assertion(&owner, &scope);
    let result = tokio::task::spawn_blocking(move || {
        viewer(addr, good, "https://staging.ato.run", b"size\r", false)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, (120, 40, b"size\r\nsize:120x40\r\nEND".to_vec()));
    assert_eq!(get(probe).await, EXPECTED);
    assert_eq!(
        run.execution_evidence().principal.as_ref().unwrap().uids,
        [65532; 4]
    );
    eprintln!("owned native terminal ready_ms={}", ready_ms);
    let completion = run.stop().await;
    assert!(completion.stop.is_confirmed());
    assert_eq!(completion.cause, "requested_stop");
    closed(surface).await;
    closed(probe).await;
    assert!(!root.path().join("oci-runtime/terminal.sock").exists());
    assert!(super::super::recovery::settle_lease(&owner, StopBudget::DEFAULT).is_confirmed());
    no_owned_resources(&owner);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires native Linux Docker and explicit already-present pinned Python CPU image"]
async fn owner_eof_during_a_stalled_native_action_cleans_workload_and_socket() {
    let (mut spec, descriptor, _, mut derivation) = fixture(&image());
    use std::os::unix::fs::MetadataExt;
    let root = scratch();
    let original_context = context(
        root.path(),
        "import sys,time\nvalue=sys.stdin.readline()\nopen('/data/accepted','w').write(value)\ntime.sleep(120)\n",
    );
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let uid = state.metadata().unwrap().uid();
    let gid = state.metadata().unwrap().gid();
    assert!(
        uid > 0 && gid > 0,
        "real native test requires nonroot workspace ownership"
    );
    let mut d = serde_json::to_value(&derivation).unwrap();
    d["native_surface"]["operations"][0]["timeout_ms"] = json!(30_000);
    d["runtimes"]["oci.user"] = json!(format!("{uid}:{gid}"));
    d["state"] = json!([{"id":"accepted","protocol":"ato.state.filesystem@1","mount":"/data","access":"read-write"}]);
    derivation = serde_json::from_value(d).unwrap();
    if let LaunchRealizationV1::Oci(oci) = &mut spec.realization {
        oci.user = Some(ato_ipc::runtime_launch::OciUserV1 { uid, gid });
        oci.native_surface = Some(
            serde_json::to_value(
                NativeLaunchDeclaration::new(
                    derivation.derivation_ref().unwrap(),
                    derivation.native_surface.clone().unwrap(),
                )
                .unwrap()
                .with_ports(derivation.ports.clone())
                .unwrap(),
            )
            .unwrap(),
        );
    }
    spec.state_attachments = vec![ato_ipc::runtime_launch::StateAttachmentV1 {
        state_key: "accepted".into(),
        revision_ref: None,
        mount_target: "/data".into(),
        access: ato_ipc::runtime_launch::StateAccessV1::ReadWrite,
        writer_fence: None,
        backing: None,
    }];
    let context = ResolvedRuntimeLaunchContext::new(
        original_context.workspace_root().to_path_buf(),
        "",
        BTreeMap::new(),
        vec![],
        vec![
            ato_runtime_attempt::launch::resolved::ResolvedStateAttachment::new(
                "accepted",
                None,
                state.clone(),
                "/data",
                ato_ipc::runtime_launch::StateAccessV1::ReadWrite,
            ),
        ],
        vec![],
    )
    .unwrap();
    let owner = owner(&spec);
    let (owner_tx, owner_rx) = watch::channel(true);
    let selected_ref = derivation.derivation_ref().unwrap();
    let start = start_terminal(
        &spec,
        &selected_ref,
        &descriptor,
        &context,
        &owner,
        config(),
        owner_rx,
    );
    let cancel = async {
        let deadline = Instant::now() + Duration::from_secs(4);
        while !state.join("accepted").exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(std::fs::read(state.join("accepted")).unwrap(), b"go\n");
        drop(owner_tx);
    };
    let (result, _) = tokio::join!(start, cancel);
    let error = result.err().expect("owner EOF refuses readiness");
    assert!(
        error.error.to_string().contains("owner disconnected"),
        "{error}"
    );
    assert!(error.stop.is_confirmed(), "{error}");
    assert!(!root.path().join("oci-runtime/terminal.sock").exists());
    assert!(super::super::recovery::settle_lease(&owner, StopBudget::DEFAULT).is_confirmed());
    no_owned_resources(&owner);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires native Linux Docker and explicit already-present pinned Python CPU image"]
async fn active_native_owner_idle_lifetime_and_workload_exit_all_reclaim_resources() {
    for expected_cause in [
        "owner_eof",
        "idle_timeout",
        "maximum_lifetime",
        "workload_exit",
    ] {
        let (spec, descriptor, _, derivation) = fixture(&image());
        let root = scratch();
        let workload = if expected_cause == "workload_exit" {
            "import sys,time\nsys.stdin.readline()\nprint('1\\nEND',end='',flush=True)\ntime.sleep(2)\n"
        } else {
            WORKLOAD
        };
        let context = context(root.path(), workload);
        let owner = owner(&spec);
        let (owner_tx, owner_rx) = watch::channel(true);
        let mut config = config();
        if expected_cause == "idle_timeout" {
            config.idle_timeout = Duration::from_secs(1);
        }
        if expected_cause == "maximum_lifetime" {
            config.maximum_lifetime = Duration::from_secs(2);
        }
        let run = start_terminal(
            &spec,
            &derivation.derivation_ref().unwrap(),
            &descriptor,
            &context,
            &owner,
            config,
            owner_rx,
        )
        .await
        .unwrap();
        let surface = run.surface_addr();
        let observation = run.observation_addr();
        assert_eq!(get(observation).await, EXPECTED);
        let started = Instant::now();
        if expected_cause == "owner_eof" {
            drop(owner_tx);
        }
        let completion = tokio::time::timeout(Duration::from_secs(6), run.wait())
            .await
            .unwrap();
        assert!(
            completion.stop.is_confirmed(),
            "{expected_cause}: {:?}",
            completion.stop
        );
        assert_eq!(completion.cause, expected_cause);
        if expected_cause == "owner_eof" {
            assert!(started.elapsed() < Duration::from_secs(3));
        }
        closed(surface).await;
        closed(observation).await;
        assert!(!root.path().join("oci-runtime/terminal.sock").exists());
        assert!(super::super::recovery::settle_lease(&owner, StopBudget::DEFAULT).is_confirmed());
        no_owned_resources(&owner);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires native Linux Docker and explicit already-present pinned Python CPU image"]
async fn startup_timeout_during_stalled_capture_never_exposes_surface() {
    let (mut spec, descriptor, _, derivation) = fixture(&image());
    if let LaunchRealizationV1::Oci(oci) = &mut spec.realization {
        oci.native_surface.as_mut().unwrap()["adapter"]["operations"][0]["timeout_ms"] =
            json!(30_000);
    }
    // Recompute the selected D rather than modifying a digest-bound catalog.
    let mut d = serde_json::to_value(derivation).unwrap();
    d["native_surface"]["operations"][0]["timeout_ms"] = json!(30_000);
    let derivation: BoundDerivation = serde_json::from_value(d).unwrap();
    if let LaunchRealizationV1::Oci(oci) = &mut spec.realization {
        oci.native_surface.as_mut().unwrap()["derivation_ref"] =
            json!(derivation.derivation_ref().unwrap());
    }
    let root = scratch();
    let context = context(
        root.path(),
        "import sys,time\nsys.stdin.readline()\ntime.sleep(120)\n",
    );
    let owner = owner(&spec);
    let (_owner_tx, owner_rx) = watch::channel(true);
    let mut config = config();
    config.startup_timeout = Duration::from_secs(1);
    let surface = config.surface_listen;
    let started = Instant::now();
    let error = start_terminal(
        &spec,
        &derivation.derivation_ref().unwrap(),
        &descriptor,
        &context,
        &owner,
        config,
        owner_rx,
    )
    .await
    .err()
    .unwrap();
    assert!(
        error.error.to_string().contains("startup timeout"),
        "{error}"
    );
    assert!(error.stop.is_confirmed(), "{error}");
    assert!(started.elapsed() < Duration::from_secs(4));
    closed(surface).await;
    assert!(!root.path().join("oci-runtime/terminal.sock").exists());
    no_owned_resources(&owner);
}

#[test]
fn surface_allowlist_is_the_lease_origin_plus_only_the_runners_own_https_ingress() {
    // Exactly the authored Lease origin when the Runner publishes no ingress.
    let lease_only = config();
    assert_eq!(
        super::surface_allowed_origins(
            lease_only.origin.clone(),
            lease_only.public_base_origin.as_deref()
        ),
        std::collections::BTreeSet::from(["https://staging.ato.run".to_string()])
    );

    // The Runner's own published ingress is additive, never a replacement, and
    // is normalized exactly as a browser serializes `Origin`.
    for (configured, expected) in [
        ("https://runner.example", "https://runner.example"),
        ("https://runner.example/", "https://runner.example"),
        ("https://RUNNER.example", "https://runner.example"),
        ("https://runner.example:443", "https://runner.example"),
        ("https://runner.example:8443", "https://runner.example:8443"),
        (
            "https://runner.example/ingress/slot-1",
            "https://runner.example",
        ),
    ] {
        let mut candidate = config();
        candidate.public_base_origin = Some(configured.into());
        assert_eq!(
            super::surface_allowed_origins(
                candidate.origin.clone(),
                candidate.public_base_origin.as_deref()
            ),
            std::collections::BTreeSet::from([
                "https://staging.ato.run".to_string(),
                expected.to_string(),
            ]),
            "configured {configured}"
        );
    }

    // Non-https or unparseable ingress values are ignored: a downgraded or
    // malformed public address must never enter the exact-match allowlist.
    for rejected in [
        "http://runner.example",
        "ws://runner.example",
        "wss://runner.example",
        "runner.example",
        "not-a-url",
        "",
        "javascript:alert(1)",
    ] {
        let mut candidate = config();
        candidate.public_base_origin = Some(rejected.into());
        assert_eq!(
            super::surface_allowed_origins(
                candidate.origin.clone(),
                candidate.public_base_origin.as_deref()
            ),
            std::collections::BTreeSet::from(["https://staging.ato.run".to_string()]),
            "unexpectedly allowed {rejected}"
        );
    }
}
