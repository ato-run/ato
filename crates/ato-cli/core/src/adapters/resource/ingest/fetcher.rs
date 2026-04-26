use crate::error::{CapsuleError, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ResourceFetchRequest {
    pub resource_id: String,
    pub url: String,
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FetcherConfig {
    pub cache_dir: PathBuf,
    pub allowed_host_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFetchResult {
    pub local_path: PathBuf,
    pub cached: bool,
    pub bytes_downloaded: u64,
}

pub async fn fetch_resource(
    req: ResourceFetchRequest,
    cfg: FetcherConfig,
) -> Result<ResourceFetchResult> {
    if req.resource_id.trim().is_empty() {
        return Err(CapsuleError::Config("resource_id is required".to_string()));
    }
    if req.url.trim().is_empty() {
        return Err(CapsuleError::Config("url is required".to_string()));
    }

    let cache_dir = cfg.cache_dir;
    let file_name = filename_from_url(&req.url).unwrap_or_else(|| "resource.bin".to_string());
    let dest_path = cache_dir.join(&req.resource_id).join(file_name);

    let dest_str = dest_path
        .to_str()
        .ok_or_else(|| CapsuleError::Config("destination path is not valid UTF-8".to_string()))?;

    crate::security::validate_path(dest_str, &cfg.allowed_host_paths)
        .map_err(|e| CapsuleError::Config(format!("Invalid destination path: {e}")))?;

    if dest_path.exists() {
        if let Some(expected) = req
            .expected_sha256
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            let actual = sha256_hex_of_file(&dest_path)?;
            if normalize_hex(expected) == actual {
                return Ok(ResourceFetchResult {
                    local_path: dest_path,
                    cached: true,
                    bytes_downloaded: 0,
                });
            }
            // Hash mismatch: fallthrough to re-download.
        } else {
            return Ok(ResourceFetchResult {
                local_path: dest_path,
                cached: true,
                bytes_downloaded: 0,
            });
        }
    }

    let bytes_downloaded =
        super::http::download_file(&req.url, dest_str, &cfg.allowed_host_paths).await?;

    if let Some(expected) = req
        .expected_sha256
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        let actual = sha256_hex_of_file(&dest_path)?;
        if normalize_hex(expected) != actual {
            let _ = fs::remove_file(&dest_path);
            return Err(CapsuleError::Pack(format!(
                "sha256 mismatch: expected={}, actual={}",
                normalize_hex(expected),
                actual
            )));
        }
    }

    Ok(ResourceFetchResult {
        local_path: dest_path,
        cached: false,
        bytes_downloaded,
    })
}

fn filename_from_url(url: &str) -> Option<String> {
    // Minimal parsing: strip query/fragment, then take the last non-empty path segment.
    let no_frag = url.split('#').next().unwrap_or(url);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    let seg = no_query.split('/').rfind(|s| !s.is_empty())?;
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}

fn sha256_hex_of_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn normalize_hex(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[cfg(test)]
#[cfg(feature = "provisioning-tests")]
mod tests {
    use std::fs;
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use axum::routing::get;
    use axum::Router;
    use tokio::net::TcpListener;

    use super::{fetch_resource, FetcherConfig, ResourceFetchRequest};

    async fn start_bytes_server(
        body: &'static [u8],
        hits: Arc<AtomicUsize>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/model.bin",
            get(move || {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::body::Bytes::from_static(body)
                }
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let url = format!("http://{}/model.bin", addr);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        (url, handle)
    }

    #[tokio::test]
    async fn download_success_with_mock_http_server() {
        let hits = Arc::new(AtomicUsize::new(0));
        let (url, _server) = start_bytes_server(b"hello-model", hits.clone()).await;

        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("models");

        let cfg = FetcherConfig {
            cache_dir: cache_dir.clone(),
            allowed_host_paths: vec![tmp.path().to_string_lossy().to_string()],
        };

        let res = fetch_resource(
            ResourceFetchRequest {
                resource_id: "test-model".to_string(),
                url: url.clone(),
                expected_sha256: None,
            },
            cfg,
        )
        .await
        .unwrap();

        assert!(res.local_path.exists());
        assert_eq!(fs::read(&res.local_path).unwrap(), b"hello-model");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert!(!res.cached);
        assert!(res.bytes_downloaded > 0);
        assert!(res.local_path.starts_with(cache_dir));
    }

    #[tokio::test]
    async fn skips_when_cached_file_exists() {
        let hits = Arc::new(AtomicUsize::new(0));
        let (url, _server) = start_bytes_server(b"should-not-be-fetched", hits.clone()).await;

        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("models");
        let cached_path = cache_dir.join("test-model").join("model.bin");
        fs::create_dir_all(cached_path.parent().unwrap()).unwrap();
        fs::write(&cached_path, b"cached").unwrap();

        let cfg = FetcherConfig {
            cache_dir: cache_dir.clone(),
            allowed_host_paths: vec![tmp.path().to_string_lossy().to_string()],
        };

        let res = fetch_resource(
            ResourceFetchRequest {
                resource_id: "test-model".to_string(),
                url,
                expected_sha256: None,
            },
            cfg,
        )
        .await
        .unwrap();

        assert_eq!(res.local_path, cached_path);
        assert_eq!(fs::read(&res.local_path).unwrap(), b"cached");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert!(res.cached);
        assert_eq!(res.bytes_downloaded, 0);
    }

    #[tokio::test]
    async fn fails_if_cache_dir_not_allowlisted() {
        let hits = Arc::new(AtomicUsize::new(0));
        let (url, _server) = start_bytes_server(b"hello", hits.clone()).await;

        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("models");

        let cfg = FetcherConfig {
            cache_dir,
            allowed_host_paths: vec!["/opt/models".to_string()],
        };

        let err = fetch_resource(
            ResourceFetchRequest {
                resource_id: "test-model".to_string(),
                url,
                expected_sha256: None,
            },
            cfg,
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("Invalid destination path"));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}
