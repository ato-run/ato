//! Opt-in real two-container proof; pinned public image, local canaries only.
use super::*;
use crate::data_plane::{
    GrantedOutputs, LeaseData, ModelCache, ModelSetEntry, ModelSetManifest, save_outputs,
};
use crate::launch::{
    oci::{OciStartFailure, launch_service_group},
    process_executor::LoopbackReadinessProbe,
    resolved::ResolvedEndpoint,
};
use ato_adapter_oci::{OciOwner, OwnedResourceScanner, StopBudget};
use ato_formation::{
    authoring::{BOUND_CONTRACT_SCHEMA, BoundContract, BoundRequirement, HTTP_CONTRACT_VERIFIER},
    verify::{RuntimeHttpObservation, RuntimeObservation, verify_runtime},
};
use ato_ipc::runtime_launch::{OciUserV1, PublicEnvV1};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::MetadataExt,
    time::{Duration, Instant},
};

const SCRIPT: &str = r#"
import http.server, os, pathlib, urllib.request
role=os.environ['MODE']
if role=='backend':
    model=pathlib.Path(os.environ['ATO_INPUT_PATH_MODELS'])/'weights/model.bin'
    body=b'model:'+model.read_bytes()
    try: model.write_bytes(b'changed')
    except OSError: pass
    else: raise RuntimeError('model was writable')
else:
    assert not any(k.startswith('ATO_INPUT_') for k in os.environ)
    assert not pathlib.Path('/.ato-data/inputs').exists()
    body=urllib.request.urlopen('http://backend:8080/',timeout=3).read()+b'|sibling:no-input'
(pathlib.Path(os.environ['ATO_OUTPUT_DIR'])/'result.txt').write_bytes(body)
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(body)
    def log_message(self,*args): pass
http.server.HTTPServer(('0.0.0.0',8080),Handler).serve_forever()
"#;
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn budget() -> StopBudget {
    StopBudget {
        graceful: Duration::from_secs(1),
        force: Duration::from_secs(1),
    }
}
fn http(port: &str, body: &[u8]) -> BoundRequirement {
    BoundRequirement {
        id: format!("{port}-proof"),
        verifier: HTTP_CONTRACT_VERIFIER.into(),
        port: Some(port.into()),
        method: Some("GET".into()),
        path: Some("/".into()),
        status: Some(200),
        body_digest: Some(digest(body)),
        input: None,
        digest: None,
    }
}

#[test]
#[ignore = "requires native Linux Docker and ATO_OCI_DATA_TEST_IMAGE pinned Python"]
fn two_components_scope_model_inputs_save_outputs_and_require_all_k() {
    let boundary = if std::env::var("ATO_OCI_DATA_TEST_HOST_BOUNDARY").as_deref() == Ok("1") {
        let boundary = crate::launch::host_boundary::HostBoundary {
            uid: std::env::var("ATO_OCI_DATA_WORKLOAD_UID")
                .unwrap()
                .parse()
                .unwrap(),
            gid: std::env::var("ATO_OCI_DATA_WORKLOAD_GID")
                .unwrap()
                .parse()
                .unwrap(),
        };
        crate::launch::host_boundary::enable(boundary).unwrap();
        Some(boundary)
    } else {
        None
    };
    let image = std::env::var("ATO_OCI_DATA_TEST_IMAGE").unwrap();
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let model_bytes = b"verified model weights\n";
    let entry = ModelSetEntry {
        path: "weights/model.bin".into(),
        digest: digest(model_bytes),
        bytes: model_bytes.len() as u64,
    };
    let cache = ModelCache::open(root.path()).unwrap();
    cache
        .ensure(
            &entry,
            |start, end| {
                Ok(Box::new(std::io::Cursor::new(
                    model_bytes[start as usize..=end as usize].to_vec(),
                )))
            },
            || Ok(()),
        )
        .unwrap();
    let models = root.path().join("inputs/models");
    cache
        .materialize(
            &ModelSetManifest {
                schema: "ato.model-set/1".into(),
                objects: vec![entry.clone()],
            },
            &models,
        )
        .unwrap();
    let outputs = root.path().join("outputs");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let context = ResolvedRuntimeLaunchContext::new(
        workspace,
        "",
        BTreeMap::new(),
        vec![],
        vec![],
        vec![ResolvedEndpoint {
            name: "app.http".into(),
            guest_port: Some(8080),
            host_port: port,
        }],
    )
    .unwrap()
    .with_read_only_inputs(vec![ResolvedReadOnlyInput {
        input_id: Some("models".into()),
        env_name: input_env_name("models"),
        path: models.clone(),
    }])
    .with_output_dir(Some(outputs.clone()))
    .with_data_grant_present(true);
    let mut spec = RuntimeLaunchSpecV2::parse(include_str!(
        "../../../../../ipc/tests/fixtures/runtime-launch-spec-v2/service-group.json"
    ))
    .unwrap();
    spec.context.run_id = format!("component-data-{}", std::process::id());
    spec.state_attachments.clear();
    let metadata = std::fs::metadata(context.workspace_root()).unwrap();
    let user = OciUserV1 {
        uid: boundary.map_or(metadata.uid(), |b| b.uid),
        gid: boundary.map_or(metadata.gid(), |b| b.gid),
    };
    let ato_ipc::runtime_launch_v2::LaunchRealizationV2::OciServiceGroup(group) =
        &mut spec.realization;
    for (index, service) in group.services.iter_mut().enumerate() {
        service.image_reference = image.clone();
        service.image_digest_ref = image.rsplit_once('@').unwrap().1.into();
        service.entrypoint = Some("/usr/local/bin/python3".into());
        service.argv = vec!["-c".into(), SCRIPT.into()];
        service.user = Some(user);
        service.state_keys.clear();
        service.secret_grants.clear();
        service.input_ids = Some(if index == 0 {
            vec!["models".into()]
        } else {
            vec![]
        });
        service.public_env = vec![PublicEnvV1 {
            name: "MODE".into(),
            value: service.name.clone(),
        }];
        service.endpoints[0].guest_port = 8080;
    }
    spec.validate().unwrap();
    let owner = OciOwner {
        runner_id: "component-data-fixture".into(),
        slot_id: spec.context.run_id.clone(),
        lease_id: "fixture-lease".into(),
        run_id: spec.context.run_id.clone(),
        incarnation: "fixture".into(),
    };
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let probe = LoopbackReadinessProbe::new(client.clone());
    let started = Instant::now();
    let group = launch_service_group(&spec, &context, &probe, &owner, vec![], budget()).unwrap();
    let backend = group
        .services()
        .find(|(name, _)| *name == "backend")
        .unwrap()
        .1;
    let backend_body = client
        .get(format!("http://{}:8080/", backend.container_address()))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .unwrap();
    let app_body = client
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .unwrap();
    let expected_backend = [b"model:".as_slice(), model_bytes].concat();
    let expected_app = [expected_backend.as_slice(), b"|sibling:no-input"].concat();
    assert_eq!(backend_body.as_ref(), expected_backend);
    assert_eq!(app_body.as_ref(), expected_app);
    let contract = BoundContract {
        schema: BOUND_CONTRACT_SCHEMA.into(),
        requirements: vec![
            http("backend.http", &expected_backend),
            http("app.http", &expected_app),
        ],
    };
    let mut observation = RuntimeObservation {
        input_refs: BTreeMap::new(),
        instance_snapshot_ref: None,
        http: vec![RuntimeHttpObservation::from_response(
            "app.http", "GET", "/", 200, &app_body,
        )],
    };
    assert!(!verify_runtime(&contract, &observation).fully_satisfied());
    observation.http.push(RuntimeHttpObservation::from_response(
        "backend.http",
        "GET",
        "/",
        503,
        &backend_body,
    ));
    assert!(!verify_runtime(&contract, &observation).fully_satisfied());
    observation.http[1] =
        RuntimeHttpObservation::from_response("backend.http", "GET", "/", 200, &backend_body);
    assert!(verify_runtime(&contract, &observation).fully_satisfied());
    assert!(
        group
            .stop_gracefully(budget())
            .overall()
            .unwrap()
            .is_confirmed()
    );
    assert_eq!(
        std::fs::read(outputs.join("backend/result.txt")).unwrap(),
        expected_backend
    );
    assert_eq!(
        std::fs::read(outputs.join("web/result.txt")).unwrap(),
        expected_app
    );
    assert_eq!(
        std::fs::read(models.join("weights/model.bin")).unwrap(),
        model_bytes
    );
    cache.verify_cached(&entry).unwrap();
    save_component_outputs(&client, &outputs, &expected_backend, &expected_app);
    if let Some(boundary) = boundary {
        for user in [
            None,
            Some(OciUserV1 {
                uid: boundary.uid + 1,
                gid: boundary.gid,
            }),
        ] {
            let mut wrong = spec.clone();
            let ato_ipc::runtime_launch_v2::LaunchRealizationV2::OciServiceGroup(group) =
                &mut wrong.realization;
            group.services[0].user = user;
            let service = &group.services[0];
            let error = crate::launch::oci::service_oci_spec(
                &wrong,
                service,
                &context,
                &owner,
                &BTreeMap::new(),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("oci_user_state_owner_mismatch"),
                "{error:#}"
            );
        }
    }

    spec.context.run_id.push_str("-failed");
    let ato_ipc::runtime_launch_v2::LaunchRealizationV2::OciServiceGroup(group) =
        &mut spec.realization;
    group.services[1].argv = vec!["-c".into(), "raise SystemExit(42)".into()];
    group.services[1].readiness = ato_ipc::runtime_launch_v2::ServiceReadinessV2::Http {
        endpoint_name: "app.http".into(),
        path: "/".into(),
        timeout_ms: 2_000,
    };
    let owner = OciOwner {
        slot_id: spec.context.run_id.clone(),
        run_id: spec.context.run_id.clone(),
        ..owner
    };
    let error = match launch_service_group(&spec, &context, &probe, &owner, vec![], budget()) {
        Ok(group) => {
            let _ = group.stop_gracefully(budget());
            panic!("failed sibling accepted")
        }
        Err(error) => error,
    };
    assert!(
        error.downcast_ref::<OciStartFailure>().unwrap().confirmed(),
        "{error:#}"
    );
    let resources = OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)
        .unwrap()
        .scan()
        .unwrap();
    assert!(resources.containers.is_empty() && resources.networks.is_empty());
    eprintln!(
        "component OCI data proof elapsed_ms={} host_boundary={}",
        started.elapsed().as_millis(),
        boundary.is_some()
    );
}

fn save_component_outputs(client: &Client, outputs: &Path, backend: &[u8], app: &[u8]) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut generated = vec![];
        for step in 0..9 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            drop(reader);
            let response = match step % 3 {
                0 => {
                    generated.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap());
                    r#"{"output_id":"out_fixture","part_bytes":65536}"#
                }
                1 => r#"{"part_number":1,"etag":"fixture"}"#,
                _ => r#"{"asset_id":"ast_fixture"}"#,
            };
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).unwrap();
        }
        generated
    });
    let data = LeaseData {
        client,
        base: &base,
        token: "fixture-canary",
        lease_id: "fixture-lease",
    };
    let saved = save_outputs(
        &data,
        outputs,
        &GrantedOutputs {
            max_count: 2,
            max_bytes_each: 1024,
        },
    );
    assert_eq!(saved.len(), 2);
    assert!(saved.iter().all(|s| s.save_status == "saved"));
    assert_eq!(
        saved
            .iter()
            .map(|s| s.output_key.as_str())
            .collect::<Vec<_>>(),
        ["backend/result.txt", "web/result.txt"]
    );
    assert_eq!(
        save_outputs(
            &data,
            outputs,
            &GrantedOutputs {
                max_count: 1,
                max_bytes_each: 1024
            }
        )
        .len(),
        1
    );
    let generated = server.join().unwrap();
    assert_eq!(generated[0]["checksum_sha256"], digest(backend));
    assert_eq!(generated[1]["checksum_sha256"], digest(app));
}
