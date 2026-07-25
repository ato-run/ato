#![cfg(feature = "provisioning-tests")]

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, body::Body, extract::State, response::IntoResponse, routing::get};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

use super::{ArtifactManager, Registry};
use crate::resource::artifact::manager::{ArtifactConfig, ArtifactError};

/// Mock registry/artifact server. The returned counter is incremented once per
/// **artifact download**, which is the only way a test can tell a cache HIT from a
/// silent re-download — the returned install path is derived from (name, version,
/// target_os) and is therefore identical either way. Same pattern as
/// `resource::ingest::fetcher`'s cache tests.
async fn start_mock_server() -> (String, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/registry.json", get(mock_registry))
        .route("/runtime.zip", get(mock_runtime_zip))
        .route("/runtime_bad_hash.zip", get(mock_runtime_zip)) // Same content, different expected hash
        .with_state(Arc::clone(&hits));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}", addr);

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (url, handle, hits)
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

async fn mock_runtime_zip(State(hits): State<Arc<AtomicUsize>>) -> impl IntoResponse {
    hits.fetch_add(1, Ordering::SeqCst);
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
    let (base_url, _handle, _hits) = start_mock_server().await;
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
    let (base_url, _handle, _hits) = start_mock_server().await;

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

/// A second `ensure_runtime` for the same (name, version, target_os) must be served
/// from the cache — i.e. it must NOT hit the network again.
///
/// The old version of this test asserted only `path1 == path2`, which is true by
/// construction: both are `install_dir.join(binary_path)` derived from (name,
/// version, target_os), so deleting the cache branch entirely and re-downloading
/// every time would still have passed it. The download counter is the only witness
/// that the cache branch ran, so that is what is asserted here.
#[tokio::test]
async fn test_cache_hit() {
    let (base_url, _handle, hits) = start_mock_server().await;
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
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the first call must download the artifact"
    );

    // Second call: cache hit — same path AND no second download.
    let path2 = manager
        .ensure_runtime("test-runtime", "1.0.0", None)
        .await
        .unwrap();

    assert_eq!(path1, path2);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the second call re-downloaded — the cache branch did not run"
    );
}

/// The cache-hit branch returns the installed binary **without re-verifying it**.
///
/// This is a characterization test, not an endorsement. `ensure_runtime`'s early
/// return (`manager.rs`) checks only that the `.binary_path` marker exists and names
/// a file that exists; no hash is recomputed. Nor could the registry's `sha256` be
/// re-checked as-is — it is the hash of the ZIP, and what survives on disk is the
/// extracted tree, so re-verification would need a per-file digest recorded at
/// install time. Until that exists, a runtime binary tampered with after install is
/// served as-is, forever, with no further network access.
///
/// It is here so the gap is visible in the test suite instead of only in the code,
/// and so that adding an integrity re-check breaks a test that explains itself
/// rather than one that silently encoded the weakness. `test_cache_hit` above proves
/// the caching works; this proves what caching currently costs.
#[tokio::test]
async fn cache_hit_returns_the_installed_binary_without_re_verifying_it() {
    let (base_url, _handle, hits) = start_mock_server().await;
    let zip_hash = calculate_zip_hash();

    let registry_json = format!(
        r#"{{
        "runtimes": {{
            "test-runtime": {{
                "versions": {{
                    "1.0.0": {{
                        "linux-x64": {{
                            "url": "{base_url}/runtime.zip",
                            "sha256": "{zip_hash}",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-arm64": {{
                            "url": "{base_url}/runtime.zip",
                            "sha256": "{zip_hash}",
                            "binary_path": "bin/test-binary"
                        }},
                        "mac-x64": {{
                            "url": "{base_url}/runtime.zip",
                            "sha256": "{zip_hash}",
                            "binary_path": "bin/test-binary"
                        }}
                    }}
                }}
            }}
        }}
    }}"#
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
    let installed = manager
        .ensure_runtime("test-runtime", "1.0.0", None)
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Tamper with the installed binary, leaving the `.binary_path` marker intact.
    tokio::fs::write(&installed, b"#!/bin/sh\nrm -rf /\n")
        .await
        .unwrap();

    let again = manager
        .ensure_runtime("test-runtime", "1.0.0", None)
        .await
        .expect("the cache-hit branch returns Ok without inspecting the bytes");
    assert_eq!(again, installed);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "no re-download either — the tampered file is what callers get"
    );
    assert_eq!(
        tokio::fs::read(&again).await.unwrap(),
        b"#!/bin/sh\nrm -rf /\n",
        "documented gap: the cache-hit path performs no integrity re-check"
    );
}
