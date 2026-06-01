use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use percent_encoding::percent_decode_str;
use wry::http::Response;

use super::materializer::{
    SystemCapsuleLookup, SystemCapsuleRoot, SystemCapsuleUnavailable, SystemCapsuleUnavailableKind,
    lookup_system_capsule,
};

const INDEX_FILE: &str = "index.html";
const HTML_MIME: &str = "text/html; charset=utf-8";

/// Per-request cache of `lookup_system_capsule` results so repeated
/// asset requests from a single WebView page-load do not redundantly
/// read `current.json`, `degraded.json`, stat directories, and
/// canonicalise paths on disk. Only `Ready` results are cached;
/// `Unavailable` results are rechecked on every request because they
/// may be transient (e.g. bootstrap hasn't finished yet).
fn lookup_cache() -> &'static Mutex<HashMap<String, SystemCapsuleLookup>> {
    static CACHE: OnceLock<Mutex<HashMap<String, SystemCapsuleLookup>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clear all cached system-capsule lookups. Called at the end of
/// `bootstrap_from_assets` so the next request picks up any changes
/// to materialized directories or degradation state.
pub fn clear_lookup_cache() {
    if let Ok(mut cache) = lookup_cache().lock() {
        cache.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticAssetResponse {
    pub status_code: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

pub fn resolve_system_capsule_asset(slug: &str, request_path: &str) -> StaticAssetResponse {
    let lookup = {
        let mut cache = lookup_cache().lock().unwrap();
        if let Some(cached) = cache.get(slug) {
            cached.clone()
        } else {
            let result = match lookup_system_capsule(slug) {
                Ok(lookup) => lookup,
                Err(_error) => SystemCapsuleLookup::Unavailable(SystemCapsuleUnavailable {
                    slug: slug.to_string(),
                    degraded: None,
                    kind: SystemCapsuleUnavailableKind::UnknownCapsule,
                }),
            };
            if matches!(result, SystemCapsuleLookup::Ready(_)) {
                cache.insert(slug.to_string(), result.clone());
            }
            result
        }
    };
    match lookup {
        SystemCapsuleLookup::Ready(root) => resolve_ready_root(root, request_path),
        SystemCapsuleLookup::Unavailable(unavailable) => resolve_unavailable(unavailable),
    }
}

pub fn resolve_system_capsule_protocol_response(
    slug: &str,
    request_path: &str,
) -> Response<Cow<'static, [u8]>> {
    let asset = resolve_system_capsule_asset(slug, request_path);
    Response::builder()
        .status(asset.status_code)
        .header("Content-Type", asset.content_type)
        .body(Cow::Owned(asset.body))
        .expect("system capsule protocol response must build")
}

fn resolve_ready_root(root: SystemCapsuleRoot, request_path: &str) -> StaticAssetResponse {
    if let Some(degraded) = root.degraded.as_ref() {
        return error_page_response(
            503,
            "System capsule is degraded",
            root.slug.as_str(),
            &degraded.error,
        );
    }

    let relative_path = match sanitize_request_path(request_path) {
        Ok(path) => path,
        Err(error) => {
            return error_page_response(400, "Invalid request path", root.slug.as_str(), &error);
        }
    };

    let serving_root = match fs::canonicalize(&root.serving_root) {
        Ok(path) => path,
        Err(error) => {
            return error_page_response(
                500,
                "System capsule serving root is unavailable",
                root.slug.as_str(),
                &error.to_string(),
            );
        }
    };

    let mut target_path = serving_root.join(&relative_path);
    if target_path.is_dir() {
        target_path = target_path.join(INDEX_FILE);
    }
    if !target_path.exists() {
        return error_page_response(404, "Asset not found", root.slug.as_str(), request_path);
    }

    let canonical_target = match fs::canonicalize(&target_path) {
        Ok(path) => path,
        Err(error) => {
            return error_page_response(
                404,
                "Asset not found",
                root.slug.as_str(),
                &error.to_string(),
            );
        }
    };
    if !canonical_target.starts_with(&serving_root) {
        return error_page_response(
            400,
            "Invalid request path",
            root.slug.as_str(),
            request_path,
        );
    }
    if !canonical_target.is_file() {
        return error_page_response(404, "Asset not found", root.slug.as_str(), request_path);
    }

    match fs::read(&canonical_target) {
        Ok(bytes) => StaticAssetResponse {
            status_code: 200,
            content_type: content_type_for(&canonical_target),
            body: bytes,
        },
        Err(error) => error_page_response(
            500,
            "Asset read failed",
            root.slug.as_str(),
            &error.to_string(),
        ),
    }
}

fn resolve_unavailable(unavailable: SystemCapsuleUnavailable) -> StaticAssetResponse {
    let title = match unavailable.kind {
        SystemCapsuleUnavailableKind::UnknownCapsule => "System capsule not found",
        SystemCapsuleUnavailableKind::MissingCurrentRecord => "System capsule is unavailable",
        SystemCapsuleUnavailableKind::MaterializedRootMissing { .. }
        | SystemCapsuleUnavailableKind::ServingRootMissing { .. } => {
            "System capsule materialization is corrupted"
        }
    };
    let detail = match unavailable.kind {
        SystemCapsuleUnavailableKind::UnknownCapsule => "Unknown system capsule slug".to_string(),
        SystemCapsuleUnavailableKind::MissingCurrentRecord => {
            "No current materialization record exists for this system capsule".to_string()
        }
        SystemCapsuleUnavailableKind::MaterializedRootMissing { ref root } => {
            format!("Materialized root is missing: {}", root.display())
        }
        SystemCapsuleUnavailableKind::ServingRootMissing {
            ref root,
            ref serving_root,
        } => format!(
            "Serving root is missing: {} (materialized root: {})",
            serving_root.display(),
            root.display()
        ),
    };
    let status = if unavailable.degraded.is_some() {
        503
    } else {
        500
    };
    let detail = unavailable
        .degraded
        .as_ref()
        .map(|degraded| degraded.error.clone())
        .unwrap_or(detail);
    error_page_response(status, title, unavailable.slug.as_str(), &detail)
}

fn sanitize_request_path(request_path: &str) -> Result<PathBuf, String> {
    let without_query = request_path
        .split_once('?')
        .map(|(path, _)| path)
        .unwrap_or(request_path)
        .split_once('#')
        .map(|(path, _)| path)
        .unwrap_or(request_path);
    let decoded = percent_decode_str(without_query)
        .decode_utf8()
        .map_err(|_| "request path is not valid UTF-8 after percent decoding".to_string())?;

    if decoded.contains('\0') {
        return Err("request path contains NUL byte".to_string());
    }

    let trimmed = decoded.trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(PathBuf::from(INDEX_FILE));
    }

    let mut relative_path = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(segment) => relative_path.push(segment),
            _ => return Err("request path contains an invalid component".to_string()),
        }
    }

    if relative_path.as_os_str().is_empty() {
        return Ok(PathBuf::from(INDEX_FILE));
    }
    Ok(relative_path)
}

fn content_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    match extension.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("map") => "application/json",
        _ => "application/octet-stream",
    }
}

fn error_page_response(
    status_code: u16,
    title: &str,
    slug: &str,
    detail: &str,
) -> StaticAssetResponse {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body><h1>{title}</h1><p>capsule: {slug}</p><pre>{detail}</pre></body></html>"
    )
    .into_bytes();
    StaticAssetResponse {
        status_code,
        content_type: HTML_MIME,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_system_capsule_asset;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    struct AtoHomeGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl AtoHomeGuard {
        fn new(path: &Path) -> Self {
            let previous = std::env::var_os("ATO_HOME");
            unsafe {
                std::env::set_var("ATO_HOME", path);
            }
            Self { previous }
        }
    }

    impl Drop for AtoHomeGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                unsafe {
                    std::env::set_var("ATO_HOME", previous);
                }
            } else {
                unsafe {
                    std::env::remove_var("ATO_HOME");
                }
            }
        }
    }

    fn bootstrap_store_assets(home: &TempDir, assets: &TempDir) {
        let _guard = AtoHomeGuard::new(home.path());
        fs::create_dir_all(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("assets"),
        )
        .expect("store dist assets should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("index.html"),
            b"<html>store</html>",
        )
        .expect("store index should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("assets")
                .join("app.js"),
            b"console.log('store');",
        )
        .expect("store app.js should exist");
        fs::write(
            assets
                .path()
                .join("system")
                .join("ato-store")
                .join("dist")
                .join("blob.unknown"),
            b"blob",
        )
        .expect("store unknown file should exist");

        super::super::materializer::bootstrap_from_assets(assets.path())
            .expect("bootstrap should succeed");
    }

    #[test]
    fn resolves_root_request_to_index_html() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);

        let response = resolve_system_capsule_asset("ato-store", "/");

        assert_eq!(response.status_code, 200);
        assert_eq!(response.content_type, "text/html; charset=utf-8");
        assert!(String::from_utf8_lossy(&response.body).contains("store"));
    }

    #[test]
    fn resolves_nested_asset_from_serving_root() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);

        let response = resolve_system_capsule_asset("ato-store", "/assets/app.js");

        assert_eq!(response.status_code, 200);
        assert_eq!(
            response.content_type,
            "application/javascript; charset=utf-8"
        );
    }

    #[test]
    fn rejects_parent_directory_traversal() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);

        let response = resolve_system_capsule_asset("ato-store", "/../secret.txt");

        assert_eq!(response.status_code, 400);
        assert!(String::from_utf8_lossy(&response.body).contains("Invalid request path"));
    }

    #[test]
    fn rejects_percent_encoded_parent_directory_traversal() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);

        let response = resolve_system_capsule_asset("ato-store", "/%2e%2e/secret.txt");

        assert_eq!(response.status_code, 400);
    }

    #[test]
    fn resolves_directory_request_to_index_html() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);

        let response = resolve_system_capsule_asset("ato-store", "/assets/");

        assert_eq!(response.status_code, 404);
    }

    #[test]
    fn falls_back_to_octet_stream_for_unknown_mime_type() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);

        let response = resolve_system_capsule_asset("ato-store", "/blob.unknown");

        assert_eq!(response.status_code, 200);
        assert_eq!(response.content_type, "application/octet-stream");
    }

    #[test]
    fn returns_degraded_error_page_when_degraded_record_exists() {
        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);
        fs::write(
            home.path()
                .join("apps")
                .join("ato-desktop")
                .join("system-capsules")
                .join("ato-store.degraded.json"),
            r#"{"capsule":"ato-store","error":"broken build","seed_hash":null,"lockfile_hash":null,"recorded_at_unix_ms":1}"#,
        )
        .expect("degraded record should write");

        let response = resolve_system_capsule_asset("ato-store", "/");

        assert_eq!(response.status_code, 503);
        assert!(String::from_utf8_lossy(&response.body).contains("broken build"));
    }

    #[test]
    fn returns_error_page_when_current_record_is_missing() {
        let home = TempDir::new().expect("temp home should exist");
        let _guard = AtoHomeGuard::new(home.path());

        let response = resolve_system_capsule_asset("ato-store", "/");

        assert_eq!(response.status_code, 500);
        assert!(
            String::from_utf8_lossy(&response.body).contains("No current materialization record")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_that_escapes_serving_root() {
        use std::os::unix::fs::symlink;

        let home = TempDir::new().expect("temp home should exist");
        let assets = TempDir::new().expect("temp assets should exist");
        let outside = TempDir::new().expect("outside dir should exist");
        let _guard = AtoHomeGuard::new(home.path());
        bootstrap_store_assets(&home, &assets);

        let external_file = outside.path().join("secret.txt");
        fs::write(&external_file, b"secret").expect("external file should write");

        let current_root = super::super::materializer::current_materialized_root("ato-store")
            .expect("current root should resolve")
            .expect("current root should exist");
        symlink(&external_file, current_root.join("dist").join("escape.txt"))
            .expect("symlink should be created");

        let response = resolve_system_capsule_asset("ato-store", "/escape.txt");

        assert_eq!(response.status_code, 400);
    }
}
