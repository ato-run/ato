//! Engine gRPC smoke tests for RunPlan and legacy manifests (AdepJson, TomlContent).
//!
//! These tests spin up the Engine gRPC service over a Unix domain socket with
//! mocked runtimes (no real containers) and assert that DeployCapsule accepts
//! both RunPlan and legacy TOML manifests.
//!
//! TODO: Fix tonic 0.12 + hyper 1.x connector compatibility for UnixStream
//! https://github.com/hyperium/tonic/issues/1483

#![cfg(unix)]
#![cfg(feature = "disabled_until_tonic_connector_fixed")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use capsuled::capsule_manager::CapsuleManager;
use capsuled::grpc_server::EngineService;
use capsuled::hardware::create_gpu_detector;
use capsuled::proto::onescluster::common::v1::run_plan::Runtime as RunPlanRuntime;
use capsuled::proto::onescluster::common::v1::{DockerRuntime, Mount, Port, RunPlan};
use capsuled::proto::onescluster::engine::v1::deploy_request::Manifest as DeployManifest;
use capsuled::proto::onescluster::engine::v1::engine_client::EngineClient;
use capsuled::proto::onescluster::engine::v1::engine_server::EngineServer;
use capsuled::proto::onescluster::engine::v1::DeployRequest;
use capsuled::proto::onescluster::engine::v1::GetSystemStatusRequest;
use capsuled::runtime::{ContainerRuntime, RuntimeConfig, RuntimeKind};
use capsuled::security::audit::AuditLogger;
use capsuled::wasm_host::AdepLogicHost;
use capsuled::{
    artifact::manager::{ArtifactConfig, ArtifactManager},
    network::{service_registry::ServiceRegistry, tailscale::TailscaleManager},
};
use capsule_core::mapper::capsule_v1_toml_to_proto_run_plan;
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::{sleep, Duration, Instant};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tower::service_fn;

struct Harness {
    _tmp: TempDir,
    client: EngineClient<Channel>,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");

        // Prepare mock runtime binary
        let mock_runtime_path = tmp.path().join("mock_runtime.sh");
        std::fs::write(
            &mock_runtime_path,
            r#"#!/bin/sh
case "$1" in
    state)
        echo '{"pid": 1234, "status": "running"}'
        ;;
    create|start|delete|kill)
        exit 0
        ;;
    *)
        exit 0
        ;;
esac
"#,
        )
        .expect("write mock runtime");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mock_runtime_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod mock runtime");
        }

        let runtime_config = RuntimeConfig {
            kind: RuntimeKind::Mock,
            binary_path: mock_runtime_path.clone(),
            bundle_root: tmp.path().join("bundles"),
            state_root: tmp.path().join("state"),
            log_dir: tmp.path().join("logs"),
            hook_retry_attempts: 1,
        };

        let audit_log = tmp.path().join("audit.log");
        let key_path = tmp.path().join("node_key.pem");
        let audit_logger = Arc::new(
            AuditLogger::new(audit_log, key_path, "test-node".to_string()).expect("audit logger"),
        );

        let gpu_detector = create_gpu_detector();
        let service_registry = Arc::new(ServiceRegistry::new(None));

        let registry_path = tmp.path().join("registry.json");
        std::fs::write(&registry_path, "{\"runtimes\":{}}").expect("write empty registry");
        let artifact_manager = Arc::new(
            ArtifactManager::new(ArtifactConfig {
                registry_url: format!("file://{}", registry_path.display()),
                cache_path: tmp.path().join("artifact_cache"),
                cas_root: None,
            })
            .await
            .expect("artifact manager"),
        );

        let wasm_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test-data")
            .join("adep_logic.wasm");
        if !wasm_path.exists() {
            panic!("missing adep_logic.wasm at {}", wasm_path.display());
        }
        let wasm_host =
            Arc::new(AdepLogicHost::from_file(wasm_path.to_str().unwrap()).expect("wasm host"));

        let tailscale_manager = Arc::new(TailscaleManager::start(None, None, None));
        let container_runtime = Arc::new(ContainerRuntime::new(
            runtime_config.clone(),
            Some(artifact_manager.clone()),
            None,
            None,
        ));

        let verifier = Arc::new(capsuled::security::verifier::ManifestVerifier::new(
            None, false,
        ));

        let capsule_manager = Arc::new(CapsuleManager::new(
            audit_logger,
            vec![],
            gpu_detector.clone(),
            Some(service_registry.clone()),
            None,
            None,
            None,
            Some(artifact_manager.clone()),
            None,
            None,
            verifier,
            Some(runtime_config),
            None,
            None,
        ));

        let engine_service = EngineService::new(
            capsule_manager,
            wasm_host,
            "test-backend".to_string(),
            tailscale_manager,
            service_registry,
            container_runtime,
            vec![],
            tmp.path().join("models"),
            gpu_detector,
            artifact_manager,
        );

        let socket_path = tmp.path().join("engine-grpc.sock");
        let uds = UnixListener::bind(&socket_path).expect("bind uds");
        let incoming = UnixListenerStream::new(uds);

        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(EngineServer::new(engine_service))
                .serve_with_incoming(incoming)
                .await
                .expect("serve engine");
        });

        let channel = Endpoint::try_from("http://[::]:50051")
            .expect("endpoint")
            .connect_with_connector(service_fn(move |_: Uri| {
                let path = socket_path.clone();
                async move { UnixStream::connect(path).await }
            }))
            .await
            .expect("connect uds");

        let client = EngineClient::new(channel);

        Self {
            _tmp: tmp,
            client,
            server,
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn sample_runplan() -> RunPlan {
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "BAR".to_string());

    RunPlan {
        capsule_id: "capsule-runplan".to_string(),
        name: "demo-capsule".to_string(),
        version: "0.1.0".to_string(),
        runtime: Some(RunPlanRuntime::Docker(DockerRuntime {
            image: "dummy".to_string(),
            digest: "sha256:deadbeef".to_string(),
            command: vec!["echo".to_string(), "hello".to_string()],
            env,
            working_dir: "/app".to_string(),
            user: "root".to_string(),
            ports: vec![Port {
                container_port: 8080,
                host_port: 18080,
                protocol: "tcp".to_string(),
            }],
            mounts: vec![Mount {
                source: "/tmp".to_string(),
                target: "/app".to_string(),
                readonly: false,
            }],
        })),
        cpu_cores: 2,
        memory_bytes: 128 * 1024 * 1024,
        gpu_profile: "none".to_string(),
        egress_allowlist: vec![],
    }
}

#[tokio::test]
async fn deploys_runplan_docker_with_ports_mounts_env() {
    let mut harness = Harness::new().await;

    let req = DeployRequest {
        capsule_id: "capsule-runplan".to_string(),
        manifest: Some(DeployManifest::RunPlan(sample_runplan())),
        oci_image: String::new(),
        digest: String::new(),
        manifest_signature: vec![],
    };

    let response = harness
        .client
        .deploy_capsule(req)
        .await
        .expect("deploy runplan")
        .into_inner();

    assert_eq!(response.capsule_id, "capsule-runplan");
    assert_eq!(response.status, "starting");
}

#[tokio::test]
async fn deploys_legacy_adep_json_manifest() {
    let mut harness = Harness::new().await;

    // Note: "legacy" refers to the JSON transport format, not the manifest schema.
    // The engine expects CapsuleManifestV1 JSON format.
    let adep_json = br#"{
        "schema_version": "1.0",
        "name": "capsule-legacy",
        "version": "0.1.0",
        "type": "app",
        "execution": {
            "runtime": "docker",
            "entrypoint": "alpine:latest"
        }
    }"#;

    let req = DeployRequest {
        capsule_id: "capsule-legacy".to_string(),
        manifest: Some(DeployManifest::AdepJson(adep_json.to_vec())),
        oci_image: String::new(),
        digest: String::new(),
        manifest_signature: vec![],
    };

    let response = harness
        .client
        .deploy_capsule(req)
        .await
        .expect("deploy legacy adep_json")
        .into_inner();

    assert_eq!(response.capsule_id, "capsule-legacy");
    assert_eq!(response.status, "starting");
}

#[tokio::test]
async fn deploys_legacy_toml_content_manifest() {
    let mut harness = Harness::new().await;

    // NOTE: This is a "legacy" request shape (TomlContent), not a legacy manifest schema.
    // The engine expects capsule_v1 TOML for TomlContent.
    let adep_toml = r#"
schema_version = "1.0"
name = "capsule-toml"
version = "0.1.0"
type = "app"

[execution]
runtime = "docker"
entrypoint = "ghcr.io/example/hello:latest"
port = 8080
"#;

    let req = DeployRequest {
        capsule_id: "capsule-toml".to_string(),
        manifest: Some(DeployManifest::TomlContent(adep_toml.to_string())),
        oci_image: String::new(),
        digest: String::new(),
        manifest_signature: vec![],
    };

    let response = harness
        .client
        .deploy_capsule(req)
        .await
        .expect("deploy legacy toml_content")
        .into_inner();

    assert_eq!(response.capsule_id, "capsule-toml");
    assert_eq!(response.status, "starting");
}

#[tokio::test]
async fn deploys_canonical_capsule_v1_toml_via_toml_content() {
    let mut harness = Harness::new().await;

    // Canonical capsule_v1 TOML (schema_version = 1.0)
    let canonical_toml = r#"
schema_version = "1.0"
name = "hello-docker"
version = "0.1.0"
type = "app"

[execution]
runtime = "docker"
entrypoint = "ghcr.io/example/hello:latest"
port = 8080
"#;

    let req = DeployRequest {
        capsule_id: "hello-docker".to_string(),
        manifest: Some(DeployManifest::TomlContent(canonical_toml.to_string())),
        oci_image: String::new(),
        digest: String::new(),
        manifest_signature: vec![],
    };

    let response = harness
        .client
        .deploy_capsule(req)
        .await
        .expect("deploy canonical capsule_v1 toml_content")
        .into_inner();

    assert_eq!(response.capsule_id, "hello-docker");
    assert_eq!(response.status, "starting");
}

#[tokio::test]
async fn smoke_deploys_static_app_and_reports_local_url() {
    let mut harness = Harness::new().await;

    // Static site-style image: container listens on 80.
    // Smoke assertion: Engine should allocate an ephemeral HOST_PORT and expose it via GetSystemStatus.local_url.
    let static_app_toml = r#"
schema_version = "1.0"
name = "static-site"
version = "0.1.0"
type = "app"

[execution]
runtime = "docker"
entrypoint = "ghcr.io/example/static:latest"
port = 80
"#;

    let req = DeployRequest {
        capsule_id: "static-site".to_string(),
        manifest: Some(DeployManifest::TomlContent(static_app_toml.to_string())),
        oci_image: String::new(),
        digest: String::new(),
        manifest_signature: vec![],
    };

    let response = harness
        .client
        .deploy_capsule(req)
        .await
        .expect("deploy static app")
        .into_inner();

    assert_eq!(response.capsule_id, "static-site");
    assert_eq!(response.status, "starting");

    // The actual deploy is async; poll until the service is registered.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_status = None;
    let local_url = loop {
        let status = harness
            .client
            .get_system_status(GetSystemStatusRequest {})
            .await
            .expect("get_system_status")
            .into_inner();

        if let Some(c) = status.capsules.iter().find(|c| c.id == "static-site") {
            last_status = Some((c.status.clone(), c.local_url.clone()));
            if !c.local_url.is_empty() {
                break c.local_url.clone();
            }
        }

        if Instant::now() >= deadline {
            panic!("timed out waiting for local_url; last={:?}", last_status);
        }
        sleep(Duration::from_millis(50)).await;
    };

    assert!(
        local_url.starts_with("http://127.0.0.1:"),
        "local_url={}",
        local_url
    );
    let port_str = local_url.rsplit(':').next().expect("port segment");
    let host_port: u16 = port_str.parse().expect("port parse");
    assert_ne!(host_port, 80, "host_port should not be 80");
    assert!(
        host_port > 1024,
        "expected non-privileged host port, got {}",
        host_port
    );
}

#[tokio::test]
async fn deploys_runplan_from_libadep_proto() {
    let mut harness = Harness::new().await;

    let canonical_toml = include_str!("../test-data/capsule_v1_hello_docker.toml");
    let runplan = capsule_v1_toml_to_proto_run_plan(canonical_toml)
        .expect("libadep should parse canonical capsule_v1 TOML");

    let req = DeployRequest {
        capsule_id: "hello-docker".to_string(),
        manifest: Some(DeployManifest::RunPlan(runplan)),
        oci_image: String::new(),
        digest: String::new(),
        manifest_signature: vec![],
    };

    let response = harness
        .client
        .deploy_capsule(req)
        .await
        .expect("deploy runplan generated by libadep")
        .into_inner();

    assert_eq!(response.capsule_id, "hello-docker");
    assert_eq!(response.status, "starting");
}

#[tokio::test]
async fn deploys_libadep_generated_proto_runplan() {
    let mut harness = Harness::new().await;

    // Load canonical v1 TOML from test-data
    let canonical_toml = include_str!("../test-data/capsule_v1_hello_docker.toml");

    // Convert TOML → proto RunPlan using capsule_core mapper
    let proto_runplan = capsule_core::mapper::capsule_v1_toml_to_proto_run_plan(canonical_toml)
        .expect("libadep should convert canonical TOML to proto RunPlan");

    let req = DeployRequest {
        capsule_id: "libadep-proto-test".to_string(),
        manifest: Some(DeployManifest::RunPlan(proto_runplan)),
        oci_image: String::new(),
        digest: String::new(),
        manifest_signature: vec![],
    };

    let response = harness
        .client
        .deploy_capsule(req)
        .await
        .expect("deploy libadep-generated proto runplan")
        .into_inner();

    assert_eq!(response.capsule_id, "libadep-proto-test");
    assert_eq!(response.status, "starting");
}
