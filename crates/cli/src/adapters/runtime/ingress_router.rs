use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::any;
use base64::Engine;
use rand::RngCore;

use tokio::net::TcpListener;
use tokio::sync::oneshot;

use capsule::types::IngressConfig;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

// ── Public API ───────────────────────────────────────────────────────────────

/// Start a local path ingress router.
///
/// The router listens on `127.0.0.1:<port>` (or auto-allocates if port is 0)
/// and proxies requests to upstream service host ports according to the
/// compiled route table.
pub async fn start_ingress_router(
    token: String,
    port: u16,
    route_entries: Vec<RouteEntry>,
) -> Result<RouterHandle, anyhow::Error> {
    let mut route_map = BTreeMap::new();
    for entry in route_entries {
        route_map.insert(entry.alias.clone(), entry);
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    let actual_port = listener.local_addr()?.port();

    let state = Arc::new(RouterState {
        token,
        router_port: actual_port,
        routes: route_map,
    });

    let app = Router::new()
        .route("/i/*path", any(catch_all_handler))
        .with_state(state);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let join_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            })
            .await
            .ok();
    });

    Ok(RouterHandle {
        join_handle: Some(join_handle),
        shutdown_tx: Some(shutdown_tx),
        port: actual_port,
    })
}

/// Handle that controls a running ingress router.
pub struct RouterHandle {
    join_handle: Option<tokio::task::JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    pub port: u16,
}

impl RouterHandle {
    /// Stop the router gracefully.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for RouterHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouteEntry {
    pub alias: String,
    pub target_host: String,
    pub target_port: u16,
    pub strip_prefix: bool,
    pub upstream_path_prefix: Option<String>,
    pub root: bool,
    #[allow(dead_code)]
    pub listed: bool,
}

/// Compiled route table indexed by alias.
#[derive(Debug, Clone)]
struct RouterState {
    token: String,
    router_port: u16,
    routes: BTreeMap<String, RouteEntry>,
}

// ── Route table construction ─────────────────────────────────────────────────

/// Build a route table from an [`IngressConfig`] and a map of service name
/// (target label) to allocated host port.
///
/// Allocates and returns a token for the session. Errors are user-facing
/// manifest issues such as missing service mappings.
pub fn build_route_table(
    ingress: &IngressConfig,
    service_host_ports: &BTreeMap<String, u16>,
) -> Result<Vec<RouteEntry>, String> {
    let mut entries = Vec::new();
    for (route_name, route) in &ingress.routes {
        let host_port = service_host_ports
            .get(&route.target)
            .copied()
            .ok_or_else(|| {
                format!(
                "ingress route '{}' targets '{}' but no host port was allocated for that service",
                route_name, route.target
            )
            })?;

        if !route.strip_prefix {
            return Err(format!(
                "ingress route '{}' has strip_prefix=false which is unsupported in v1",
                route_name
            ));
        }

        let alias = if route.root {
            String::new()
        } else {
            route.alias.clone().unwrap_or_else(|| route_name.clone())
        };

        entries.push(RouteEntry {
            alias,
            target_host: "127.0.0.1".to_string(),
            target_port: host_port,
            strip_prefix: route.strip_prefix,
            upstream_path_prefix: route.upstream_path_prefix.clone(),
            root: route.root,
            listed: route.listed,
        });
    }
    Ok(entries)
}

/// Generate a high-entropy session token using random bytes + base64url.
pub fn generate_session_token() -> String {
    let mut rng = rand::thread_rng();
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    engine.encode(bytes)
}

// ── Axum handlers ────────────────────────────────────────────────────────────

async fn catch_all_handler(
    State(state): State<Arc<RouterState>>,
    Path(path): Path<String>,
    req: Request<Body>,
) -> Response {
    // ── Host header validation ────────────────────────────────────────────
    // The router is bound to 127.0.0.1:<router_port>.  Accept only
    // 127.0.0.1:<port> or localhost:<port> in the Host to prevent
    // external rebinding or Host-injection attacks.
    // Check both the Host header and the URI authority (hyper may strip the
    // header from headers() and store it in the URI).
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().host());
    if let Some(host) = host {
        // Require <host>:<port> exactly; missing or invalid port is rejected.
        let Some((host_part, port_part)) = host.rsplit_once(':') else {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("Invalid Host header"))
                .unwrap();
        };
        let Ok(port) = port_part.parse::<u16>() else {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("Invalid Host header"))
                .unwrap();
        };
        let host_ok = host_part == "127.0.0.1" || host_part == "localhost";
        if !host_ok || port != state.router_port {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("Invalid Host header"))
                .unwrap();
        }
    }

    // path is the full path after /i/
    // Example incoming: /i/TOKEN/    -> path = "TOKEN/"
    // Example incoming: /i/TOKEN/web/foo -> path = "TOKEN/web/foo"
    // Extract token and remaining segments

    let (token, rest) = match split_path(&path) {
        Some(parts) => parts,
        None => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("Not found"))
                .unwrap();
        }
    };

    if token != state.token {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not found"))
            .unwrap();
    }

    // ── Trailing-slash redirects ──────────────────────────────────────────
    // /i/<token>         → 308 /i/<token>/
    // /i/<token>/<alias> → 308 /i/<token>/<alias>/
    // Preserve query string.  Unknown aliases fall through to 404.

    let has_trailing_slash = path.ends_with('/');

    let needs_trailing_slash = |r: &str| -> bool { !r.is_empty() && !r.contains('/') };

    if rest.is_empty() && !has_trailing_slash {
        let location = format!("/i/{}/", state.token);
        let query = req
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        return redirect_308(&format!("{location}{query}"));
    }

    if !has_trailing_slash && needs_trailing_slash(&rest) && state.routes.contains_key(&rest) {
        let location = format!("/i/{}/{}/", state.token, rest);
        let query = req
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        return redirect_308(&format!("{location}{query}"));
    }

    let query_string = req.uri().query().map(|q| q.to_string());

    let (method, headers, body_bytes) = match extract_request_parts(req).await {
        Ok(parts) => parts,
        Err(resp) => return resp,
    };

    handle_route(
        &state.routes,
        &rest,
        method,
        headers,
        body_bytes,
        query_string,
    )
    .await
}

fn redirect_308(location: &str) -> Response {
    Response::builder()
        .status(StatusCode::PERMANENT_REDIRECT)
        .header("Location", location)
        .body(Body::empty())
        .unwrap()
}

// ── Route resolution and proxy ───────────────────────────────────────────────

async fn handle_route(
    routes: &BTreeMap<String, RouteEntry>,
    rest: &str,
    method: Method,
    headers: HeaderMap,
    body_bytes: Vec<u8>,
    query: Option<String>,
) -> Response {
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();

    let (route_entry, upstream_path) = if segments.is_empty() {
        // No alias = root route
        let entry = match routes.values().find(|e| e.root) {
            Some(e) => e,
            None => {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("Not found"))
                    .unwrap();
            }
        };
        let upstream_path = rewrite_path("", entry);
        (entry, upstream_path)
    } else {
        let alias = segments[0];
        let remaining = if segments.len() > 1 {
            "/".to_string() + &segments[1..].join("/")
        } else {
            String::new()
        };

        let entry = match routes.get(alias) {
            Some(e) => e,
            None => {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("Not found"))
                    .unwrap();
            }
        };
        let upstream_path = rewrite_path(&remaining, entry);
        (entry, upstream_path)
    };

    let upstream_url = format!(
        "http://{}:{}{}",
        route_entry.target_host, route_entry.target_port, upstream_path
    );

    let upstream_url = if let Some(q) = &query {
        format!("{}?{}", upstream_url, q)
    } else {
        upstream_url
    };

    proxy_request(&method, &upstream_url, &headers, &body_bytes).await
}

fn rewrite_path(remaining_path: &str, entry: &RouteEntry) -> String {
    let path = if remaining_path.is_empty() || remaining_path == "/" {
        String::new()
    } else {
        remaining_path.to_string()
    };

    if entry.strip_prefix {
        if let Some(prefix) = &entry.upstream_path_prefix {
            format!("{}{}", prefix, path)
        } else if path.is_empty() {
            "/".to_string()
        } else {
            path
        }
    } else {
        unreachable!("strip_prefix=false should not reach this code path in v1")
    }
}

async fn proxy_request(
    method: &Method,
    upstream_url: &str,
    headers: &HeaderMap,
    body_bytes: &[u8],
) -> Response {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from(format!("Failed to create HTTP client: {e}")))
                .unwrap();
        }
    };

    let filtered_headers = filter_headers(headers);

    let upstream_req = client
        .request(method.clone(), upstream_url)
        .headers(filtered_headers);

    let upstream_resp = if body_bytes.is_empty() {
        upstream_req.send().await
    } else {
        let body_vec = body_bytes.to_vec();
        upstream_req.body(body_vec).send().await
    };

    match upstream_resp {
        Ok(resp) => {
            let status = resp.status();
            let resp_headers = filter_resp_headers(resp.headers());
            let resp_bytes = resp.bytes().await.unwrap_or_default();
            let mut builder = Response::builder().status(status);
            for (key, value) in resp_headers.iter() {
                builder = builder.header(key, value);
            }
            // NOTE: Response body is currently buffered in memory before being
            // forwarded.  For SSE / chunked streaming the router should use
            // resp.bytes_stream() + a streaming body adapter, but axum 0.7
            // (http-body 1.0) and hyper 0.14 (http-body 0.4) use incompatible
            // http-body versions, making direct passthrough non-trivial.
            // Streaming passthrough is tracked as follow-up work.
            builder.body(Body::from(resp_bytes)).unwrap()
        }
        Err(e) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("Upstream proxy error: {e}")))
            .unwrap(),
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Split the path into (token, rest) where rest is the path after token
fn split_path(path: &str) -> Option<(String, String)> {
    let path = path.trim_start_matches('/');
    let slash_pos = path.find('/').unwrap_or(path.len());
    let token = path[..slash_pos].to_string();
    let rest = if slash_pos < path.len() {
        path[slash_pos + 1..].to_string()
    } else {
        String::new()
    };
    if token.is_empty() {
        None
    } else {
        Some((token, rest))
    }
}

/// Extract method, headers, and body bytes from the incoming request.
async fn extract_request_parts(
    req: Request<Body>,
) -> Result<(Method, HeaderMap, Vec<u8>), Response> {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY_SIZE).await {
        Ok(b) => b.to_vec(),
        Err(_) => {
            return Err(Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .body(Body::from("Request body too large"))
                .unwrap());
        }
    };
    Ok((parts.method, parts.headers, bytes))
}

/// Filter out hop-by-hop headers for upstream request.
fn filter_headers(headers: &HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    for (key, value) in headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if !HOP_BY_HOP.contains(&key_str.as_str()) {
            filtered.insert(key.clone(), value.clone());
        }
    }
    // Remove Host so reqwest sets it from the upstream URL.
    let _ = filtered.remove("host");
    filtered
}

/// Filter hop-by-hop headers from upstream response.
fn filter_resp_headers(headers: &HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::new();
    for (key, value) in headers.iter() {
        let key_str = key.as_str().to_lowercase();
        if !HOP_BY_HOP.contains(&key_str.as_str()) {
            filtered.insert(key.clone(), value.clone());
        }
    }
    filtered
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::types::IngressRoute;
    use std::collections::BTreeMap;

    fn sample_ingress_config() -> IngressConfig {
        IngressConfig {
            mode: capsule::types::IngressMode::Path,
            routes: BTreeMap::from([
                (
                    "web".to_string(),
                    IngressRoute {
                        target: "web-target".to_string(),
                        port: 80,
                        listed: true,
                        alias: None,
                        strip_prefix: true,
                        upstream_path_prefix: None,
                        root: true,
                    },
                ),
                (
                    "api".to_string(),
                    IngressRoute {
                        target: "api-target".to_string(),
                        port: 5001,
                        listed: false,
                        alias: Some("api".to_string()),
                        strip_prefix: true,
                        upstream_path_prefix: Some("/api".to_string()),
                        root: false,
                    },
                ),
            ]),
            env_inject: BTreeMap::new(),
        }
    }

    #[test]
    fn test_build_route_table_from_manifest() {
        let ingress = sample_ingress_config();
        let mut ports = BTreeMap::new();
        ports.insert("web-target".to_string(), 38001u16);
        ports.insert("api-target".to_string(), 38002u16);

        let entries = build_route_table(&ingress, &ports).unwrap();
        assert_eq!(entries.len(), 2);

        let web = entries.iter().find(|e| e.root).unwrap();
        assert_eq!(web.target_host, "127.0.0.1");
        assert_eq!(web.target_port, 38001);
        assert_eq!(web.alias, "");
        assert!(web.strip_prefix);
        assert!(web.listed);

        let api = entries.iter().find(|e| e.alias == "api").unwrap();
        assert_eq!(api.target_port, 38002);
        assert!(api.strip_prefix);
        assert_eq!(api.upstream_path_prefix.as_deref(), Some("/api"));
        assert!(!api.listed);
    }

    #[test]
    fn test_rejects_unknown_route() {
        let ingress = sample_ingress_config();
        let mut ports = BTreeMap::new();
        ports.insert("web-target".to_string(), 38001u16);

        let err = build_route_table(&ingress, &ports).unwrap_err();
        assert!(err.contains("api-target"));
    }

    #[test]
    fn test_rejects_strip_prefix_false() {
        let mut ingress = sample_ingress_config();
        ingress.routes.get_mut("api").unwrap().strip_prefix = false;
        let mut ports = BTreeMap::new();
        ports.insert("web-target".to_string(), 38001u16);
        ports.insert("api-target".to_string(), 38002u16);

        let err = build_route_table(&ingress, &ports).unwrap_err();
        assert!(err.contains("strip_prefix=false"), "error: {err}");
        assert!(err.contains("api"), "error: {err}");
    }

    #[test]
    fn test_generate_session_token() {
        let t1 = generate_session_token();
        let t2 = generate_session_token();
        assert_ne!(t1, t2);
        assert!(!t1.is_empty());
        assert!(!t1.contains('/'));
    }

    #[test]
    fn test_rewrite_root_route_path() {
        let entry = RouteEntry {
            alias: String::new(),
            target_host: "127.0.0.1".to_string(),
            target_port: 8080,
            strip_prefix: true,
            upstream_path_prefix: None,
            root: true,
            listed: true,
        };
        assert_eq!(rewrite_path("/foo/bar", &entry), "/foo/bar");
        assert_eq!(rewrite_path("", &entry), "/");
    }

    #[test]
    fn test_rewrite_alias_route_path() {
        let entry = RouteEntry {
            alias: "api".to_string(),
            target_host: "127.0.0.1".to_string(),
            target_port: 9000,
            strip_prefix: true,
            upstream_path_prefix: None,
            root: false,
            listed: false,
        };
        assert_eq!(rewrite_path("/users", &entry), "/users");
    }

    #[test]
    fn test_rewrite_with_upstream_path_prefix() {
        let entry = RouteEntry {
            alias: "api".to_string(),
            target_host: "127.0.0.1".to_string(),
            target_port: 9000,
            strip_prefix: true,
            upstream_path_prefix: Some("/api".to_string()),
            root: false,
            listed: false,
        };
        assert_eq!(rewrite_path("/foo", &entry), "/api/foo");
        assert_eq!(rewrite_path("", &entry), "/api");
    }

    #[test]
    fn test_split_path() {
        assert_eq!(
            split_path("TOKEN/web/foo"),
            Some(("TOKEN".to_string(), "web/foo".to_string()))
        );
        assert_eq!(
            split_path("TOKEN/"),
            Some(("TOKEN".to_string(), String::new()))
        );
        assert_eq!(split_path(""), None);
        assert_eq!(split_path("/"), None);
    }

    #[test]
    fn test_split_path_handles_leading_slash() {
        assert_eq!(
            split_path("/TOKEN/web"),
            Some(("TOKEN".to_string(), "web".to_string()))
        );
    }

    // ── Host header validation ────────────────────────────────────────────

    #[tokio::test]
    async fn test_invalid_host_rejected() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });

        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "evil.com:9999")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_valid_host_127_0_0_1_accepted() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });

        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        // Should not be BAD_REQUEST (will be NOT_FOUND because no routes, but
        // that means host validation passed)
        assert_ne!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_valid_host_localhost_accepted() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });

        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "localhost:9999")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_ne!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_rebinding_host_rejected() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });

        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "127.0.0.1:8888")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_absolute_form_uri_mismatched_port_rejected() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });

        let req = Request::builder()
            .uri("http://127.0.0.1:8888/i/test-token/")
            .header("host", "127.0.0.1:8888")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_host_without_port_rejected() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });
        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "127.0.0.1")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_localhost_without_port_rejected() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });
        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "localhost")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_host_with_non_numeric_port_rejected() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });
        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "127.0.0.1:notaport")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_host_with_empty_port_rejected() {
        let state = Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes: BTreeMap::new(),
        });
        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "127.0.0.1:")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── Trailing-slash redirects ──────────────────────────────────────────

    fn test_state() -> Arc<RouterState> {
        let mut routes = BTreeMap::new();
        routes.insert(
            "".to_string(),
            RouteEntry {
                alias: String::new(),
                target_host: "127.0.0.1".to_string(),
                target_port: 18080,
                strip_prefix: true,
                upstream_path_prefix: None,
                root: true,
                listed: true,
            },
        );
        routes.insert(
            "api".to_string(),
            RouteEntry {
                alias: "api".to_string(),
                target_host: "127.0.0.1".to_string(),
                target_port: 18081,
                strip_prefix: true,
                upstream_path_prefix: Some("/api".to_string()),
                root: false,
                listed: true,
            },
        );
        Arc::new(RouterState {
            token: "test-token".to_string(),
            router_port: 9999,
            routes,
        })
    }

    #[tokio::test]
    async fn test_redirects_root_without_slash() {
        let state = test_state();
        let req = Request::builder()
            .uri("/i/test-token")
            .header("host", "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(resp.headers().get("location").unwrap(), "/i/test-token/");
    }

    #[tokio::test]
    async fn test_redirects_alias_without_slash() {
        let state = test_state();
        let req = Request::builder()
            .uri("/i/test-token/api")
            .header("host", "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/api".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/i/test-token/api/"
        );
    }

    #[tokio::test]
    async fn test_redirect_preserves_query() {
        let state = test_state();
        let req = Request::builder()
            .uri("/i/test-token?foo=bar&baz=1")
            .header("host", "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token".to_string()), req).await;
        assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/i/test-token/?foo=bar&baz=1"
        );
    }

    #[tokio::test]
    async fn test_unknown_alias_does_not_redirect() {
        let state = test_state();
        let req = Request::builder()
            .uri("/i/test-token/unknown")
            .header("host", "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        let resp =
            catch_all_handler(State(state), Path("/test-token/unknown".to_string()), req).await;
        // Unknown alias should 404, not redirect
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_root_with_slash_does_not_redirect() {
        let state = test_state();
        let req = Request::builder()
            .uri("/i/test-token/")
            .header("host", "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        let resp = catch_all_handler(State(state), Path("/test-token/".to_string()), req).await;
        assert_ne!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    }

    #[tokio::test]
    async fn test_alias_sub_path_does_not_redirect() {
        let state = test_state();
        let req = Request::builder()
            .uri("/i/test-token/api/users")
            .header("host", "127.0.0.1:9999")
            .body(Body::empty())
            .unwrap();
        let resp =
            catch_all_handler(State(state), Path("/test-token/api/users".to_string()), req).await;
        assert_ne!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    }
}
