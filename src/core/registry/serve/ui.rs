use super::*;

pub(super) async fn handle_ui_request(uri: Uri) -> impl IntoResponse {
    let path = uri.path();
    if path == "/v1" || path.starts_with("/v1/") {
        return json_error(StatusCode::NOT_FOUND, "not_found", "API route not found");
    }

    if let Some(response) = ui_asset_response(path) {
        return response;
    }

    if let Some(response) = ui_embedded_response("index.html", true) {
        return response;
    }

    let html = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Web UI unavailable</title></head><body style=\"font-family:sans-serif;padding:24px\"><h2>Web UI assets are missing</h2><p>Build <code>apps/ato-store-local</code> and rebuild <code>ato</code>.</p><pre>npm install --prefix apps/ato-store-local\ncargo build</pre></body></html>";
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        html,
    )
        .into_response()
}

fn ui_asset_response(request_path: &str) -> Option<axum::response::Response> {
    let normalized = normalize_ui_path(request_path)?;
    ui_embedded_response(&normalized, false)
}

pub(super) fn normalize_ui_path(request_path: &str) -> Option<String> {
    let path = request_path.trim_start_matches('/');
    if path.is_empty() {
        return Some("index.html".to_string());
    }
    if path.contains('\\') || path.contains("..") {
        return None;
    }
    Some(path.to_string())
}

fn ui_embedded_response(path: &str, force_html: bool) -> Option<axum::response::Response> {
    let file = LocalRegistryUiAssets::get(path)?;
    let mime = if force_html {
        "text/html; charset=utf-8".to_string()
    } else {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_string()
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        cache_control_for_ui_path(path, force_html),
    );
    Some((StatusCode::OK, headers, file.data.into_owned()).into_response())
}

pub(super) fn cache_control_for_ui_path(path: &str, force_html: bool) -> HeaderValue {
    if force_html || path == "index.html" {
        return HeaderValue::from_static("no-cache");
    }
    if path.starts_with("assets/") {
        return HeaderValue::from_static("public, max-age=31536000, immutable");
    }
    HeaderValue::from_static("public, max-age=300")
}
