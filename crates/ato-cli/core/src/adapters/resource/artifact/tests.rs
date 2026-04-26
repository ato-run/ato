#![cfg(feature = "provisioning-tests")]

use std::io::Write;

use axum::{body::Body, response::IntoResponse, routing::get, Router};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

use super::{ArtifactManager, Registry};
use crate::resource::artifact::manager::{ArtifactConfig, ArtifactError};

async fn start_mock_server() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/registry.json", get(mock_registry))
        .route("/runtime.zip", get(mock_runtime_zip))
        .route("/runtime_bad_hash.zip", get(mock_runtime_zip)); // Same content, different expected hash

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (url, handle)
}

async fn mock_registry() -> impl IntoResponse {
    r#"{
        "runtimes": {
            "test-runtime": {
                "versions": {
                    "1.0.0": {
                        "linux-x64": {
                            "url": "/runtime.zip",
                            "sha256": "HASH_PLACEHOLDER",
                            "binary_path": "bin/test-binary"
                        },
                        "mac-arm64": {
                            "url": "/runtime.zip",
                            "sha256": "HASH_PLACEHOLDER",
                            "binary_path": "bin/test-binary"
                        },
                         "mac-x64": {
                            "url": "/runtime.zip",
                            "sha256": "HASH_PLACEHOLDER",
                            "binary_path": "bin/test-binary"
                        }
                    }
                }
            }
        }
    }"#
}

async fn mock_runtime_zip() -> impl IntoResponse {
    Body::from(build_runtime_zip_bytes())
}

fn build_runtime_zip_bytes() -> Vec<u8> {
    // Make the zip deterministic: zip headers include timestamps by default.
    let mut buf = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));

    let options = zip::write::FileOptions::<()>::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).unwrap());

    zip.start_file("bin/test-binary", options).unwrap();
    zip.write_all(b"#!/bin/sh\necho 'Hello'").unwrap();
    zip.finish().unwrap();

    buf
}

fn calculate_zip_hash() -> String {
    let buf = build_runtime_zip_bytes();
    let mut hasher = Sha256::new();
    hasher.update(&buf);
    format!("{:x}", hasher.finalize())
}

#[tokio::test]
async fn test_registry_parsing() {
    let registry_json = r#"{
        "runtimes": {
            "test": {
                "versions": {
                    "1.0": {
                        "linux-x64": {
                            "url": "http://example.com/file.zip",
                            "sha256": "abc",
                            "binary_path": "bin/run"
                        }
                    }
                }
            }
        }
    }"#;

    let registry: Registry = serde_json::from_str(registry_json).unwrap();
    assert!(registry.runtimes.contains_key("test"));
    let version = &registry.runtimes["test"].versions["1.0"]["linux-x64"];
    assert_eq!(version.url, "http://example.com/file.zip");
}

#[tokio::test]
async fn test_ensure_runtime_success() {
    let (base_url, _handle) = start_mock_server().await;
    let zip_hash = calculate_zip_hash();

    // Create registry with correct hash and full URL
    let registry_json = format!(
        r#"{{
        "runtimes": {{
            "test-runtime": {{
                "versions": {{
                    "1.0.0": {{
                        "linux-x64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "{}",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-arm64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "{}",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-x64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "{}",
                            "binary_path": "bin/test-binary"
                        }}
                    }}
                }}
            }}
        }}
    }}"#,
        base_url, zip_hash, base_url, zip_hash, base_url, zip_hash
    );

    let temp_dir = tempfile::tempdir().unwrap();
    let registry_path = temp_dir.path().join("registry.json");
    tokio::fs::write(&registry_path, registry_json)
        .await
        .unwrap();

    let config = ArtifactConfig {
        registry_url: format!("file://{}", registry_path.to_string_lossy()),
        cache_path: temp_dir.path().join("cache"),
        cas_root: None,
    };

    let manager = ArtifactManager::new(config).await.unwrap();
    let result = manager.ensure_runtime("test-runtime", "1.0.0", None).await;

    assert!(result.is_ok());
    let path = result.unwrap();
    assert!(path.exists());
    assert!(path.ends_with("bin/test-binary"));
}

#[tokio::test]
async fn test_hash_verification_failure() {
    let (base_url, _handle) = start_mock_server().await;

    // Registry with WRONG hash
    let registry_json = format!(
        r#"{{
        "runtimes": {{
            "test-runtime": {{
                "versions": {{
                    "1.0.0": {{
                        "linux-x64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "badhash",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-arm64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "badhash",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-x64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "badhash",
                            "binary_path": "bin/test-binary"
                        }}
                    }}
                }}
            }}
        }}
    }}"#,
        base_url, base_url, base_url
    );

    let temp_dir = tempfile::tempdir().unwrap();
    let registry_path = temp_dir.path().join("registry.json");
    tokio::fs::write(&registry_path, registry_json)
        .await
        .unwrap();

    let config = ArtifactConfig {
        registry_url: format!("file://{}", registry_path.to_string_lossy()),
        cache_path: temp_dir.path().join("cache"),
        cas_root: None,
    };

    let manager = ArtifactManager::new(config).await.unwrap();
    let result = manager.ensure_runtime("test-runtime", "1.0.0", None).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ArtifactError::HashMismatch { .. } => (),
        e => panic!("Expected HashMismatch, got {:?}", e),
    }
}

#[tokio::test]
async fn test_cache_hit() {
    let (base_url, _handle) = start_mock_server().await;
    let zip_hash = calculate_zip_hash();

    let registry_json = format!(
        r#"{{
        "runtimes": {{
            "test-runtime": {{
                "versions": {{
                    "1.0.0": {{
                        "linux-x64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "{}",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-arm64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "{}",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-x64": {{
                            "url": "{}/runtime.zip",
                            "sha256": "{}",
                            "binary_path": "bin/test-binary"
                        }}
                    }}
                }}
            }}
        }}
    }}"#,
        base_url, zip_hash, base_url, zip_hash, base_url, zip_hash
    );

    let temp_dir = tempfile::tempdir().unwrap();
    let registry_path = temp_dir.path().join("registry.json");
    tokio::fs::write(&registry_path, registry_json)
        .await
        .unwrap();

    let config = ArtifactConfig {
        registry_url: format!("file://{}", registry_path.to_string_lossy()),
        cache_path: temp_dir.path().join("cache"),
        cas_root: None,
    };

    let manager = ArtifactManager::new(config).await.unwrap();

    // First call: Download
    let path1 = manager
        .ensure_runtime("test-runtime", "1.0.0", None)
        .await
        .unwrap();

    // Second call: Cache hit
    // We can verify it's a cache hit by stopping the server or checking logs,
    // but here we just ensure it returns success quickly and same path.
    let path2 = manager
        .ensure_runtime("test-runtime", "1.0.0", None)
        .await
        .unwrap();

    assert_eq!(path1, path2);
}
