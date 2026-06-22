//! HTTP / WebSocket ingress reverse proxy.
//!
//! `proxy_request` is the single dispatch point.  It:
//! 1. Detects WebSocket upgrade (`Upgrade: websocket` + `Connection: Upgrade`)
//! 2. Routes to either `proxy_websocket` (full-duplex TCP relay) or
//!    `proxy_http` (streaming HTTP/1.1 pass-through)
//!
//! # Streaming contract
//!
//! `proxy_http` does **not** buffer the upstream response body.  It uses
//! `http_body_util::BodyExt::frame` to stream frames from upstream to
//! the client, supporting SSE (`text/event-stream`), long-poll, and
//! chunked transfer without end-to-end buffering.
//!
//! # WebSocket upgrade (hyper v1)
//!
//! The upgrade dance has two sides:
//! - Client side: `hyper::upgrade::on(&mut req)` extracts the upgrade
//!   future from the incoming request *before* the request body is
//!   consumed.  hyper drives the `101 Switching Protocols` response and
//!   turns the connection into a raw I/O stream.
//! - Upstream side: `hyper::upgrade::on(&mut upstream_resp)` does the
//!   same for the outbound connection.
//!   Both futures are joined and `tokio::io::copy_bidirectional` relays
//!   bytes forever until either side closes.
//!
//! The server `Connection` **must** be wrapped with `.with_upgrades()` —
//! see `run_ingress_listener` in `mod.rs`.

use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use http::{Request, Response, StatusCode, Uri, Version};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::sync::RwLock;
use url::Url;

use super::hop_by_hop::{scrub_hop_by_hop, scrub_hop_by_hop_ws};

type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;

fn empty_body() -> BoxBody {
    Empty::<Bytes>::new()
        .map_err(|_| unreachable!("infallible"))
        .boxed()
}

fn full_body(bytes: impl Into<Bytes>) -> BoxBody {
    Full::new(bytes.into())
        .map_err(|_| unreachable!("infallible"))
        .boxed()
}

fn error_response(status: StatusCode, body: &'static str) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .body(full_body(body))
        .expect("static response never fails")
}

/// 503 response with an HTML auto-refresh page shown while the upstream service
/// is still starting up (e.g. Docker container boot, Python/JVM initialisation).
/// The browser re-requests the page every 3 seconds until the service is ready.
fn service_starting_response() -> Response<BoxBody> {
    const HTML: &str = concat!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">",
        "<meta http-equiv=\"refresh\" content=\"3\">",
        "<title>Starting\u{2026}</title>",
        "<style>",
        "body{font-family:system-ui,sans-serif;display:flex;align-items:center;",
        "justify-content:center;height:100vh;margin:0;background:#f5f5f5}",
        ".box{text-align:center;color:#333}",
        ".spinner{width:40px;height:40px;border:3px solid #ddd;border-top-color:#555;",
        "border-radius:50%;animation:spin .8s linear infinite;margin:0 auto 16px}",
        "@keyframes spin{to{transform:rotate(360deg)}}",
        "</style></head><body>",
        "<div class=\"box\">",
        "<div class=\"spinner\"></div>",
        "<p>Service is starting\u{2026}</p>",
        "<small>This page will refresh automatically.</small>",
        "</div></body></html>",
    );
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("content-type", "text/html; charset=utf-8")
        .header("retry-after", "3")
        .body(full_body(HTML))
        .expect("static response never fails")
}

fn is_websocket_upgrade(req: &Request<Incoming>) -> bool {
    let is_upgrade_header = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    let has_connection_upgrade = req
        .headers()
        .get("connection")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false);

    is_upgrade_header && has_connection_upgrade
}

/// Build a `Uri` by replacing the scheme+authority with the upstream
/// base URL, preserving the original path+query.
fn rewrite_uri(upstream_base: &Url, original: &Uri) -> Result<Uri, http::Error> {
    let mut parts = http::uri::Parts::default();

    let scheme = upstream_base.scheme();
    parts.scheme = Some(scheme.parse().unwrap_or(http::uri::Scheme::HTTP));

    let host_and_port = match upstream_base.port() {
        Some(p) => format!("{}:{}", upstream_base.host_str().unwrap_or("127.0.0.1"), p),
        None => upstream_base.host_str().unwrap_or("127.0.0.1").to_string(),
    };
    parts.authority =
        Some(host_and_port.parse().map_err(|_| {
            http::Error::from("invalid authority".parse::<StatusCode>().unwrap_err())
        })?);

    let pq = original
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    parts.path_and_query = Some(
        pq.parse()
            .map_err(|_| http::Error::from("invalid pq".parse::<StatusCode>().unwrap_err()))?,
    );

    Uri::from_parts(parts).map_err(Into::into)
}

/// Add `X-Forwarded-For`, `X-Forwarded-Host`, and `X-Forwarded-Proto`
/// headers. If `X-Forwarded-For` is already present, appends to it.
fn add_forwarded_headers(headers: &mut http::HeaderMap, client_addr: SocketAddr, scheme: &str) {
    // X-Forwarded-For
    let client_ip = client_addr.ip().to_string();
    let xff = match headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        Some(existing) => format!("{existing}, {client_ip}"),
        None => client_ip,
    };
    if let Ok(v) = http::HeaderValue::from_str(&xff) {
        headers.insert("x-forwarded-for", v);
    }
    // X-Forwarded-Host (original Host header before rewrite)
    if let Some(host) = headers.get("host").cloned() {
        headers.insert("x-forwarded-host", host);
    }
    // X-Forwarded-Proto
    if let Ok(v) = http::HeaderValue::from_str(scheme) {
        headers.insert("x-forwarded-proto", v);
    }
}

/// Main dispatch: route to WebSocket or HTTP proxy.
pub async fn proxy_request(
    req: Request<Incoming>,
    upstream: Arc<RwLock<Url>>,
    client_addr: SocketAddr,
) -> Result<Response<BoxBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let upstream_for_log = Arc::clone(&upstream);

    if is_websocket_upgrade(&req) {
        match proxy_websocket(req, upstream, client_addr).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                let upstream_url = upstream_for_log.read().await.to_string();
                tracing::warn!(
                    client = %client_addr,
                    method = %method,
                    path = %path,
                    upstream = %upstream_url,
                    error = %e,
                    "websocket proxy error"
                );
                Ok(error_response(
                    StatusCode::BAD_GATEWAY,
                    "WebSocket proxy error",
                ))
            }
        }
    } else {
        match proxy_http(req, upstream, client_addr).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                let upstream_url = upstream_for_log.read().await.to_string();
                tracing::warn!(
                    client = %client_addr,
                    method = %method,
                    path = %path,
                    upstream = %upstream_url,
                    error = %e,
                    "http proxy error"
                );
                Ok(error_response(StatusCode::BAD_GATEWAY, "HTTP proxy error"))
            }
        }
    }
}

// ── HTTP streaming proxy ───────────────────────────────────────────────────

async fn proxy_http(
    mut req: Request<Incoming>,
    upstream: Arc<RwLock<Url>>,
    client_addr: SocketAddr,
) -> anyhow::Result<Response<BoxBody>> {
    let upstream_url = upstream.read().await.clone();
    let scheme = upstream_url.scheme().to_string();

    // Rewrite URI: extract path+query from the rewritten absolute URI, then
    // use origin-form (path+query only) for the upstream request.
    // hyper serialises an absolute-URI (scheme+authority+path) as
    // `GET http://host/path HTTP/1.1`, which is the HTTP proxy request-target
    // format.  ASGI servers such as uvicorn/FastAPI treat the full URL as the
    // route path and return 404.  A direct reverse-proxy connection must use
    // origin-form: `GET /path HTTP/1.1`.
    let rewritten = rewrite_uri(&upstream_url, req.uri())
        .map_err(|e| anyhow::anyhow!("URI rewrite failed: {e}"))?;
    let pq = rewritten
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    let origin_uri = Uri::builder()
        .path_and_query(pq)
        .build()
        .map_err(|e| anyhow::anyhow!("origin-form URI: {e}"))?;
    *req.uri_mut() = origin_uri;

    // Rewrite Host header.
    let upstream_host = match upstream_url.port() {
        Some(p) => format!("{}:{}", upstream_url.host_str().unwrap_or("127.0.0.1"), p),
        None => upstream_url.host_str().unwrap_or("127.0.0.1").to_string(),
    };
    if let Ok(v) = http::HeaderValue::from_str(&upstream_host) {
        req.headers_mut().insert("host", v);
    }

    // Forwarded headers before scrubbing (so original Host survives via xfh).
    add_forwarded_headers(req.headers_mut(), client_addr, &scheme);

    // Scrub hop-by-hop headers.
    scrub_hop_by_hop(req.headers_mut());

    // Force HTTP/1.1 for upstream connection (we don't support HTTP/2 upstream
    // in this slice).
    *req.version_mut() = Version::HTTP_11;

    // Establish upstream connection.  Retry briefly on ConnectionRefused to
    // absorb the startup race where the Docker port forwarder allocates the
    // host port before it starts accepting connections (typically <200 ms on
    // macOS with com.docker.proxy).
    let addr = format!(
        "{}:{}",
        upstream_url.host_str().unwrap_or("127.0.0.1"),
        upstream_url.port_or_known_default().unwrap_or(80)
    );
    const CONNECT_RETRIES: u32 = 3;
    const CONNECT_RETRY_MS: u64 = 150;
    let stream = {
        let mut _last_err: Option<std::io::Error> = None;
        let mut connected = None;
        for attempt in 0..=CONNECT_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(CONNECT_RETRY_MS)).await;
            }
            match tokio::net::TcpStream::connect(&addr).await {
                Ok(s) => {
                    connected = Some(s);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    tracing::debug!(attempt, addr = %addr, "upstream connect refused, retrying");
                    _last_err = Some(e);
                }
                Err(e) => return Err(anyhow::anyhow!("upstream connect failed: {e}")),
            }
        }
        match connected {
            Some(s) => s,
            None => {
                // All retries exhausted with ConnectionRefused — upstream is still
                // initialising (e.g. Docker container boot, slow Python startup).
                // Return the "starting…" page so the browser auto-retries instead
                // of showing a permanent "HTTP proxy error".
                tracing::debug!(
                    addr = %addr,
                    retries = CONNECT_RETRIES,
                    "upstream still not ready after retries, returning service-starting page"
                );
                return Ok(service_starting_response());
            }
        }
    };

    let io = TokioIo::new(stream);
    let (mut sender, conn) = match hyper::client::conn::http1::Builder::new()
        .handshake(io)
        .await
    {
        Ok(pair) => pair,
        Err(e) => {
            // TCP connection was accepted but immediately dropped — upstream is
            // still initialising (common when Docker port forwarder accepts a
            // connection before Open WebUI / the container app is ready).
            tracing::debug!(addr = %addr, error = %e, "upstream handshake failed, returning service-starting page");
            return Ok(service_starting_response());
        }
    };

    // Drive the upstream connection in a detached task.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!("upstream HTTP connection error: {e}");
        }
    });

    let upstream_resp = match sender.send_request(req).await {
        Ok(resp) => resp,
        Err(e) => {
            // Connection was dropped while sending the request — the upstream
            // accepted the TCP connection but reset it before completing the
            // HTTP exchange.  This happens when Docker proxy accepts connections
            // before the container process is ready to serve them.
            tracing::debug!(addr = %addr, error = %e, "upstream dropped connection, returning service-starting page");
            return Ok(service_starting_response());
        }
    };

    let (mut parts, body) = upstream_resp.into_parts();

    // Scrub hop-by-hop headers from the upstream response.
    scrub_hop_by_hop(&mut parts.headers);

    // Stream body frames without buffering — Incoming already implements
    // Body<Data=Bytes, Error=hyper::Error>, so .boxed() works directly.
    Ok(Response::from_parts(parts, body.boxed()))
}

// ── WebSocket upgrade relay ────────────────────────────────────────────────

async fn proxy_websocket(
    mut req: Request<Incoming>,
    upstream: Arc<RwLock<Url>>,
    client_addr: SocketAddr,
) -> anyhow::Result<Response<BoxBody>> {
    let upstream_url = upstream.read().await.clone();

    // Extract the client-side upgrade future BEFORE consuming the request.
    let client_upgrade = hyper::upgrade::on(&mut req);

    // Rewrite to origin-form URI for the upstream WebSocket upgrade request
    // (same reason as proxy_http: direct connections use origin-form).
    let rewritten = rewrite_uri(&upstream_url, req.uri())
        .map_err(|e| anyhow::anyhow!("URI rewrite failed: {e}"))?;
    let pq = rewritten
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| http::uri::PathAndQuery::from_static("/"));
    let origin_uri = Uri::builder()
        .path_and_query(pq)
        .build()
        .map_err(|e| anyhow::anyhow!("origin-form URI: {e}"))?;
    *req.uri_mut() = origin_uri;

    let upstream_host = match upstream_url.port() {
        Some(p) => format!("{}:{}", upstream_url.host_str().unwrap_or("127.0.0.1"), p),
        None => upstream_url.host_str().unwrap_or("127.0.0.1").to_string(),
    };
    if let Ok(v) = http::HeaderValue::from_str(&upstream_host) {
        req.headers_mut().insert("host", v);
    }

    let scheme = upstream_url.scheme().to_string();
    add_forwarded_headers(req.headers_mut(), client_addr, &scheme);

    // Scrub hop-by-hop but KEEP Connection/Upgrade for WebSocket.
    scrub_hop_by_hop_ws(req.headers_mut());

    *req.version_mut() = Version::HTTP_11;

    // Connect to upstream.
    let stream = tokio::net::TcpStream::connect(format!(
        "{}:{}",
        upstream_url.host_str().unwrap_or("127.0.0.1"),
        upstream_url.port_or_known_default().unwrap_or(80)
    ))
    .await
    .map_err(|e| anyhow::anyhow!("upstream connect failed: {e}"))?;

    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake(io)
        .await
        .map_err(|e| anyhow::anyhow!("upstream WS handshake failed: {e}"))?;

    // Drive upstream connection with upgrade support.
    tokio::spawn(async move {
        if let Err(e) = conn.with_upgrades().await {
            tracing::debug!("upstream WS connection error: {e}");
        }
    });

    let mut upstream_resp = sender
        .send_request(req)
        .await
        .map_err(|e| anyhow::anyhow!("upstream WS request failed: {e}"))?;

    if upstream_resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        return Err(anyhow::anyhow!(
            "upstream did not upgrade: {}",
            upstream_resp.status()
        ));
    }

    let upstream_upgrade = hyper::upgrade::on(&mut upstream_resp);

    // Relay both directions in a detached task.
    tokio::spawn(async move {
        match tokio::join!(client_upgrade, upstream_upgrade) {
            (Ok(client_io), Ok(upstream_io)) => {
                let mut client_io = TokioIo::new(client_io);
                let mut upstream_io = TokioIo::new(upstream_io);
                if let Err(e) =
                    tokio::io::copy_bidirectional(&mut client_io, &mut upstream_io).await
                {
                    tracing::debug!("WebSocket relay ended: {e}");
                }
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!("WebSocket upgrade failed: {e}");
            }
        }
    });

    // Return 101 to hyper — it drives the client-side upgrade.
    let (parts, _) = upstream_resp.into_parts();
    Ok(Response::from_parts(parts, empty_body()))
}
