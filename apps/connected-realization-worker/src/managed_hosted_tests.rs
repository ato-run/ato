//! CPU acceptance of the actual claimed-lease evaluator, with a local control
//! transport and the production Broker router behind its test CA. No live keys.
use super::*;
use ato_formation::authoring::{BoundContract, BoundDerivation};
use ato_formation::verify::{
    RuntimeHttpObservation, VerificationExecutionEvidence, VerificationTargetKind,
};
use ato_runtime_attempt::spec::{AttemptSpec, CandidateShape};
use ato_runtime_attempt::verification::{ReceiptContext, verify_observed_candidate};
use serde_json::{Value, json};

const KEY: &str = "managed-hosted-fixture-managed-hosted-fixture";
const RUN: &str = "sdk-run";
const LEASE: &str = "sdk-lease";
const RUNNER: &str = "sdk-runner";

fn address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn proof(port: &str, path: &str) -> String {
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "v":1,"kind":"observation","run_id":RUN,"lease_id":LEASE,"runner_id":RUNNER,
            "exp":OffsetDateTime::now_utc().unix_timestamp()+60,"jti":"managed-hosted",
            "port":port,"method":"GET","path":path,
        }))
        .unwrap(),
    );
    let mut mac = HmacSha256::new_from_slice(KEY.as_bytes()).unwrap();
    mac.update(payload.as_bytes());
    format!("{payload}.{}", hex::encode(mac.finalize().into_bytes()))
}
fn read_request(stream: &mut TcpStream) -> (String, Value) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        assert!(bytes.len() < 1024 * 1024);
        if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    assert!(
        header
            .lines()
            .any(|line| line.eq_ignore_ascii_case("authorization: Bearer fixture-runner"))
    );
    let path = header
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let length = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
    };
    (path, body)
}

#[test]
#[ignore = "requires CPU Linux Docker, pinned Python image and production Broker TLS fixture"]
fn actual_managed_embedding_lease_materializes_redeems_verifies_and_stops() {
    let fixture_root = PathBuf::from(std::env::var("ATO_MANAGED_SDK_FIXTURE_DIR").unwrap());
    let fixture: Value =
        serde_json::from_slice(&fs::read(fixture_root.join("fixture.json")).unwrap()).unwrap();
    let origin = url::Url::parse(fixture["OPENAI_BASE_URL"].as_str().unwrap())
        .unwrap()
        .origin()
        .ascii_serialization();
    let tls = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(
            reqwest::Certificate::from_pem(&fs::read(fixture_root.join("ca.pem")).unwrap())
                .unwrap(),
        )
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let image = std::env::var("ATO_MANAGED_HOSTED_TEST_IMAGE").unwrap();
    let image_digest = image.rsplit_once('@').unwrap().1;
    let bad = std::env::var("ATO_MANAGED_HOSTED_BAD_COMPONENT").as_deref() == Ok("1");
    let parent = PathBuf::from(std::env::var("ATO_MANAGED_HOSTED_TEST_ROOT").unwrap());
    let root = tempfile::Builder::new()
        .prefix("mh")
        .tempdir_in(&parent)
        .unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("app.py"),
        include_str!("../tests/managed-hosted/app.py"),
    )
    .unwrap();
    let artifact = runtime_launch::state_artifact::pack_state_tree(&source).unwrap();
    let services:Vec<Value>=["backend","web"].into_iter().map(|name|json!({
        "name":name,"image_reference":image,"image_digest_ref":image_digest,"entrypoint":"/usr/local/bin/python3",
        "user":{"uid":65534,"gid":65534},"argv":["/app/app.py"],"working_dir":"/app","workspace_mount_path":"/app",
        "public_env":[{"name":"MODE","value":name},{"name":"PYTHONDONTWRITEBYTECODE","value":"1"},{"name":"REPORTED_DIMENSIONS","value":if bad {"1"} else {"1536"}}],
        "secret_grants":if name=="backend" {json!([{"name":"OPENAI_API_KEY","grant_ref":"portable-binding:key"},{"name":"OPENAI_BASE_URL","grant_ref":"portable-binding:url"}])} else {json!([])},
        "state_keys":[],"endpoints":[{"name":if name=="backend" {"backend.http"} else {"app.http"},"protocol":if name=="backend" {"tcp"} else {"http"},"guest_port":8080,"exposure":if name=="backend" {"internal"} else {"surface"}}],
        "readiness":if name=="backend" {json!({"kind":"tcp","endpoint_name":"backend.http","timeout_ms":10000})} else {json!({"kind":"http","endpoint_name":"app.http","path":"/health","timeout_ms":10000})},
        "resource_limits":{"cpu_limit_millis":500,"memory_bytes":134217728,"pids_limit":32},
    })).collect();
    let raw = json!({"protocol":"ato.runtime-launch-spec.v2","context":{"compute_id":"sdk-compute","compute_schema_id":"sdk-schema","compute_instance_id":"sdk-instance","run_id":RUN},
        "workspace":{"materialization_ref":artifact.digest(),"cwd_relative":""},"state_attachments":[],
        "lifecycle":{"graceful_shutdown_ms":1000,"force_kill_after_ms":2000},
        "realization":{"kind":"oci_service_group","platform":"linux/amd64","services":services,"total_limits":{"cpu_limit_millis":1000,"memory_bytes":268435456,"pids_limit":64}}});
    let spec =
        ato_ipc::runtime_launch_v2::RuntimeLaunchSpec::parse(&serde_json::to_string(&raw).unwrap())
            .unwrap();
    let digest = spec.canonical_digest().unwrap();
    let command=serde_json::from_value(json!({"run_id":RUN,"compute_instance_id":"sdk-instance","launch_spec_digest":digest,"launch_spec":raw,
        "max_duration_secs":30,"network_authorization":{"egress":[],"fixed_tcp":[],"managed_embeddings":[{"service_id":"backend","api_key_binding_id":"key","base_url_binding_id":"url","broker_origin":origin}]}})).unwrap();
    let d:BoundDerivation=serde_json::from_value(json!({"schema":"ato.derivation/1","inputs":[],"runtimes":{"oci.image":image,"oci.platform":"linux/amd64"},
        "steps":[{"id":"backend","protocol":"ato.oci@1","op":"serve","argv":["/app/app.py"],"env":{"MODE":"backend","REPORTED_DIMENSIONS":if bad {"1"} else {"1536"}}},{"id":"web","protocol":"ato.oci@1","op":"serve","argv":["/app/app.py"],"env":{"MODE":"web"}}],
        "ports":[{"id":"backend.http","from":"backend","protocol":"ato.http@1","guest_port":8080},{"id":"app.http","from":"web","protocol":"ato.http@1","guest_port":8080}],"state":[],"effects":"pure"})).unwrap();
    let expected = [
        (
            "backend-health",
            "backend.http",
            "/health",
            b"dimensions=1536\n".as_slice(),
        ),
        (
            "whole-application",
            "app.http",
            "/",
            b"application ready\n".as_slice(),
        ),
    ];
    let contract:BoundContract=serde_json::from_value(json!({"schema":"ato.contract/1","requirements":expected.iter().map(|(id,port,path,body)|json!({"id":id,"verifier":"ato.contract.http@1","port":port,"method":"GET","path":path,"status":200,"body_digest":RuntimeHttpObservation::from_response("","","",0,body).body_digest})).collect::<Vec<_>>()})).unwrap();
    let contract_ref = contract.contract_ref().unwrap();
    let d_ref = d.derivation_ref().unwrap();
    let api = TcpListener::bind("127.0.0.1:0").unwrap();
    api.set_nonblocking(true).unwrap();
    let api_base = format!("http://{}", api.local_addr().unwrap());
    let public = address();
    let hidden = address();
    let done = Arc::new(AtomicBool::new(false));
    let redeemed = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    let flags = (
        done.clone(),
        redeemed.clone(),
        stopped.clone(),
        ready.clone(),
    );
    let bytes = artifact.bytes().to_vec();
    let workspace_path = format!("/v1/runner-leases/{LEASE}/workspace/{}", artifact.digest());
    let lease_prefix = format!("/v1/runner-leases/{LEASE}");
    let expected_execution_id = digest.clone();
    let binding = fixture.clone();
    let check_tls = tls.clone();
    let saved_contract_ref = contract_ref.clone();
    let cleanup_origin = origin.clone();
    let server = thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while !flags.0.load(Ordering::Acquire) && Instant::now() < deadline {
            let (mut stream, _) = match api.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            let (path, body) = read_request(&mut stream);
            let (status, response) = if path == workspace_path {
                (200, bytes.clone())
            } else if path == format!("{lease_prefix}/runtime-bindings") {
                flags.1.store(true, Ordering::Release);
                (
                    200,
                    serde_json::to_vec(&json!({"bindings":binding})).unwrap(),
                )
            } else if path == format!("{lease_prefix}/data") {
                (404, b"{}".to_vec())
            } else if path == format!("{lease_prefix}/control") {
                (
                    200,
                    serde_json::to_vec(&json!({"stop_requested":flags.3.load(Ordering::Acquire)}))
                        .unwrap(),
                )
            } else if path == format!("{lease_prefix}/ready") {
                assert!(flags.1.load(Ordering::Acquire));
                assert_eq!(body["execution_id"], expected_execution_id);
                assert!(
                    body["local_port"]
                        .as_u64()
                        .is_some_and(|port| port > 0 && port <= 65535)
                );
                // The private component cannot be observed without a scoped proof.
                assert_eq!(
                    client
                        .get(format!("http://{public}/health"))
                        .send()
                        .unwrap()
                        .status()
                        .as_u16(),
                    403
                );
                assert_eq!(body["execution"]["realization"], "oci_service_group");
                assert_eq!(body["execution"]["services"].as_array().unwrap().len(), 2);
                let observations = expected
                    .iter()
                    .map(|(_, port, path, _)| {
                        let response = client
                            .get(format!("http://{public}{path}"))
                            .header(RUN_ASSERTION_HEADER, proof(port, path))
                            .send()
                            .unwrap();
                        let status = response.status().as_u16();
                        let bytes = response.bytes().unwrap();
                        RuntimeHttpObservation::from_response(*port, "GET", *path, status, &bytes)
                    })
                    .collect();
                let attempt = AttemptSpec {
                    contract: &contract,
                    contract_ref: &contract_ref,
                    derivation: &d,
                    derivation_ref: &d_ref,
                    shape: CandidateShape::Process,
                    input_refs: BTreeMap::new(),
                    instance_snapshot_ref: None,
                };
                // Runtime principal facts belong to the ready report. The shared
                // receipt schema carries its explicit, smaller evidence projection.
                let mut projected = body["execution"].clone();
                projected.as_object_mut().unwrap().remove("principal");
                for service in projected["services"].as_array_mut().unwrap() {
                    assert!(service["principal"].is_object());
                    service.as_object_mut().unwrap().remove("principal");
                }
                projected["run_id"] = json!(RUN);
                projected["lease_id"] = json!(LEASE);
                let evidence: VerificationExecutionEvidence =
                    serde_json::from_value(projected).unwrap();
                let receipt = verify_observed_candidate(
                    &attempt,
                    observations,
                    evidence,
                    &ReceiptContext {
                        target: VerificationTargetKind::AtoRunHosted,
                        bundle_sha256: None,
                        portability_profile: None,
                        run_id: Some(RUN),
                    },
                )
                .receipt;
                assert_eq!(receipt.fully_satisfied, !bad);
                assert_eq!(contract.contract_ref().unwrap(), saved_contract_ref);
                assert_eq!(receipt.observations.len(), 2);
                eprintln!(
                    "managed Hosted fixture: {}",
                    serde_json::to_string(&receipt).unwrap()
                );
                assert!(
                    check_tls
                        .post(format!("{origin}/_fixture/stop"))
                        .send()
                        .unwrap()
                        .status()
                        .is_success()
                );
                flags.3.store(true, Ordering::Release);
                (200, b"{}".to_vec())
            } else if path == format!("{lease_prefix}/status") {
                assert_eq!(body["status"], "preparing");
                (200, b"{}".to_vec())
            } else if path == format!("{lease_prefix}/stopped") {
                assert_eq!(body["execution_id"], expected_execution_id);
                flags.2.store(true, Ordering::Release);
                (200, b"{}".to_vec())
            } else {
                panic!("unexpected fixture path {path}")
            };
            write!(
                stream,
                "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(&response).unwrap();
        }
    });
    let work_root = root.path().join("runner");
    fs::create_dir(&work_root).unwrap();
    fs::create_dir(work_root.join("leases")).unwrap();
    let slot = format!("hosted-{}", std::process::id());
    let config = WorkerConfig::try_parse_from([
        "runner",
        "--api-base",
        &api_base,
        "--runner-id",
        RUNNER,
        "--runner-token",
        "fixture-runner",
        "--work-root",
        work_root.to_str().unwrap(),
        "--surface-target",
        "127.0.0.1:8080",
        "--surface-listen",
        &public.to_string(),
        "--hidden-surface-listen",
        &hidden.to_string(),
        "--public-base-url",
        "https://managed-fixture.example",
        "--network-controls",
        "--slot-id",
        &slot,
    ])
    .unwrap();
    let worker = ConnectedWorker {
        api: HttpRunnerApi::new(&api_base, RUNNER, "fixture-runner").unwrap(),
        config,
        volume_store: None,
        fixed_tcp: runtime_launch::network_broker::FixedTcpRegistry::new("").unwrap(),
    };
    let lease = ClaimedLease {
        id: LEASE.into(),
        run_id: RUN.into(),
        command: LeaseCommand::RuntimeLaunch(Box::new(command)),
        expires_at: None,
        proxy_assertion_key: Some(KEY.into()),
    };
    let previous = runtime_launch::recovery::slot_recovered();
    runtime_launch::recovery::mark_slot_recovered(true);
    let started = Instant::now();
    let result =
        runtime_launch::managed_embedding_relay::with_fixture_http_client(tls.clone(), || {
            worker.execute_lease(&lease)
        });
    runtime_launch::recovery::mark_slot_recovered(previous);
    done.store(true, Ordering::Release);
    let server_result = server.join();
    // Revoke the test capability on every outcome, including startup failure.
    let revoke = tls.post(format!("{cleanup_origin}/_fixture/stop")).send();
    assert!(!work_root.join("leases").join(LEASE).exists());
    assert!(TcpStream::connect(public).is_err());
    let journal = runtime_launch::recovery::RunJournal::new(&work_root, RUNNER, &slot).unwrap();
    assert!(journal.load().unwrap().is_empty());
    let resources = ato_adapter_oci::OwnedResourceScanner::new(RUNNER, &slot)
        .unwrap()
        .scan()
        .unwrap();
    assert!(resources.containers.is_empty() && resources.networks.is_empty());
    assert!(revoke.unwrap().status().is_success());
    server_result.unwrap();
    result.unwrap();
    assert!(
        redeemed.load(Ordering::Acquire)
            && ready.load(Ordering::Acquire)
            && stopped.load(Ordering::Acquire)
    );
    eprintln!(
        "managed Hosted fixture complete: elapsed_ms={} bad_component={bad}",
        started.elapsed().as_millis()
    );
}
