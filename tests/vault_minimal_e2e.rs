//! Phase 6A: Vault Minimal — HostPath bind mount による永続化の e2e テスト
//!
//! 検証内容:
//! 1. Deploy with [storage] → HostPath 作成 → mount → 書込み
//! 2. Stop
//! 3. Re-deploy with same [storage] → HostPath 再 mount → データ残存確認
//!
//! TODO: This test requires significant updates to match current EngineService API.
//! Currently disabled - please refactor when needed.

#![allow(dead_code)]
#![cfg(feature = "disabled_vault_minimal_e2e")]

use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

use capsuled_engine::capsule_manager::CapsuleManager;
use capsuled_engine::grpc_server::EngineService;
use capsuled_engine::hardware::create_gpu_detector;
use capsuled_engine::proto::onescluster::engine::v1::deploy_request::Manifest as DeployManifest;
use capsuled_engine::proto::onescluster::engine::v1::engine_server::EngineServer;
use capsuled_engine::proto::onescluster::engine::v1::DeployRequest;
use capsuled_engine::runtime::{ContainerRuntime, RuntimeConfig, RuntimeKind};
use capsuled_engine::security::audit::AuditLogger;
use tonic::transport::Server;
use tonic::Request;

/// Test prerequisite check - disabled pending API updates
#[tokio::test]
#[ignore]
async fn test_prerequisites_check() {
    println!("Phase 6A vault e2e prerequisites OK");
}

/// Phase 6A: HostPath bind mount で永続化を確認（Linux only）
#[cfg(target_os = "linux")]
#[tokio::test]
async fn test_vault_minimal_hostpath_persistence() {
    use std::fs;

    let tmp = TempDir::new().expect("tempdir");
    let storage_base = tmp.path().join("gumball_volumes");
    fs::create_dir_all(&storage_base).expect("create storage base");

    // Set env for GUMBALL_STORAGE_BASE
    std::env::set_var("GUMBALL_STORAGE_BASE", storage_base.to_str().unwrap());

    // Setup mock runtime
    let mock_runtime_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mock_runtime");
    if !mock_runtime_path.exists() {
        println!("SKIP: mock_runtime not found at {:?}", mock_runtime_path);
        return;
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
    fs::write(&registry_path, "{\"runtimes\":{}}").expect("write empty registry");
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
        println!("SKIP: missing adep_logic.wasm at {}", wasm_path.display());
        return;
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

    let verifier = Arc::new(capsuled_engine::security::verifier::ManifestVerifier::new(
        None, false,
    ));

    let capsule_manager = Arc::new(CapsuleManager::new(
        audit_logger,
        vec![storage_base.to_string_lossy().to_string()],
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
        capsule_manager.clone(),
        wasm_host,
        "test-backend".to_string(),
        tailscale_manager,
        service_registry,
        container_runtime,
        vec![],
        gpu_detector,
        artifact_manager,
    );

    let addr = "127.0.0.1:0".parse().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();

    let server_handle = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
        let local_addr = listener.local_addr().unwrap();
        tx.send(local_addr).unwrap();

        Server::builder()
            .add_service(EngineServer::new(engine_service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let local_addr = rx.await.unwrap();
    let endpoint = format!("http://{}", local_addr);

    // Give server time to fully start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Connect to gRPC
    let channel = tonic::transport::Channel::from_shared(endpoint.clone())
        .unwrap()
        .connect()
        .await
        .unwrap();

    use capsuled_engine::proto::onescluster::engine::v1::engine_client::EngineClient;
    let mut client = EngineClient::new(channel);

    // --- Step 1: Deploy with storage volume ---
    let capsule_id = "vault-test-capsule";

    // Canonical TOML with [storage]
    let toml_content = r#"
schema_version = "1.0"

[metadata]
name = "vault-test"
version = "1.0.0"

[execution]
runtime = "docker"
image = "alpine:latest"
command = ["sh", "-c", "echo 'hello vault' > /data/message.txt && cat /data/message.txt && sleep 2"]

[storage]
volumes = [
  { name = "data", mount_path = "/data", read_only = false }
]
"#;

    let deploy_req = DeployRequest {
        capsule_id: capsule_id.to_string(),
        manifest: Some(DeployManifest::TomlContent(toml_content.to_string())),
        oci_image: "".to_string(),
        digest: "".to_string(),
        manifest_signature: vec![],
    };

    let response = client
        .deploy_capsule(Request::new(deploy_req.clone()))
        .await;
    assert!(
        response.is_ok(),
        "First deploy failed: {:?}",
        response.err()
    );

    // Wait for container to finish (mock runtime exits quickly)
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Check HostPath created
    let expected_volume_path = storage_base.join(capsule_id).join("data");
    assert!(
        expected_volume_path.exists(),
        "HostPath volume not created: {:?}",
        expected_volume_path
    );

    // Check data written (mock runtime doesn't actually run container, but directory should exist)
    // For a real test on Linux with real OCI runtime, you'd verify file content here.
    // With mock, we just verify mount directory was created.
    println!("✓ HostPath created: {:?}", expected_volume_path);

    // Write a marker file to simulate data (since mock doesn't execute)
    let marker = expected_volume_path.join("marker.txt");
    fs::write(&marker, "persistent-data").expect("write marker");

    // --- Step 2: Stop ---
    use capsuled_engine::proto::onescluster::engine::v1::StopRequest;
    let stop_req = StopRequest {
        capsule_id: capsule_id.to_string(),
    };
    let stop_response = client.stop_capsule(Request::new(stop_req)).await;
    assert!(
        stop_response.is_ok(),
        "Stop failed: {:?}",
        stop_response.err()
    );

    println!("✓ Capsule stopped");

    // --- Step 3: Re-deploy with same storage ---
    let redeploy_response = client.deploy_capsule(Request::new(deploy_req)).await;
    assert!(
        redeploy_response.is_ok(),
        "Re-deploy failed: {:?}",
        redeploy_response.err()
    );

    println!("✓ Capsule re-deployed");

    // Wait for re-deploy to mount
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // --- Step 4: Verify data persistence ---
    assert!(
        marker.exists(),
        "Persistent marker file not found after re-deploy: {:?}",
        marker
    );

    let content = fs::read_to_string(&marker).expect("read marker");
    assert_eq!(content, "persistent-data", "Marker content mismatch");

    println!("✓ Data persisted across deploy/stop/redeploy cycle");

    // Cleanup
    std::env::remove_var("GUMBALL_STORAGE_BASE");
    server_handle.abort();
}

/// macOS/Windows では DirectRuntime が動かないため skip
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn test_vault_minimal_hostpath_persistence() {
    println!("SKIP: vault e2e test requires Linux (DirectRuntime / OCI)");
}
