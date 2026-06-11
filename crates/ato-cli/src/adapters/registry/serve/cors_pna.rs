/// CORS + Private Network Access (PNA) middleware for the local registry.
///
/// Handles preflight OPTIONS requests and adds CORS response headers for an
/// explicit allowlist of origins.  Wildcard (`*`) is never used for Runtime
/// Control APIs because they require an `Authorization` bearer token and the
/// Fetch spec forbids credentials with wildcard origins.
///
/// PNA (Private Network Access):  when a browser page served over HTTPS
/// requests a loopback address, Chrome/Chromium adds
/// `Access-Control-Request-Private-Network: true` to the OPTIONS preflight.
/// We echo back `Access-Control-Allow-Private-Network: true` **only** for
/// allowlisted origins.
///
/// ## Disallowed origin handling
///
/// When an `Origin` header is present but not in the allowlist, the middleware
/// must not leak any CORS allow headers — even those that might be added by an
/// inner layer (e.g. the desktop `CorsLayer`).  The contract is:
///
/// - **OPTIONS preflight from disallowed origin**: return `204 No Content`
///   immediately with only `Vary: Origin`.  No allow headers.  The inner
///   handler is NOT called, so the desktop `CorsLayer` cannot add its ACAO.
/// - **Actual request from disallowed origin**: run the inner handler normally
///   (so auth, routing, and endpoint behaviour are preserved), then strip all
///   CORS allow headers from the response before returning.
///
/// # Configuration
///
/// Built-in defaults:
/// - `https://app.ato.run`
/// - `http://localhost:5173`
/// - `http://127.0.0.1:5173`
///
/// Override with a comma-separated env var (wildcards silently dropped):
/// ```text
/// ATO_REGISTRY_CORS_ORIGINS=https://app.ato.run,http://localhost:5173
/// ```
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode};
use axum::middleware::Next;

pub(super) const CORS_ORIGINS_ENV: &str = "ATO_REGISTRY_CORS_ORIGINS";

pub(super) const DEFAULT_ORIGINS: &[&str] = &[
    "https://app.ato.run",
    "http://localhost:5173",
    "http://127.0.0.1:5173",
];

static HDR_ORIGIN: HeaderName = HeaderName::from_static("origin");
static HDR_VARY: HeaderName = HeaderName::from_static("vary");
static HDR_ACAO: HeaderName = HeaderName::from_static("access-control-allow-origin");
static HDR_ACAC: HeaderName = HeaderName::from_static("access-control-allow-credentials");
static HDR_ACAM: HeaderName = HeaderName::from_static("access-control-allow-methods");
static HDR_ACAH: HeaderName = HeaderName::from_static("access-control-allow-headers");
static HDR_ACMA: HeaderName = HeaderName::from_static("access-control-max-age");
static HDR_ACAPN: HeaderName = HeaderName::from_static("access-control-allow-private-network");
static HDR_ACRPN: HeaderName = HeaderName::from_static("access-control-request-private-network");

const ALLOWED_METHODS: &str = "GET, POST, OPTIONS";
const ALLOWED_HEADERS: &str = "Authorization, Content-Type, Accept";
const MAX_AGE_SECS: &str = "86400";

/// Parse the allowlist, falling back to built-in defaults when the env var is
/// absent or empty.  Entries containing `*` are silently dropped so the env
/// var cannot accidentally enable wildcard CORS.
pub(super) fn parse_allowed_origins() -> Arc<Vec<String>> {
    let from_env = parse_origins_str(&std::env::var(CORS_ORIGINS_ENV).unwrap_or_default());
    if from_env.is_empty() {
        Arc::new(DEFAULT_ORIGINS.iter().map(|s| s.to_string()).collect())
    } else {
        Arc::new(from_env)
    }
}

/// Pure helper: parse a comma-separated origin string into a Vec.
/// Exposed for unit tests.
pub(super) fn parse_origins_str(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty() && !v.contains('*'))
        .map(str::to_owned)
        .collect()
}

/// Return true iff `origin` appears in `allowed`, treating trailing slashes
/// as insignificant.
pub(super) fn is_allowed(origin: &str, allowed: &[String]) -> bool {
    let normalized = origin.trim_end_matches('/');
    allowed
        .iter()
        .any(|a| a.trim_end_matches('/') == normalized)
}

/// Axum middleware function.  Capture `allowed` in a closure at the call
/// site and pass it as state:
///
/// ```ignore
/// let origins = parse_allowed_origins();
/// app.layer(axum::middleware::from_fn(move |req, next| {
///     cors_pna_layer(Arc::clone(&origins), req, next)
/// }))
/// ```
pub(super) async fn cors_pna_layer(
    allowed: Arc<Vec<String>>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let origin_val = request.headers().get(&HDR_ORIGIN).cloned();

    // No Origin header → not a cross-origin request; pass through.
    let origin_str = match &origin_val {
        Some(v) => match v.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => return next.run(request).await,
        },
        None => return next.run(request).await,
    };

    // Origin present but not in allowlist.
    //
    // We must not let any inner layer (e.g. the desktop CorsLayer) attach CORS
    // allow headers that could be misread by diagnostics tools or scanners.
    //
    // - OPTIONS preflight: short-circuit immediately so the inner stack never
    //   runs.  Return 204 + Vary: Origin and nothing else.
    // - Actual request: run the inner handler (preserves auth/routing/endpoint
    //   behaviour), then strip all CORS allow headers from the response.
    if !is_allowed(&origin_str, &allowed) {
        if request.method() == Method::OPTIONS {
            let mut resp = Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Body::empty())
                .expect("infallible disallowed-origin preflight response");
            set_header(resp.headers_mut(), &HDR_VARY, "Origin");
            return resp;
        }
        let mut resp = next.run(request).await;
        remove_cors_allow_headers(resp.headers_mut());
        return resp;
    }

    let wants_pna = request
        .headers()
        .get(&HDR_ACRPN)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // Preflight (OPTIONS) — short-circuit without calling the handler.
    if request.method() == Method::OPTIONS {
        let mut resp = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .expect("infallible preflight response");

        insert_cors_preflight_headers(resp.headers_mut(), &origin_str, wants_pna);
        return resp;
    }

    // Actual request — run the handler, then decorate the response.
    let mut resp = next.run(request).await;
    let h = resp.headers_mut();
    set_header(h, &HDR_ACAO, &origin_str);
    set_header(h, &HDR_VARY, "Origin");

    resp
}

fn insert_cors_preflight_headers(headers: &mut HeaderMap, origin: &str, pna: bool) {
    set_header(headers, &HDR_ACAO, origin);
    set_header(headers, &HDR_ACAM, ALLOWED_METHODS);
    set_header(headers, &HDR_ACAH, ALLOWED_HEADERS);
    set_header(headers, &HDR_ACMA, MAX_AGE_SECS);
    set_header(headers, &HDR_VARY, "Origin");
    if pna {
        set_header(headers, &HDR_ACAPN, "true");
    }
}

/// Strip all CORS allow-* headers from a response.  Used to prevent inner
/// layers (e.g. the desktop `CorsLayer`) from leaking their ACAO to disallowed
/// browser origins.
fn remove_cors_allow_headers(headers: &mut HeaderMap) {
    headers.remove(&HDR_ACAO);
    headers.remove(&HDR_ACAC);
    headers.remove(&HDR_ACAM);
    headers.remove(&HDR_ACAH);
    headers.remove(&HDR_ACMA);
    headers.remove(&HDR_ACAPN);
}

fn set_header(headers: &mut HeaderMap, name: &HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(name, v);
    }
}

// ─── Unit tests (pure functions only) ────────────────────────────────────
// Integration tests that exercise the middleware over a live Axum router
// are in serve/tests.rs.

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn allowed_origin_recognised() {
        assert!(is_allowed(
            "https://app.ato.run",
            &["https://app.ato.run".into()]
        ));
    }

    #[test]
    fn trailing_slash_normalised_on_request_origin() {
        assert!(is_allowed(
            "https://app.ato.run/",
            &["https://app.ato.run".into()]
        ));
    }

    #[test]
    fn trailing_slash_normalised_on_allowlist_entry() {
        assert!(is_allowed(
            "https://app.ato.run",
            &["https://app.ato.run/".into()]
        ));
    }

    #[test]
    fn disallowed_origin_rejected() {
        assert!(!is_allowed(
            "https://evil.example.com",
            &["https://app.ato.run".into()]
        ));
    }

    #[test]
    fn localhost_5173_in_defaults() {
        let list = parse_allowed_origins();
        assert!(list.contains(&"http://localhost:5173".to_string()));
    }

    #[test]
    fn loopback_5173_in_defaults() {
        let list = parse_allowed_origins();
        assert!(list.contains(&"http://127.0.0.1:5173".to_string()));
    }

    #[test]
    fn app_ato_run_in_defaults() {
        let list = parse_allowed_origins();
        assert!(list.contains(&"https://app.ato.run".to_string()));
    }

    #[test]
    fn env_override_replaces_defaults() {
        let parsed = parse_origins_str("https://custom.example.com,http://localhost:3000");
        assert_eq!(
            parsed,
            vec!["https://custom.example.com", "http://localhost:3000"]
        );
    }

    #[test]
    fn wildcard_entry_silently_dropped() {
        let parsed = parse_origins_str("*, https://app.ato.run");
        assert!(!parsed.iter().any(|o| o.contains('*')));
        assert_eq!(parsed, vec!["https://app.ato.run"]);
    }

    #[test]
    fn empty_string_falls_back_to_defaults() {
        let parsed = parse_origins_str("");
        assert!(parsed.is_empty(), "empty input → fall through to defaults");
    }

    #[test]
    fn whitespace_trimmed() {
        let parsed = parse_origins_str("  https://app.ato.run  ,  http://localhost:5173  ");
        assert_eq!(parsed, vec!["https://app.ato.run", "http://localhost:5173"]);
    }
}
