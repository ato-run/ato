//! Actual lease evaluator tests: fake control-plane transport, real isolated
//! Docker workload/PTY/Native probe/scoped proxy, existing Rust K authority.
use super::*;
use runtime_launch::lease::{RuntimeLaunchLeaseCommand, verified_spec};
use serde_json::json;

#[test]
fn native_advertisement_requires_opt_in_linux_oci_and_never_claims_pixel() {
    assert_eq!(
        NATIVE_TERMINAL_RUNTIME_FEATURE,
        ato_formation::native_surface::NATIVE_SURFACE_RUNTIME_FEATURE
    );
    for enabled in [false, true] {
        for linux in [false, true] {
            for oci in [false, true] {
                let native = native_terminal_available(enabled, linux, oci);
                assert_eq!(native, enabled && linux && oci);
                let caps = with_native_terminal_capability(
                    runner_capabilities(false, oci, false, false, false),
                    native,
                );
                assert_eq!(caps.contains(&NATIVE_TERMINAL_RUNTIME_FEATURE), native);
                let surfaces = supported_session_surfaces(native, false);
                assert_eq!(
                    surfaces.as_array().unwrap().len(),
                    if native { 2 } else { 1 }
                );
                assert!(!surfaces.to_string().contains("pixel"));
                assert_eq!(surfaces[0]["kind"], "web");
                if native {
                    assert_eq!(
                        surfaces[1],
                        json!({"kind":"terminal","profiles":["ato.terminal-surface.v1"],"transports":["terminal_websocket"]})
                    );
                }
            }
        }
    }
}
#[test]
fn pixel_typed_profile_is_separate_from_terminal_and_uses_existing_probe_feature() {
    let surfaces = supported_session_surfaces(false, true);
    assert_eq!(
        surfaces,
        json!([
            {"kind":"web","profiles":["ato.web-surface.v1"],"transports":["https"]},
            {"kind":"pixel_stream","profiles":["ato.pixel-stream.v1"],"transports":["rfb_websocket"]}
        ])
    );
    assert!(
        with_native_terminal_capability(vec![], true).contains(&NATIVE_TERMINAL_RUNTIME_FEATURE)
    );
    assert_eq!(
        supported_session_surfaces(true, true)
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn pixel_wire_preserves_exact_d_descriptor_and_refuses_promised_tcp_facts() {
    let (spec, descriptor, _, d) = runtime_launch::native::pixel::tests::fixture(&format!(
        "example.invalid/gui@sha256:{}",
        "33".repeat(32)
    ));
    let command = native_command(&spec, descriptor, &d.derivation_ref().unwrap());
    if cfg!(target_os = "linux") {
        assert!(verified_spec(&command).is_ok());
    }
    for invalid in ["missing-ports", "tcp-facts", "wrong-d", "changed-size"] {
        let mut command = native_command(
            &spec,
            command.session_surface.clone().unwrap(),
            &d.derivation_ref().unwrap(),
        );
        let mut spec = spec.clone();
        let ato_ipc::runtime_launch::LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
            unreachable!()
        };
        match invalid {
            "missing-ports" => {
                oci.native_surface
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("ports");
            }
            "tcp-facts" => {
                oci.native_surface.as_mut().unwrap()["ports"][0]["guest_port"] = json!(5900);
                oci.native_surface.as_mut().unwrap()["ports"][1]["guest_port"] = json!(8081);
            }
            "wrong-d" => {
                command.portable_derivation_ref = Some(format!("sha256:{}", "ff".repeat(32)))
            }
            "changed-size" => {
                if let Some(ato_ipc::session_surface::SessionSurfaceDescriptor::PixelStream {
                    viewport,
                    ..
                }) = &mut command.session_surface
                {
                    viewport.width += 1;
                }
            }
            _ => unreachable!(),
        }
        command.launch_spec_digest = spec.canonical_digest().unwrap();
        command.launch_spec = serde_json::to_value(&spec).unwrap();
        assert!(verified_spec(&command).is_err(), "{invalid}");
    }
}
fn native_command(
    spec: &ato_ipc::runtime_launch::RuntimeLaunchSpecV1,
    descriptor: ato_ipc::session_surface::SessionSurfaceDescriptor,
    selected_ref: &str,
) -> RuntimeLaunchLeaseCommand {
    serde_json::from_value(json!({"run_id":spec.context.run_id,"compute_instance_id":spec.context.compute_instance_id,
        "launch_spec":spec,"launch_spec_digest":spec.canonical_digest().unwrap(),"portable_derivation_ref":selected_ref,
        "session_surface":descriptor,"session_id":format!("run:{}",spec.context.run_id),"surface_origin":"https://staging.ato.run",
        "surface_contract_version":"1","max_duration_secs":30})).unwrap()
}
#[test]
fn native_command_requires_exact_session_policy_before_materialization() {
    let (spec, descriptor, _, d) = runtime_launch::native::tests::fixture(&format!(
        "example.invalid/test@sha256:{}",
        "22".repeat(32)
    ));
    let command = native_command(&spec, descriptor, &d.derivation_ref().unwrap());
    if cfg!(target_os = "linux") {
        assert!(verified_spec(&command).is_ok());
    } else {
        assert!(verified_spec(&command).is_err());
        return;
    }
    let wire = serde_json::to_value(&command.launch_spec).unwrap();
    for patch in [
        json!({"session_id":null}),
        json!({"session_id":"run:other"}),
        json!({"surface_origin":null}),
        json!({"surface_origin":"https://staging.ato.run/"}),
        json!({"surface_origin":"https://staging.ato.run.evil/#x"}),
        json!({"surface_origin":"http://staging.ato.run"}),
        json!({"surface_origin":"https://user:secret@staging.ato.run"}),
        json!({"surface_contract_version":null}),
        json!({"surface_contract_version":"2"}),
        json!({"max_duration_secs":null}),
    ] {
        let mut raw = json!({"run_id":spec.context.run_id,"compute_instance_id":spec.context.compute_instance_id,
            "launch_spec":wire,"launch_spec_digest":spec.canonical_digest().unwrap(),"portable_derivation_ref":d.derivation_ref().unwrap(),
            "session_surface":command.session_surface,"session_id":command.session_id,"surface_origin":command.surface_origin,
            "surface_contract_version":"1","max_duration_secs":30});
        for (key, value) in patch.as_object().unwrap() {
            raw[key] = value.clone();
        }
        let invalid = serde_json::from_value(raw).unwrap();
        assert!(verified_spec(&invalid).is_err(), "{patch}");
    }
}

fn free_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn observation_proof(key: &str, run: &str, lease: &str, runner: &str, port: &str) -> String {
    let encoded=URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"v":1,"kind":"observation","run_id":run,
        "lease_id":lease,"runner_id":runner,"exp":OffsetDateTime::now_utc().unix_timestamp()+60,"jti":"native-test",
        "port":port,"method":"GET","path":"/result"})).unwrap());
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
    mac.update(encoded.as_bytes());
    format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
}
fn read_api_request(stream: &mut TcpStream) -> Option<(String, serde_json::Value)> {
    use std::io::Read;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).unwrap();
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..n]);
        assert!(bytes.len() < 1024 * 1024);
        if let Some(at) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let header = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
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
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let n = stream.read(&mut buf).unwrap();
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&buf[..n]);
        assert!(bytes.len() < 1024 * 1024);
    }
    let body = if length == 0 {
        json!(null)
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
    };
    Some((path, body))
}

#[test]
#[ignore = "requires real Linux Docker and explicit already-present pinned CPU Python image"]
fn real_native_lease_dispatch_owns_journal_mux_ready_and_stop_with_actual_k() {
    run_actual_native_dispatch(false, false);
}

#[test]
#[ignore = "requires local Linux Docker, explicit verified warm helper and independent static X11 workspace executable"]
fn real_pixel_lease_dispatch_owns_two_workloads_mux_original_k_and_stop() {
    run_actual_native_dispatch(true, false);
}

#[test]
#[ignore = "requires real local Linux Docker and independent immutable GUI/helper fixtures"]
fn real_pixel_contract_mismatch_never_grants_viewer_access_and_reclaims_both() {
    run_actual_native_dispatch(true, true);
}

fn run_actual_native_dispatch(pixel: bool, mismatch: bool) {
    use ato_formation::verify::{
        RuntimeHttpObservation, VerificationExecutionEvidence, VerificationTargetKind,
    };
    use ato_runtime_attempt::{
        spec::{AttemptSpec, CandidateShape},
        verification::{ReceiptContext, verify_observed_candidate},
    };
    use std::io::Write;
    const KEY: &str = "native-test-key-native-test-key-native-test-key";
    let image = if pixel {
        "ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf"
            .to_owned()
    } else {
        std::env::var("ATO_NATIVE_TERMINAL_TEST_IMAGE").unwrap()
    };
    let (mut spec, descriptor, mut contract, mut d) = if pixel {
        runtime_launch::native::pixel::tests::fixture(&image)
    } else {
        runtime_launch::native::tests::fixture(&image)
    };
    // Independent negative K authored before the Run, never derived from
    // its frame or substituted into the positive fixture after measurement.
    if mismatch {
        contract.requirements[0].body_digest = Some(format!("sha256:{}", "00".repeat(32)));
    }
    let expected_body = if pixel {
        runtime_launch::native::pixel::tests::expected()
    } else {
        b"go\r\n1\r\nEND".to_vec()
    };
    if pixel {
        d.runtimes
            .insert("oci.entrypoint".into(), "/app/native-app".into());
        let declaration = ato_runtime_attempt::native_launch::NativeLaunchDeclaration::new(
            d.derivation_ref().unwrap(),
            d.native_surface.clone().unwrap(),
        )
        .unwrap()
        .with_ports(d.ports.clone())
        .unwrap();
        let ato_ipc::runtime_launch::LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
            unreachable!()
        };
        oci.entrypoint = Some("/app/native-app".into());
        oci.native_surface = Some(serde_json::to_value(declaration).unwrap());
    }
    let root = runtime_launch::native::tests::scratch();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    if pixel {
        fs::copy(
            std::env::var_os("ATO_NATIVE_PIXEL_STATIC_APP").unwrap(),
            source.join("native-app"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(source.join("native-app"), fs::Permissions::from_mode(0o755)).unwrap();
    } else {
        fs::write(
            source.join("main.py"),
            runtime_launch::native::tests::WORKLOAD,
        )
        .unwrap();
    }
    let artifact = runtime_launch::state_artifact::pack_state_tree(&source).unwrap();
    spec.workspace.materialization_ref = artifact.digest().to_owned();
    let command = native_command(&spec, descriptor.clone(), &d.derivation_ref().unwrap());
    assert!(verified_spec(&command).is_ok());
    let run_id = spec.context.run_id.clone();
    let lease_id = format!(
        "native-dispatch-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    );
    let runner_id = format!("native-dispatch-runner-{}", std::process::id());
    let public = free_address();
    let hidden = free_address();
    let api = TcpListener::bind("127.0.0.1:0").unwrap();
    let api_base = format!("http://{}", api.local_addr().unwrap());
    api.set_nonblocking(true).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let verified = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    let worker_done = Arc::clone(&done);
    let worker_stopped = Arc::clone(&stopped);
    let worker_verified = Arc::clone(&verified);
    let worker_ready = Arc::clone(&ready);
    let work_run = run_id.clone();
    let work_lease = lease_id.clone();
    let work_runner = runner_id.clone();
    let selected_ref = d.derivation_ref().unwrap();
    let dispatched_ref = selected_ref.clone();
    let contract_ref = contract.contract_ref().unwrap();
    let gateway_scope = netd::surface_authorization::SurfaceGatewayScope {
        session_id: format!("run:{run_id}"),
        surface_id: descriptor.surface_id().unwrap().to_owned(),
    };
    let bytes = artifact.bytes().to_vec();
    let digest = spec.canonical_digest().unwrap();
    let server = thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        while !worker_done.load(Ordering::Acquire) {
            let (mut stream, _) = match api.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            let Some((path, body)) = read_api_request(&mut stream) else {
                continue;
            };
            let (status, response) = if path.contains("/workspace/") {
                (200, bytes.clone())
            } else if path.ends_with("/data") {
                (404, b"{}".to_vec())
            } else if path.ends_with("/control") {
                (
                    200,
                    serde_json::to_vec(
                        &json!({"stop_requested":worker_ready.load(Ordering::Acquire)}),
                    )
                    .unwrap(),
                )
            } else if path.ends_with("/ready") {
                assert!(!worker_ready.load(Ordering::Acquire));
                assert_eq!(body["execution_id"], digest);
                assert_eq!(body["local_port"], hidden.port());
                assert!(body.get("port_mapping").is_none());
                assert_eq!(body["execution"]["realization"], "oci");
                assert_eq!(
                    body["execution"]["principal"]["uids"],
                    json!([65532, 65532, 65532, 65532])
                );
                assert!(!worker_verified.load(Ordering::Acquire));
                let denied = client
                    .get(format!("http://{public}/result"))
                    .send()
                    .unwrap();
                assert_eq!(denied.status().as_u16(), 403);
                let wrong =
                    observation_proof(KEY, &work_run, &work_lease, &work_runner, "app.native");
                assert_eq!(
                    client
                        .get(format!("http://{public}/result"))
                        .header(RUN_ASSERTION_HEADER, wrong)
                        .send()
                        .unwrap()
                        .status()
                        .as_u16(),
                    403
                );
                let proof =
                    observation_proof(KEY, &work_run, &work_lease, &work_runner, "app.probe");
                let actual = client
                    .get(format!("http://{public}/result"))
                    .header(RUN_ASSERTION_HEADER, &proof)
                    .send()
                    .unwrap()
                    .bytes()
                    .unwrap();
                assert_eq!(actual.as_ref(), expected_body);
                let attempt = AttemptSpec {
                    contract: &contract,
                    contract_ref: &contract_ref,
                    derivation: &d,
                    derivation_ref: &selected_ref,
                    shape: CandidateShape::Process,
                    input_refs: BTreeMap::new(),
                    instance_snapshot_ref: None,
                };
                let services: Vec<_> = body["execution"]["services"].as_array().into_iter().flatten().map(|service| json!({
                    "name":service["name"], "container_id":service["container_id"], "image":service["image"]
                })).collect();
                let evidence: VerificationExecutionEvidence = serde_json::from_value(json!({
                    "realization":"oci", "container_id":body["execution"]["container_id"], "lease_id":work_lease,
                    "image":body["execution"]["image"], "platform":body["execution"]["platform"], "services": services
                })).unwrap();
                let receipt = verify_observed_candidate(
                    &attempt,
                    vec![RuntimeHttpObservation::from_response(
                        "app.probe",
                        "GET",
                        "/result",
                        200,
                        &actual,
                    )],
                    evidence,
                    &ReceiptContext {
                        target: VerificationTargetKind::AtoRunHosted,
                        bundle_sha256: None,
                        portability_profile: None,
                        run_id: Some(&work_run),
                    },
                )
                .receipt;
                assert_eq!(receipt.fully_satisfied, !mismatch);
                assert_eq!(
                    receipt.execution.as_ref().unwrap().services.len(),
                    usize::from(pixel)
                );
                assert_eq!(contract.contract_ref().unwrap(), contract_ref);
                if !mismatch {
                    worker_verified.store(true, Ordering::Release);
                    // A real API counterpart must enforce this same current-attempt
                    // receipt gate before issuing a browser credential.
                    let keys =
                        netd::pixel_authorization::SurfaceAssertionKeyring::new(BTreeMap::from([
                            ("ato.runner-surface@1".into(), KEY.into()),
                        ]))
                        .unwrap();
                    let scope = ato_ipc::session_surface::SurfaceExecutionScope {
                        run_id: work_run.clone(),
                        lease_id: work_lease.clone(),
                        runner_id: work_runner.clone(),
                    };
                    let assertion = keys
                        .issue_readiness_assertion_for_execution(&gateway_scope, Some(scope))
                        .unwrap();
                    if pixel {
                        let frame = tokio::runtime::Runtime::new()
                            .unwrap()
                            .block_on(netd::rfb_probe::wait_for_authenticated_gateway_frame(
                                netd::rfb_probe::PixelGatewayProbeRequest::new(
                                    &format!("http://{public}/__ato/surface"),
                                    "https://staging.ato.run",
                                    assertion.as_str(),
                                ),
                                Duration::from_secs(3),
                            ))
                            .unwrap();
                        assert_eq!((frame.width, frame.height), (640, 480));
                        let services = body["execution"]["services"].as_array().unwrap();
                        assert_eq!(services.len(), 1);
                        assert_eq!(services[0]["name"], "native-display");
                        assert_ne!(
                            services[0]["container_id"],
                            body["execution"]["container_id"]
                        );
                        assert!(services[0]["principal"]["uids"][0].as_u64().unwrap() > 0);
                    } else {
                        let viewed = runtime_launch::native::tests::viewer(
                            public,
                            assertion.as_str().to_owned(),
                            "https://staging.ato.run",
                            b"go\r",
                            true,
                        )
                        .unwrap();
                        assert_eq!(viewed.2, b"go\r\n2\r\nEND");
                    }
                }
                assert_eq!(
                    client
                        .get(format!("http://{public}/result"))
                        .header(RUN_ASSERTION_HEADER, proof)
                        .send()
                        .unwrap()
                        .bytes()
                        .unwrap(),
                    actual
                );
                worker_ready.store(true, Ordering::Release);
                (200, b"{}".to_vec())
            } else if path.ends_with("/status") {
                assert_eq!(body["status"], "preparing");
                (200, b"{}".to_vec())
            } else if path.ends_with("/stopped") {
                worker_stopped.store(true, Ordering::Release);
                (200, b"{}".to_vec())
            } else {
                panic!("unexpected fake control-plane path {path}")
            };
            write!(
                stream,
                "HTTP/1.1 {status} test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            )
            .unwrap();
            stream.write_all(&response).unwrap();
        }
    });
    let work_root = root.path().join("runner");
    let lease_root = work_root.join("leases").join(&lease_id);
    fs::create_dir_all(work_root.join("leases")).unwrap();
    let mut config = WorkerConfig::try_parse_from([
        "runner",
        "--api-base",
        &api_base,
        "--runner-id",
        &runner_id,
        "--runner-token",
        "fixture-token",
        "--work-root",
        work_root.to_str().unwrap(),
        "--surface-target",
        "127.0.0.1:8080",
        "--surface-listen",
        &public.to_string(),
        "--hidden-surface-listen",
        &hidden.to_string(),
        "--public-base-url",
        "https://native-runner.example",
        "--native-terminal",
    ])
    .unwrap();
    if pixel {
        config.native_pixel.enabled = true;
        config.native_pixel.helper_archive = Some(
            std::env::var_os("ATO_NATIVE_PIXEL_HELPER_ARCHIVE")
                .unwrap()
                .into(),
        );
        config.native_pixel.helper_image =
            Some(std::env::var("ATO_NATIVE_PIXEL_HELPER_IMAGE").unwrap());
        config.native_pixel.cgroup_parent = Some("system.slice".into());
        config.native_pixel.prepare(&work_root, 1).unwrap();
    }
    let mut worker = ConnectedWorker {
        api: HttpRunnerApi::new(&api_base, &runner_id, "fixture-token").unwrap(),
        config,
        volume_store: None,
        fixed_tcp: runtime_launch::network_broker::FixedTcpRegistry::new("").unwrap(),
    };
    let mut lease=ClaimedLease{id:lease_id.clone(),run_id:run_id.clone(),command:LeaseCommand::RuntimeLaunch(serde_json::from_value(json!({"run_id":"unused","compute_instance_id":"unused","launch_spec_digest":"unused","launch_spec":null})).unwrap()),expires_at:None,proxy_assertion_key:Some(KEY.into())};
    let previous = runtime_launch::recovery::slot_recovered();
    runtime_launch::recovery::mark_slot_recovered(true);
    worker.config.native_terminal = false;
    worker.config.native_pixel.enabled = false;
    assert!(
        worker
            .execute_runtime_launch_lease(&lease, &command, &lease_root)
            .unwrap_err()
            .to_string()
            .contains(if pixel {
                "Pixel is disabled"
            } else {
                "not enabled"
            })
    );
    worker.config.native_terminal = !pixel;
    worker.config.native_pixel.enabled = pixel;
    for invalid in [None, Some("weak".to_owned())] {
        lease.proxy_assertion_key = invalid;
        assert!(
            worker
                .execute_runtime_launch_lease(&lease, &command, &lease_root)
                .unwrap_err()
                .to_string()
                .contains("scoped Runner assertion key")
        );
        assert!(!lease_root.join("workspace").exists());
    }
    lease.proxy_assertion_key = Some(KEY.into());
    lease.command =
        LeaseCommand::RuntimeLaunch(Box::new(native_command(&spec, descriptor, &dispatched_ref)));
    let evaluated_at = Instant::now();
    let result = worker.execute_lease(&lease);
    eprintln!(
        "NATIVE_DISPATCH_EVIDENCE {}",
        json!({"pixel":pixel,"authored_contract_mismatch":mismatch,
        "fully_satisfied":verified.load(Ordering::Acquire),"lease_execution_ms":evaluated_at.elapsed().as_millis(),
        "selected_derivation_ref":dispatched_ref})
    );
    runtime_launch::recovery::mark_slot_recovered(previous);
    done.store(true, Ordering::Release);
    server.join().unwrap();
    result.unwrap();
    assert!(
        ready.load(Ordering::Acquire)
            && verified.load(Ordering::Acquire) != mismatch
            && stopped.load(Ordering::Acquire)
    );
    assert!(TcpStream::connect(public).is_err());
    assert!(TcpStream::connect(hidden).is_err());
    let journal = runtime_launch::recovery::RunJournal::new(&work_root, &runner_id, "0").unwrap();
    assert!(journal.load().unwrap().is_empty());
    assert!(
        !lease_root.exists(),
        "confirmed/report-acknowledged owner directory removed"
    );
    let resources = ato_adapter_oci::OwnedResourceScanner::new(&runner_id, "0")
        .unwrap()
        .scan()
        .unwrap();
    assert!(resources.containers.is_empty() && resources.networks.is_empty());
}

#[test]
#[ignore = "requires real Linux Docker and explicit already-present pinned CPU Python image"]
fn real_native_startup_control_cancels_owned_workload_before_ready() {
    let image = std::env::var("ATO_NATIVE_TERMINAL_TEST_IMAGE").unwrap();
    let (spec, descriptor, _, d) = runtime_launch::native::tests::fixture(&image);
    let root = runtime_launch::native::tests::scratch();
    let context = runtime_launch::native::tests::context(
        root.path(),
        "import sys,time\nsys.stdin.readline()\ntime.sleep(120)\n",
    );
    let owner = runtime_launch::native::tests::owner(&spec);
    let mut observed_creation = false;
    let mut control = || -> Result<()> {
        if root.path().join("oci-runtime/ownership.json").exists() {
            observed_creation = true;
            return Err(ato_runtime_attempt::startup::StartupCancelled.into());
        }
        Ok(())
    };
    let started = Instant::now();
    let error = runtime_launch::native::NativeLeaseRun::start(
        &spec,
        &d.derivation_ref().unwrap(),
        &descriptor,
        &context,
        &owner,
        runtime_launch::native::tests::config(),
        None,
        &mut control,
    )
    .err()
    .unwrap();
    assert!(observed_creation);
    assert!(
        error
            .error
            .is::<ato_runtime_attempt::startup::StartupCancelled>()
    );
    assert!(error.stop.is_confirmed(), "{error}");
    assert!(started.elapsed() < Duration::from_secs(4));
    let resources = ato_adapter_oci::OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)
        .unwrap()
        .scan()
        .unwrap();
    assert!(
        !resources
            .containers
            .iter()
            .any(|item| item.lease_id() == Some(&owner.lease_id))
    );
    assert!(
        !resources
            .networks
            .iter()
            .any(|item| item.lease_id() == Some(&owner.lease_id))
    );
}

#[test]
fn bounded_control_and_renew_requests_cannot_wait_for_the_default_client_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = HttpRunnerApi::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "runner",
        "fixture",
    )
    .unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_api_request(&mut stream);
            thread::sleep(Duration::from_millis(200));
        }
    });
    let started = Instant::now();
    let error = api
        .control_with_timeout("lease", Duration::from_millis(50))
        .unwrap_err();
    assert_eq!(classify_comm_error(&error), CommErrorKind::Transient);
    let error = api
        .renew_execution_authorization_with_timeout("lease", Duration::from_millis(50))
        .unwrap_err();
    assert_eq!(classify_comm_error(&error), CommErrorKind::Transient);
    assert!(started.elapsed() < Duration::from_secs(1));
    server.join().unwrap();
}
