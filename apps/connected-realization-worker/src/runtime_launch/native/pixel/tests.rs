#[path = "controls_acceptance.rs"]
pub(crate) mod controls_acceptance;
use super::super::tests as terminal;
use super::*;
use ato_formation::{
    authoring::{BoundContract, BoundDerivation},
    native_surface::{NativePresentation, NativeProbeOperation, pixel_probe_result},
    verify::{RuntimeHttpObservation, VerificationExecutionEvidence, VerificationTargetKind},
};
use ato_runtime_attempt::{
    spec::{AttemptSpec, CandidateShape},
    verification::{ReceiptContext, verify_observed_candidate},
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) fn fixture(
    image: &str,
) -> (
    RuntimeLaunchSpecV1,
    SessionSurfaceDescriptor,
    BoundContract,
    BoundDerivation,
) {
    let (mut spec, _, mut k, mut d) = terminal::fixture(image);
    d.runtimes
        .insert("oci.entrypoint".into(), "/usr/local/bin/gui".into());
    d.steps[0].argv = vec!["run".into()];
    d.ports[0].protocol = "ato.tcp@1".into();
    let adapter = d.native_surface.as_mut().unwrap();
    adapter.surface = NativePresentation::PixelStream {
        port: "app.native".into(),
        width: 640,
        height: 480,
    };
    adapter.operations = vec![NativeProbeOperation::PixelKeyFrame {
        path: "/result".into(),
        keysyms: vec![0x67],
        max_frame_bytes: 640 * 480 * 4,
        timeout_ms: 2000,
        settle_ms: 50,
    }];
    let adapter = adapter.clone();
    let declaration = NativeLaunchDeclaration::new(d.derivation_ref().unwrap(), adapter)
        .unwrap()
        .with_ports(d.ports.clone())
        .unwrap();
    let descriptor = declaration
        .session_surface_descriptor("native-terminal")
        .unwrap();
    let LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
        unreachable!()
    };
    oci.entrypoint = Some("/usr/local/bin/gui".into());
    oci.argv = Some(vec!["run".into()]);
    oci.native_surface = Some(serde_json::to_value(declaration).unwrap());
    // Authored before execution: the fixture fills every pixel with 0x336699
    // on its first key. No expected pixels/digest are read from the Run.
    k.requirements[0].body_digest =
        Some(RuntimeHttpObservation::from_response("", "", "", 0, &expected()).body_digest);
    (spec, descriptor, k, d)
}
pub(crate) fn expected() -> Vec<u8> {
    pixel_probe_result(&[0x99, 0x66, 0x33, 0].repeat(640 * 480)).unwrap()
}
fn stub_helper() -> PixelHelper {
    PixelHelper {
        controls: PixelGatewayControls::default(),
        image: format!("missing.invalid/helper@sha256:{}", "11".repeat(32)),
        platform: "linux/amd64".into(),
        archive: Arc::new(vec![]),
        config_reference: format!("sha256:{}", "22".repeat(32)),
        limits: limits(),
        owner: None,
        prepared_user: None,
    }
}
fn limits() -> OciResourceLimits {
    OciResourceLimits {
        memory_bytes: 256 * 1024 * 1024,
        cpu_limit_millis: 500,
        pids_limit: 32,
    }
}
fn helper() -> PixelHelper {
    let path = std::env::var_os("ATO_NATIVE_PIXEL_HELPER_ARCHIVE")
        .expect("explicit verified helper JSON archive path");
    PixelHelper::from_archive(
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
        limits(),
    )
    .unwrap()
}
fn app_archive() -> PortableOciArchive {
    let path = std::env::var_os("ATO_NATIVE_PIXEL_APP_ARCHIVE")
        .expect("explicit immutable GUI app archive");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn image() -> String {
    app_archive().image
}

async fn get(addr: SocketAddr) -> Vec<u8> {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /result HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut data = vec![];
    s.read_to_end(&mut data).await.unwrap();
    assert!(data.starts_with(b"HTTP/1.1 200"));
    data.split_off(data.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4)
}
fn assert_no_resources(owner: &OciOwner) {
    let resources = ato_adapter_oci::OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)
        .unwrap()
        .scan()
        .unwrap();
    assert!(
        resources
            .containers
            .iter()
            .all(|r| r.lease_id() != Some(owner.lease_id.as_str()))
    );
    assert!(
        resources
            .networks
            .iter()
            .all(|r| r.lease_id() != Some(owner.lease_id.as_str()))
    );
}
async fn closed(addr: SocketAddr) {
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

#[test]
fn display_cookie_is_fresh_private_delegated_and_lifetime_cleaned() {
    let root = terminal::scratch();
    let runtime = root.path().join("runtime");
    let dirs = DisplayDirectories::create(&runtime).unwrap();
    let cookie = std::fs::read(dirs.path("auth/Xauthority")).unwrap();
    assert_eq!(&cookie[..2], &[255, 255]);
    assert_eq!(
        std::fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(dirs.path("auth/Xauthority"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
    assert!(DisplayDirectories::create(&runtime).is_err()); // Never reuse old credentials.
    let path = dirs.root.clone();
    drop(dirs);
    assert!(!path.exists());
    let dirs = DisplayDirectories::create(&runtime).unwrap();
    assert_ne!(cookie, std::fs::read(dirs.path("auth/Xauthority")).unwrap());
    let path = dirs.root.clone();
    dirs.retain_for_recovery();
    assert!(path.exists());
    assert!(!path.join("auth/Xauthority").exists());
}

#[tokio::test]
async fn gui_refuses_invalid_scope_config_declared_env_mounts_and_tcp_facts_before_actions() {
    for invalid in [
        "key",
        "origin",
        "scope",
        "display",
        "auth",
        "mount",
        "workspace",
        "ports",
        "legacy",
        "selection",
        "platform",
        "root_user",
        "controls_mismatch",
    ] {
        let (mut spec, descriptor, _, d) =
            fixture(&format!("missing.invalid/app@sha256:{}", "33".repeat(32)));
        let root = terminal::scratch();
        let context = terminal::context(root.path(), "");
        let owner = terminal::owner(&spec);
        let mut config = terminal::config();
        let mut selected = d.derivation_ref().unwrap();
        let mut helper = stub_helper();
        let LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
            unreachable!()
        };
        match invalid {
            "controls_mismatch" => config.controls.text_paste = true,
            "key" => config.verification_key = "weak".into(),
            "origin" => config.origin = "http://untrusted.example".into(),
            "scope" => config.scope.surface_id = "other".into(),
            "display" => {
                spec.public_env.push(ato_ipc::runtime_launch::PublicEnvV1 {
                    name: "DISPLAY".into(),
                    value: ":0".into(),
                });
            }
            "auth" => {
                spec.public_env.push(ato_ipc::runtime_launch::PublicEnvV1 {
                    name: "XAUTHORITY".into(),
                    value: "/host/cookie".into(),
                });
            }
            "mount" => {
                oci.workspace_mount_path = Some("/run".into());
            }
            "workspace" => {
                oci.workspace_mount_path = Some("/tmp/.X11-unix/sub".into());
            }
            "ports" => {
                oci.native_surface.as_mut().unwrap()["ports"][0]["guest_port"] = json!(5900);
                oci.native_surface.as_mut().unwrap()["ports"][1]["guest_port"] = json!(8081);
            }
            "legacy" => {
                oci.native_surface
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("ports");
            }
            "selection" => selected = format!("sha256:{}", "44".repeat(32)),
            "platform" => helper.platform = "linux/arm64".into(),
            "root_user" => oci.user = None,
            _ => unreachable!(),
        }
        let (_alive, rx) = watch::channel(true);
        let failure = start_pixel(
            &spec,
            &selected,
            &descriptor,
            &context,
            &owner,
            config,
            helper,
            None,
            rx,
        )
        .await
        .err()
        .unwrap();
        assert!(failure.stop.is_confirmed(), "{invalid}: {failure}");
        assert!(!root.path().join("oci-runtime").exists(), "{invalid}");
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn relay_stop_closes_active_and_stalled_streams_without_detached_tasks() {
    use std::os::fd::AsRawFd;
    let root = terminal::scratch();
    let directory = std::fs::File::open(root.path()).unwrap();
    let path = PathBuf::from(format!("/proc/self/fd/{}/rfb.sock", directory.as_raw_fd()));
    let unix = tokio::net::UnixListener::bind(&path).unwrap();
    let mut relay = RfbRelay::start(path).await.unwrap();
    let addr = relay.addr;
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut server, _) = unix.accept().await.unwrap();
    client.write_all(b"RFB").await.unwrap();
    let mut bytes = [0; 3];
    server.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"RFB");
    relay.stop().await.unwrap();
    let mut byte = [0];
    assert_eq!(server.read(&mut byte).await.unwrap(), 0);
    assert_eq!(client.read(&mut byte).await.unwrap(), 0);
    closed(addr).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux Docker, independently built GUI app and verified pinned display helper"]
async fn actual_gui_helper_app_probe_original_k_and_owned_cleanup() {
    let (spec, descriptor, k, d) = fixture(&image());
    let root = terminal::scratch();
    let context = terminal::context(root.path(), "");
    let owner = terminal::owner(&spec);
    let mut config = terminal::config();
    config.startup_timeout = Duration::from_secs(30);
    let scope = config.scope.clone();
    let key = config.verification_key.clone();
    let origin = config.origin.clone();
    let (_alive, rx) = watch::channel(true);
    let setup = Instant::now();
    let controls = std::env::var("ATO_NATIVE_PIXEL_TEST_CONTROLS").as_deref() == Ok("1");
    config.controls = PixelGatewayControls {
        text_paste: controls,
        framebuffer_resize: controls,
    };
    let helper = if controls {
        let path = std::env::var_os("ATO_NATIVE_PIXEL_HELPER_ARCHIVE").unwrap();
        PixelHelper::from_archive_with_controls(
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
            limits(),
            config.controls,
        )
        .unwrap()
    } else {
        helper()
    };
    let archive = app_archive();
    let materialization_input_ms = setup.elapsed().as_millis();
    let start = Instant::now();
    let run = start_pixel(
        &spec,
        &d.derivation_ref().unwrap(),
        &descriptor,
        &context,
        &owner,
        config,
        helper,
        Some(archive),
        rx,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed().as_millis();
    assert_eq!(run.controls().text_paste, controls);
    assert_eq!(run.controls().framebuffer_resize, controls);
    let surface = run.surface_addr();
    let probe = run.observation_addr();
    let actual = get(probe).await;
    assert_eq!(actual, expected());
    assert_eq!(get(probe).await, actual);
    let evidence = run.execution_evidence().clone();
    assert_eq!(evidence.principal.as_ref().unwrap().uids, [65532; 4]);
    assert_eq!(evidence.services.len(), 1);
    assert_eq!(evidence.services[0].name, "native-display");
    let helper_cmd = std::process::Command::new("docker")
        .args([
            "inspect",
            &evidence.services[0].container_id,
            "--format",
            "{{json .Config.Cmd}}",
        ])
        .output()
        .unwrap();
    assert!(helper_cmd.status.success());
    assert_eq!(
        serde_json::from_slice::<Vec<String>>(&helper_cmd.stdout).unwrap(),
        if controls {
            vec!["640", "480", "--controls-v1", "1", "1"]
        } else {
            vec!["640", "480"]
        }
    );
    let app_mounts = std::process::Command::new("docker")
        .args([
            "inspect",
            run.container_id(),
            "--format",
            "{{json .Mounts}}",
        ])
        .output()
        .unwrap();
    assert!(app_mounts.status.success());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&app_mounts.stdout)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|m| !m["Destination"]
                .as_str()
                .unwrap()
                .starts_with("/run/ato-native-rfb"))
    );
    assert!(evidence.services[0].principal.as_ref().unwrap().uids[0] > 0);
    let helper_mounts = std::process::Command::new("docker")
        .args([
            "inspect",
            &evidence.services[0].container_id,
            "--format",
            "{{json .Mounts}}",
        ])
        .output()
        .unwrap();
    assert!(helper_mounts.status.success());
    let mounts: serde_json::Value = serde_json::from_slice(&helper_mounts.stdout).unwrap();
    assert!(
        mounts
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["Source"].as_str() != Some(context.workspace_root().to_str().unwrap()))
    );
    assert!(
        mounts
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["Destination"] == "/run/ato-unused-workspace"
                && m["RW"] == false
                && m["Source"].as_str().unwrap().ends_with("/display/empty"))
    );
    let isolated = std::process::Command::new("docker").args(["inspect",run.container_id(),"--format","{{.HostConfig.ReadonlyRootfs}}|{{json .HostConfig.CapAdd}}|{{json .HostConfig.PortBindings}}|{{range .NetworkSettings.Networks}}{{.NetworkID}}{{end}}"]).output().unwrap();
    assert!(isolated.status.success());
    let isolated = String::from_utf8(isolated.stdout).unwrap();
    assert!(isolated.starts_with("true|null|{}|"), "{isolated}");
    let network = isolated.trim().rsplit('|').next().unwrap();
    let internal = std::process::Command::new("docker")
        .args(["network", "inspect", network, "--format", "{{.Internal}}"])
        .output()
        .unwrap();
    assert_eq!(internal.stdout, b"true\n");
    // Frozen GET cannot replay the first key. Observe the private owned server
    // separately, then send the next key exactly once and observe its red frame.
    use std::os::fd::AsRawFd;
    let directory = std::fs::File::open(root.path().join("oci-runtime/display/rfb")).unwrap();
    let socket = PathBuf::from(format!(
        "/proc/self/fd/{}/display.sock",
        directory.as_raw_fd()
    ));
    let mut inspection = RfbRelay::start(socket).await.unwrap();
    let frame = netd::rfb_probe::capture_private_rfb_frame(
        inspection.addr,
        640,
        480,
        &[],
        640 * 480 * 4,
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(pixel_probe_result(&frame).unwrap(), actual);
    let changed = netd::rfb_probe::capture_private_rfb_frame_with_settle(
        inspection.addr,
        640,
        480,
        &[103],
        640 * 480 * 4,
        Duration::from_millis(200),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(
        pixel_probe_result(&changed).unwrap(),
        pixel_probe_result(&[0x66, 0x33, 0x99, 0].repeat(640 * 480)).unwrap()
    );
    assert_eq!(get(probe).await, actual);
    inspection.stop().await.unwrap();
    let k_ref = k.contract_ref().unwrap();
    let d_ref = d.derivation_ref().unwrap();
    let attempt = AttemptSpec {
        contract: &k,
        contract_ref: &k_ref,
        derivation: &d,
        derivation_ref: &d_ref,
        shape: CandidateShape::Process,
        input_refs: BTreeMap::new(),
        instance_snapshot_ref: None,
        scenarios: Vec::new(),
    };
    let execution: VerificationExecutionEvidence = serde_json::from_value(
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
            execution.clone(),
            &receipt_context,
        )
        .receipt
        .fully_satisfied
    };
    assert!(verify("app.probe", &actual));
    assert!(!verify("app.native", &actual));
    assert!(!verify("app.probe", b"surface ready"));
    assert_eq!(k.contract_ref().unwrap(), k_ref);
    // Authenticated reconnect runs real subsequent input against this display;
    // frozen GET remains the first operation. Wrong-Origin cannot connect.
    let keys = SurfaceAssertionKeyring::new(BTreeMap::from([(
        "ato.runner-surface@1".into(),
        key.clone(),
    )]))
    .unwrap();
    let execution = SurfaceExecutionScope {
        run_id: owner.run_id.clone(),
        lease_id: owner.lease_id.clone(),
        runner_id: owner.runner_id.clone(),
    };
    let assertion = keys
        .issue_readiness_assertion_for_execution(&scope, Some(execution))
        .unwrap();
    let url = format!("http://{surface}/__ato/surface");
    let frame = netd::rfb_probe::wait_for_authenticated_gateway_frame(
        netd::rfb_probe::PixelGatewayProbeRequest::new(&url, &origin, assertion.as_str()),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!((frame.width, frame.height), (640, 480));
    assert_eq!(get(probe).await, actual);
    let assertion = keys
        .issue_readiness_assertion_for_execution(&scope, None)
        .unwrap();
    assert!(
        netd::rfb_probe::wait_for_authenticated_gateway_frame(
            netd::rfb_probe::PixelGatewayProbeRequest::new(&url, &origin, assertion.as_str()),
            Duration::from_secs(1)
        )
        .await
        .is_err()
    );
    if controls {
        controls_acceptance::exercise(
            surface,
            root.path(),
            &scope,
            &key,
            &origin,
            &SurfaceExecutionScope {
                run_id: owner.run_id.clone(),
                lease_id: owner.lease_id.clone(),
                runner_id: owner.runner_id.clone(),
            },
        )
        .await;
        assert_eq!(
            get(probe).await,
            actual,
            "controls never recapture K or frozen probe bytes"
        );
        assert_eq!(k.contract_ref().unwrap(), k_ref);
        assert_eq!(d.derivation_ref().unwrap(), d_ref);
    }
    let completion = run.stop().await;
    assert!(completion.stop.is_confirmed(), "{:?}", completion.stop);
    closed(surface).await;
    closed(probe).await;
    assert_no_resources(&owner);
    assert!(
        !root
            .path()
            .join("oci-runtime/display/auth/Xauthority")
            .exists()
    );
    eprintln!(
        "GUI_EVIDENCE {}",
        json!({"contract_ref":k_ref,"derivation_ref":d_ref,"app_image":image(),"helper_image":evidence.services[0].image,"width":640,"height":480,"settle_ms":50,"body":String::from_utf8(actual).unwrap(),"warm_readiness_ms":elapsed,"verified_input_ms":materialization_input_ms,"fully_satisfied":true,"surface_controls":controls,"cleanup_confirmed":true})
    );
    eprintln!(
        "GUI warm readiness_ms={elapsed}, verified image input_ms={materialization_input_ms}, original K satisfied, two owned containers cleaned"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux Docker and explicit GUI/helper fixtures"]
async fn actual_gui_owner_eof_helper_exit_idle_and_startup_timeout_reclaim_both_containers() {
    for cause in [
        "owner_eof",
        "helper_exit",
        "application_exit",
        "idle_timeout",
        "maximum_lifetime",
        "startup_timeout",
    ] {
        let (mut spec, descriptor, _, d) = fixture(&image());
        let root = terminal::scratch();
        let context = terminal::context(root.path(), "");
        let owner = terminal::owner(&spec);
        let mut config = terminal::config();
        config.startup_timeout = Duration::from_secs(30);
        match cause {
            "idle_timeout" => config.idle_timeout = Duration::from_secs(1),
            "maximum_lifetime" => config.maximum_lifetime = Duration::from_secs(12),
            "application_exit" => {
                let LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
                    unreachable!()
                };
                oci.argv = Some(vec!["1500".into()]);
            }
            "startup_timeout" => {
                config.startup_timeout = Duration::from_secs(1);
                let LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
                    unreachable!()
                };
                oci.entrypoint = Some("/bin/sleep".into());
                oci.argv = Some(vec!["30".into()]);
            }
            _ => {}
        }
        let (alive, rx) = watch::channel(true);
        let result = start_pixel(
            &spec,
            &d.derivation_ref().unwrap(),
            &descriptor,
            &context,
            &owner,
            config,
            helper(),
            Some(app_archive()),
            rx,
        )
        .await;
        if cause == "startup_timeout" {
            let failure = result.err().unwrap();
            assert!(failure.stop.is_confirmed(), "{failure}");
            assert_no_resources(&owner);
            continue;
        }
        let run = result.unwrap();
        let surface = run.surface_addr();
        let probe = run.observation_addr();
        if cause == "owner_eof" {
            drop(alive);
        } else if cause == "helper_exit" {
            assert!(
                std::process::Command::new("docker")
                    .args(["kill", &run.execution_evidence().services[0].container_id])
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        let completion = tokio::time::timeout(Duration::from_secs(15), run.wait())
            .await
            .unwrap();
        assert!(
            completion.stop.is_confirmed(),
            "{cause}: {:?}",
            completion.stop
        );
        closed(surface).await;
        closed(probe).await;
        assert_no_resources(&owner);
        assert!(
            !root
                .path()
                .join("oci-runtime/display/auth/Xauthority")
                .exists()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires independently built immutable helper/app archives"]
async fn actual_offline_archive_mismatch_closure_and_partial_helper_start_fail_closed() {
    use base64::Engine;
    for invalid in ["image", "platform", "closure"] {
        let mut archive = app_archive();
        let (spec, descriptor, _, d) = fixture(&archive.image);
        let root = terminal::scratch();
        let context = terminal::context(root.path(), "");
        let owner = terminal::owner(&spec);
        match invalid {
            "image" => archive.image = format!("ato.local/native-app@sha256:{}", "12".repeat(32)),
            "platform" => archive.platform = "linux/arm64".into(),
            "closure" => {
                let mut bytes = base64::engine::general_purpose::STANDARD
                    .decode(&archive.bytes)
                    .unwrap();
                let position = {
                    let mut tar = tar::Archive::new(std::io::Cursor::new(&bytes));
                    tar.entries()
                        .unwrap()
                        .map(Result::unwrap)
                        .find(|e| e.size() > 65536)
                        .unwrap()
                        .raw_file_position() as usize
                };
                bytes[position] ^= 1;
                archive.bytes = base64::engine::general_purpose::STANDARD.encode(bytes);
            }
            _ => unreachable!(),
        }
        let (_alive, rx) = watch::channel(true);
        let failure = start_pixel(
            &spec,
            &d.derivation_ref().unwrap(),
            &descriptor,
            &context,
            &owner,
            terminal::config(),
            helper(),
            Some(archive),
            rx,
        )
        .await
        .err()
        .unwrap();
        assert!(failure.stop.is_confirmed(), "{invalid}: {failure}");
        assert!(!root.path().join("oci-runtime").exists());
        assert_no_resources(&owner);
    }
    // A valid helper is actually running before the application's impossible
    // entrypoint fails. Its labels, group network, cookie and relay all stop.
    let (mut spec, descriptor, _, d) = fixture(&image());
    let LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
        unreachable!()
    };
    oci.entrypoint = Some("/no-such-application-executable".into());
    let root = terminal::scratch();
    let context = terminal::context(root.path(), "");
    let owner = terminal::owner(&spec);
    let (_alive, rx) = watch::channel(true);
    let failure = start_pixel(
        &spec,
        &d.derivation_ref().unwrap(),
        &descriptor,
        &context,
        &owner,
        terminal::config(),
        helper(),
        Some(app_archive()),
        rx,
    )
    .await
    .err()
    .unwrap();
    assert!(failure.stop.is_confirmed(), "{failure}");
    assert!(
        format!("{failure}").contains("no-such-application-executable"),
        "{failure}"
    );
    assert!(root.path().join("oci-runtime/display-helper").exists());
    assert_no_resources(&owner);
    assert!(!root.path().join("oci-runtime/display").exists());
}

#[test]
fn recovery_never_follows_a_credential_parent_symlink_or_preserves_a_cookie() {
    let root = terminal::scratch();
    let lease = root.path().join("lease");
    std::fs::create_dir(&lease).unwrap();
    let dirs = DisplayDirectories::create(&lease.join("oci-runtime")).unwrap();
    clear_recovered_credentials(&lease).unwrap();
    assert!(!dirs.path("auth/Xauthority").exists());
    drop(dirs);
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("Xauthority"), b"must-survive").unwrap();
    std::fs::create_dir_all(lease.join("oci-runtime/display")).unwrap();
    std::os::unix::fs::symlink(&outside, lease.join("oci-runtime/display/auth")).unwrap();
    assert!(clear_recovered_credentials(&lease).is_err());
    assert_eq!(
        std::fs::read(outside.join("Xauthority")).unwrap(),
        b"must-survive"
    );
}

#[test]
#[ignore = "private subprocess harness, invoked only by the SIGKILL recovery test"]
fn owned_gui_crash_child() {
    let Some(root) = std::env::var_os("ATO_NATIVE_GUI_CRASH_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let v: serde_json::Value =
        serde_json::from_str(&std::env::var("ATO_NATIVE_GUI_CRASH_OWNER").unwrap()).unwrap();
    let field = |name: &str| v[name].as_str().unwrap().to_owned();
    let owner = OciOwner {
        runner_id: field("runner_id"),
        slot_id: field("slot_id"),
        run_id: field("run_id"),
        lease_id: field("lease_id"),
        incarnation: field("incarnation"),
    };
    let (spec, descriptor, _, d) = fixture(&image());
    let context = terminal::context(&root, "");
    let mut config = terminal::config();
    config.startup_timeout = Duration::from_secs(30);
    config.maximum_lifetime = Duration::from_secs(120);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let (_alive, rx) = watch::channel(true);
        let run = start_pixel(
            &spec,
            &d.derivation_ref().unwrap(),
            &descriptor,
            &context,
            &owner,
            config,
            helper(),
            Some(app_archive()),
            rx,
        )
        .await
        .unwrap();
        std::fs::write(root.join("physical-ready"), run.container_id()).unwrap();
        let _run = run;
        tokio::time::sleep(Duration::from_secs(120)).await;
    });
}

#[test]
#[ignore = "requires Linux Docker and explicit GUI/helper fixtures; kills only its own child"]
fn actual_runner_sigkill_recovers_both_gui_containers_and_does_not_preserve_cookie() {
    use crate::runtime_launch::recovery::{
        LeaseLiveness, LeaseRecoveryReport, RecoveryReporter, RunJournal, RunJournalEntry,
        recover_slot, settle_lease,
    };
    let (spec, _, _, _) = fixture(&image());
    let mut owner = terminal::owner(&spec);
    owner.slot_id = format!("gui-crash-{:08x}", rand::random::<u32>());
    let root = terminal::scratch();
    let leases = root.path().join("leases");
    let lease = leases.join(&owner.lease_id);
    std::fs::create_dir_all(&lease).unwrap();
    let journal = RunJournal::new(root.path(), &owner.runner_id, &owner.slot_id).unwrap();
    journal.record(&RunJournalEntry::new(&owner)).unwrap();
    let owner_wire = json!({"runner_id":owner.runner_id,"slot_id":owner.slot_id,"lease_id":owner.lease_id,"run_id":owner.run_id,"incarnation":owner.incarnation});
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "runtime_launch::native::pixel::tests::owned_gui_crash_child",
            "--nocapture",
        ])
        .env("ATO_NATIVE_GUI_CRASH_ROOT", &lease)
        .env("ATO_NATIVE_GUI_CRASH_OWNER", owner_wire.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    struct Guard {
        child: std::process::Child,
        owner: OciOwner,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = settle_lease(
                &self.owner,
                StopBudget {
                    graceful: Duration::from_millis(500),
                    force: Duration::from_secs(1),
                },
            );
        }
    }
    let mut guard = Guard {
        child,
        owner: owner.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(45);
    while !lease.join("physical-ready").exists() {
        assert!(
            guard.child.try_wait().unwrap().is_none(),
            "private GUI child failed before ready"
        );
        assert!(Instant::now() < deadline, "GUI child readiness timeout");
        std::thread::sleep(Duration::from_millis(50));
    }
    let scanner =
        ato_adapter_oci::OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id).unwrap();
    let resources = scanner.scan().unwrap();
    assert_eq!(resources.containers.len(), 2);
    assert_eq!(resources.networks.len(), 1);
    assert!(lease.join("oci-runtime/display/auth/Xauthority").is_file());
    guard.child.kill().unwrap();
    guard.child.wait().unwrap();
    struct Reporter;
    impl RecoveryReporter for Reporter {
        fn report_recovery(&self, report: &LeaseRecoveryReport) -> Result<()> {
            ensure!(report.stop.is_confirmed(), "unconfirmed GUI recovery");
            Ok(())
        }
        fn lease_liveness(&self, _: &str) -> Result<LeaseLiveness> {
            Ok(LeaseLiveness::Terminal)
        }
    }
    let recovered = recover_slot(
        &journal,
        Some(&scanner),
        &Reporter,
        StopBudget {
            graceful: Duration::from_millis(500),
            force: Duration::from_secs(1),
        },
        &leases,
    )
    .unwrap();
    assert!(recovered.clean);
    assert_eq!(recovered.reports.len(), 1);
    assert!(journal.load().unwrap().is_empty());
    assert_no_resources(&owner);
    let preserved = root.path().join("leases.resolved").join(&owner.lease_id);
    assert!(preserved.join("w/main.py").is_file());
    assert!(
        !preserved
            .join("oci-runtime/display/auth/Xauthority")
            .exists()
    );
}

#[test]
fn helper_argv_preserves_legacy_and_only_encodes_fixed_physical_controls() {
    let mut helper = stub_helper();
    assert_eq!(helper.launch_argv(640, 480), vec!["640", "480"]);
    helper.controls = PixelGatewayControls {
        text_paste: true,
        framebuffer_resize: false,
    };
    assert_eq!(
        helper.launch_argv(640, 480),
        vec!["640", "480", "--controls-v1", "1", "0"]
    );
    helper.controls = PixelGatewayControls {
        text_paste: false,
        framebuffer_resize: true,
    };
    assert_eq!(
        helper.launch_argv(640, 480),
        vec!["640", "480", "--controls-v1", "0", "1"]
    );
}
