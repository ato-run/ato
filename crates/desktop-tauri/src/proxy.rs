//! `atoview://` same-origin app-view proxy.
//!
//! ## Why
//!
//! The bundled PWA runs at the shell's local-asset origin (`tauri://localhost`),
//! but a preview app is served by the Ato app-proxy at
//! `https://<session>.app.ato.run`. Those are *cross-site*, so the guest
//! app-view auth cookie (`__Host-ato_app_view`, `SameSite=Lax`) minted by the
//! app-proxy's 303 exchange is never sent back on the preview iframe's
//! cross-site requests — the app answers `app_view_access_required`.
//!
//! ## How
//!
//! Instead of loading the upstream directly, the preview iframe loads
//! `atoview://<session>.app.ato.run/…`. This handler fetches the real
//! `https://<session>.app.ato.run/…` with a shell-owned [`reqwest`] client whose
//! **cookie jar** holds the app-view cookie. reqwest performs the token→cookie
//! 303 exchange and every subsequent request itself, so the *browser* never has
//! to store or send a cross-site cookie: the auth is entirely server-side inside
//! the shell. The iframe is same-origin with the scheme the shell controls, and
//! `SameSite` no longer applies.
//!
//! ## Bounds
//!
//! - **SSRF guard:** only hosts under [`ALLOWED_HOST_SUFFIX`] are proxied.
//! - The single-use `app_view_token` is stripped from a host's requests once the
//!   exchange has happened (the jar holds the cookie), so an iframe reload does
//!   not replay a consumed token into a 403.
//! - Framing headers from the upstream are dropped so the sandboxed preview
//!   iframe can render; the iframe's own `sandbox` attribute is the isolation
//!   boundary. WebSocket upgrades (pixel-stream surfaces) are NOT proxied here —
//!   this handler covers Web surfaces; pixel streams keep their own transport.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Mutex;

use tauri::http::{Request, Response, StatusCode};

/// The custom scheme the preview iframe loads through.
pub const ATOVIEW_SCHEME: &str = "atoview";

/// SSRF guard: only the Ato app-proxy session hosts may be proxied. A preview
/// `app_url` is always `https://<session>.app.ato.run/…`.
const ALLOWED_HOST_SUFFIX: &str = ".app.ato.run";

/// Query parameter carrying the single-use guest app-view token.
const APP_VIEW_TOKEN_PARAM: &str = "app_view_token";

/// Shell-owned proxy for `atoview://` requests.
pub struct AppViewProxy {
    client: reqwest::Client,
    /// Hosts whose single-use token has already been exchanged for a cookie
    /// (held in the client jar). Subsequent requests strip the token so a reload
    /// does not replay it into a 403.
    authed_hosts: Mutex<HashSet<String>>,
}

impl AppViewProxy {
    /// Build a proxy with a cookie-storing HTTP client.
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .build()
            .expect("build app-view proxy http client");
        Self {
            client,
            authed_hosts: Mutex::new(HashSet::new()),
        }
    }

    /// Proxy one `atoview://` request, always producing a Response (errors become
    /// a small JSON body so nothing hangs the webview).
    pub async fn handle(&self, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
        match self.try_proxy(request).await {
            Ok(response) => response,
            Err(status) => error_response(status),
        }
    }

    async fn try_proxy(
        &self,
        request: Request<Vec<u8>>,
    ) -> Result<Response<Cow<'static, [u8]>>, StatusCode> {
        // atoview://<host>/<path>?<query>  →  https://<host>/<path>?<query>
        let uri = request.uri();
        let host = uri.host().ok_or(StatusCode::BAD_REQUEST)?.to_string();
        if !host.ends_with(ALLOWED_HOST_SUFFIX) {
            return Err(StatusCode::FORBIDDEN);
        }
        let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
        let mut upstream = format!("https://{host}{path_and_query}");

        // Once the host is authed (the jar holds the cookie), strip the single-use
        // token so a reload of the token-bearing iframe URL does not replay it.
        let already_authed = self
            .authed_hosts
            .lock()
            .map(|set| set.contains(&host))
            .unwrap_or(false);
        if already_authed {
            upstream = strip_query_param(&upstream, APP_VIEW_TOKEN_PARAM);
        }

        let method = reqwest::Method::from_bytes(request.method().as_str().as_bytes())
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let mut outbound = self.client.request(method, &upstream);
        for (name, value) in request.headers() {
            if should_drop_request_header(name.as_str()) {
                continue;
            }
            if let Ok(text) = value.to_str() {
                outbound = outbound.header(name.as_str(), text);
            }
        }
        let body = request.into_body();
        if !body.is_empty() {
            outbound = outbound.body(body);
        }

        let response = outbound.send().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
        // The exchange (or a plain authed request) succeeded — the jar now holds
        // the cookie, so future requests to this host can drop the token.
        if let Ok(mut set) = self.authed_hosts.lock() {
            set.insert(host);
        }

        let status = StatusCode::from_u16(response.status().as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        let mut builder = Response::builder().status(status);
        for (name, value) in response.headers() {
            if should_drop_response_header(name.as_str()) {
                continue;
            }
            builder = builder.header(name.as_str(), value.as_bytes());
        }
        let bytes = response.bytes().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
        builder
            .body(Cow::Owned(bytes.to_vec()))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl Default for AppViewProxy {
    fn default() -> Self {
        Self::new()
    }
}

/// Request headers the shell owns and must not forward verbatim: the cookie jar
/// supplies `cookie`; reqwest sets `host`; the custom-scheme `origin`/`referer`
/// would confuse the upstream's origin checks.
fn should_drop_request_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "host" | "cookie" | "origin" | "referer" | "connection" | "content-length"
    )
}

/// Response headers not forwarded to the browser:
/// - `set-cookie`: the app-view cookie stays in the shell's jar; the browser
///   must never receive it (that is the whole point).
/// - `x-frame-options` / `content-security-policy`: the upstream forbids framing
///   from unknown origins; the shell is a trusted embedder and the preview
///   iframe is sandboxed, so these are dropped to allow rendering.
/// - hop-by-hop / framing headers reqwest already resolved.
fn should_drop_response_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "set-cookie"
            | "x-frame-options"
            | "content-security-policy"
            | "content-security-policy-report-only"
            | "connection"
            | "transfer-encoding"
            | "content-length"
            | "keep-alive"
    )
}

/// Remove a single query parameter from a URL string, preserving the rest.
fn strip_query_param(url: &str, param: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map(|(k, _)| k).unwrap_or(pair);
            key != param
        })
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

fn error_response(status: StatusCode) -> Response<Cow<'static, [u8]>> {
    let body = format!("{{\"error\":\"app_view_proxy_error\",\"status\":{}}}", status.as_u16());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Cow::Owned(body.into_bytes()))
        .expect("build proxy error response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_the_named_query_param() {
        assert_eq!(
            strip_query_param("https://h.app.ato.run/?app_view_token=abc", "app_view_token"),
            "https://h.app.ato.run/"
        );
        assert_eq!(
            strip_query_param(
                "https://h.app.ato.run/p?a=1&app_view_token=abc&b=2",
                "app_view_token"
            ),
            "https://h.app.ato.run/p?a=1&b=2"
        );
        // No query / param absent → unchanged.
        assert_eq!(
            strip_query_param("https://h.app.ato.run/p", "app_view_token"),
            "https://h.app.ato.run/p"
        );
        assert_eq!(
            strip_query_param("https://h.app.ato.run/p?a=1", "app_view_token"),
            "https://h.app.ato.run/p?a=1"
        );
    }

    #[test]
    fn drops_shell_owned_and_framing_headers() {
        assert!(should_drop_request_header("Cookie"));
        assert!(should_drop_request_header("host"));
        assert!(should_drop_request_header("Origin"));
        assert!(!should_drop_request_header("accept"));
        assert!(should_drop_response_header("Set-Cookie"));
        assert!(should_drop_response_header("X-Frame-Options"));
        assert!(should_drop_response_header("content-security-policy"));
        assert!(!should_drop_response_header("content-type"));
    }
}
