use anyhow::{Context, Result};
use axum::body::{to_bytes, Body};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::any;
use axum::Router;
use chrono::Utc;
use rcgen::generate_simple_self_signed;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

const MAX_PROXY_BODY_BYTES: usize = 16 * 1024 * 1024;
const TLS_METADATA_FILE_NAME: &str = "tls-bootstrap.json";
const TLS_CERT_FILE_NAME: &str = "server-cert.pem";
const TLS_KEY_FILE_NAME: &str = "server-key.pem";

#[derive(Debug, Clone)]
pub struct IngressProxyConfig {
    pub binding_id: String,
    pub endpoint_locator: String,
    pub upstream_locator: String,
    pub tls_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngressTlsBootstrapRecord {
    pub binding_id: String,
    pub endpoint_host: String,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub system_trust_installed: bool,
    pub consented_at: String,
    pub updated_at: String,
}

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    upstream_base: reqwest::Url,
}

pub async fn serve(config: IngressProxyConfig) -> Result<()> {
    let endpoint = reqwest::Url::parse(&config.endpoint_locator)
        .with_context(|| format!("invalid endpoint locator: {}", config.endpoint_locator))?;
    let listen_addr = endpoint_listen_addr(&endpoint)?;
    let state = ProxyState {
        client: reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .context("failed to build reverse proxy HTTP client")?,
        upstream_base: reqwest::Url::parse(&config.upstream_locator)
            .with_context(|| format!("invalid upstream locator: {}", config.upstream_locator))?,
    };
    let app = build_proxy_router(state);

    if config.tls_mode.eq_ignore_ascii_case("explicit") {
        let tls = load_tls_bootstrap(&config.binding_id)?.ok_or_else(|| {
            anyhow::anyhow!(
                "ingress TLS bootstrap required for binding '{}'. Run `ato binding bootstrap-tls --binding {}` first.",
                config.binding_id,
                config.binding_id
            )
        })?;
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            tls.cert_path.clone(),
            tls.key_path.clone(),
        )
        .await
        .with_context(|| {
            format!(
                "failed to load ingress TLS assets for binding '{}'",
                config.binding_id
            )
        })?;
        axum_server::bind_rustls(listen_addr, tls_config)
            .serve(app.into_make_service())
            .await
            .with_context(|| {
                format!(
                    "failed to serve HTTPS ingress binding '{}' on {}",
                    config.binding_id, listen_addr
                )
            })?;
        return Ok(());
    }

    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind ingress listener on {}", listen_addr))?;
    axum::serve(listener, app.into_make_service())
        .await
        .with_context(|| {
            format!(
                "failed to serve HTTP ingress binding '{}' on {}",
                config.binding_id, listen_addr
            )
        })?;
    Ok(())
}

pub fn bootstrap_tls(
    binding_id: &str,
    endpoint_host: &str,
    install_system_trust: bool,
    yes: bool,
) -> Result<IngressTlsBootstrapRecord> {
    let base_dir = capsule_core::config::config_dir()?.join("state");
    bootstrap_tls_in_dir(
        &base_dir,
        binding_id,
        endpoint_host,
        install_system_trust,
        yes,
    )
}

pub fn load_tls_bootstrap(binding_id: &str) -> Result<Option<IngressTlsBootstrapRecord>> {
    let base_dir = capsule_core::config::config_dir()?.join("state");
    load_tls_bootstrap_from_dir(&base_dir, binding_id)
}

pub fn load_tls_bootstrap_from_dir(
    base_dir: &Path,
    binding_id: &str,
) -> Result<Option<IngressTlsBootstrapRecord>> {
    let metadata_path = tls_metadata_path(base_dir, binding_id);
    if !metadata_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read {}", metadata_path.display()))?;
    let record = serde_json::from_str::<IngressTlsBootstrapRecord>(&raw)
        .with_context(|| format!("failed to parse {}", metadata_path.display()))?;
    Ok(Some(record))
}

pub fn bootstrap_tls_in_dir(
    base_dir: &Path,
    binding_id: &str,
    endpoint_host: &str,
    install_system_trust: bool,
    yes: bool,
) -> Result<IngressTlsBootstrapRecord> {
    let endpoint_host = endpoint_host.trim();
    if endpoint_host.is_empty() {
        anyhow::bail!("ingress TLS bootstrap requires a non-empty endpoint host");
    }

    require_tls_consent(binding_id, endpoint_host, install_system_trust, yes)?;

    let tls_dir = tls_dir(base_dir, binding_id);
    fs::create_dir_all(&tls_dir)
        .with_context(|| format!("failed to create {}", tls_dir.display()))?;

    let cert_path = tls_dir.join(TLS_CERT_FILE_NAME);
    let key_path = tls_dir.join(TLS_KEY_FILE_NAME);
    let cert = generate_simple_self_signed(vec![endpoint_host.to_string()]).with_context(|| {
        format!(
            "failed to generate self-signed ingress certificate for '{}'",
            endpoint_host
        )
    })?;
    fs::write(&cert_path, cert.cert.pem())
        .with_context(|| format!("failed to write {}", cert_path.display()))?;
    fs::write(&key_path, cert.key_pair.serialize_pem())
        .with_context(|| format!("failed to write {}", key_path.display()))?;

    let system_trust_installed = if install_system_trust {
        install_system_trust_for_cert(&cert_path)?;
        true
    } else {
        false
    };

    let now = Utc::now().to_rfc3339();
    let record = IngressTlsBootstrapRecord {
        binding_id: binding_id.to_string(),
        endpoint_host: endpoint_host.to_string(),
        cert_path,
        key_path,
        system_trust_installed,
        consented_at: now.clone(),
        updated_at: now,
    };
    let metadata_path = tls_metadata_path(base_dir, binding_id);
    fs::write(&metadata_path, serde_json::to_vec_pretty(&record)?)
        .with_context(|| format!("failed to write {}", metadata_path.display()))?;
    Ok(record)
}

fn build_proxy_router(state: ProxyState) -> Router {
    Router::new().fallback(any(proxy_request)).with_state(state)
}

async fn proxy_request(State(state): State<ProxyState>, request: Request) -> impl IntoResponse {
    match proxy_request_inner(state, request).await {
        Ok(response) => response,
        Err(err) => {
            let mut response = Response::new(Body::from(err.to_string()));
            *response.status_mut() = StatusCode::BAD_GATEWAY;
            response
        }
    }
}

async fn proxy_request_inner(state: ProxyState, request: Request) -> Result<Response<Body>> {
    let (parts, body) = request.into_parts();
    let target_url = rewrite_upstream_url(&state.upstream_base, &parts.uri)?;
    let body_bytes = to_bytes(body, MAX_PROXY_BODY_BYTES)
        .await
        .context("failed to read proxy request body")?;

    let mut upstream_request = state.client.request(parts.method.clone(), target_url);
    for (name, value) in &parts.headers {
        if is_hop_by_hop_header(name) || *name == header::HOST {
            continue;
        }
        upstream_request = upstream_request.header(name, value);
    }
    if !body_bytes.is_empty() {
        upstream_request = upstream_request.body(body_bytes.to_vec());
    }

    let upstream_response = upstream_request
        .send()
        .await
        .context("failed to forward request to ingress upstream")?;
    let status = upstream_response.status();
    let headers = upstream_response.headers().clone();
    let response_bytes = upstream_response
        .bytes()
        .await
        .context("failed to read ingress upstream response")?;

    let mut response = Response::new(Body::from(response_bytes));
    *response.status_mut() = status;
    copy_response_headers(&headers, response.headers_mut());
    Ok(response)
}

fn rewrite_upstream_url(
    upstream_base: &reqwest::Url,
    uri: &axum::http::Uri,
) -> Result<reqwest::Url> {
    let mut upstream = upstream_base.clone();
    upstream.set_path(uri.path());
    upstream.set_query(uri.query());
    Ok(upstream)
}

fn copy_response_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for (name, value) in from {
        if is_hop_by_hop_header(name) {
            continue;
        }
        to.append(name, value.clone());
    }
}

fn is_hop_by_hop_header(name: &header::HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn endpoint_listen_addr(endpoint: &reqwest::Url) -> Result<SocketAddr> {
    let port = endpoint
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("endpoint is missing a listen port"))?;
    let host = endpoint
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("endpoint is missing a host"))?;
    let ip = match host.parse::<IpAddr>() {
        Ok(ip) => ip,
        Err(_) if host.eq_ignore_ascii_case("localhost") => IpAddr::V4(Ipv4Addr::LOCALHOST),
        Err(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
    };
    Ok(SocketAddr::new(ip, port))
}

fn tls_dir(base_dir: &Path, binding_id: &str) -> PathBuf {
    base_dir
        .join("service-bindings")
        .join(binding_id)
        .join("tls")
}

fn tls_metadata_path(base_dir: &Path, binding_id: &str) -> PathBuf {
    tls_dir(base_dir, binding_id).join(TLS_METADATA_FILE_NAME)
}

fn require_tls_consent(
    binding_id: &str,
    endpoint_host: &str,
    install_system_trust: bool,
    yes: bool,
) -> Result<()> {
    if yes {
        return Ok(());
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!(
            "ingress TLS bootstrap requires explicit consent in an interactive terminal. Re-run with --yes after reviewing the trust action for binding '{}'.",
            binding_id
        );
    }

    println!(
        "TLS bootstrap will generate a local self-signed certificate for binding '{}' ({}){}.",
        binding_id,
        endpoint_host,
        if install_system_trust {
            " and attempt to install it into the user trust store"
        } else {
            ""
        }
    );
    print!("Proceed? [y/N]: ");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to read TLS consent response")?;
    let accepted = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if !accepted {
        anyhow::bail!(
            "ingress TLS bootstrap cancelled because consent was not granted for binding '{}'.",
            binding_id
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_system_trust_for_cert(cert_path: &Path) -> Result<()> {
    let keychain = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("failed to resolve home directory for login keychain"))?
        .join("Library/Keychains/login.keychain-db");
    let status = Command::new("security")
        .arg("add-trusted-cert")
        .arg("-d")
        .arg("-r")
        .arg("trustRoot")
        .arg("-k")
        .arg(&keychain)
        .arg(cert_path)
        .status()
        .with_context(|| {
            format!(
                "ingress TLS trust installation failed while invoking macOS security for {}",
                cert_path.display()
            )
        })?;
    if !status.success() {
        anyhow::bail!(
            "ingress TLS trust installation failed for {} (security exited with {})",
            cert_path.display(),
            status
        );
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_system_trust_for_cert(cert_path: &Path) -> Result<()> {
    anyhow::bail!(
        "ingress TLS trust installation is not yet implemented for this platform. Import {} into your local trust store manually.",
        cert_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        bootstrap_tls_in_dir, build_proxy_router, load_tls_bootstrap_from_dir, ProxyState,
    };
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use tower::util::ServiceExt;

    #[test]
    fn tls_bootstrap_writes_assets_and_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = bootstrap_tls_in_dir(temp.path(), "binding-demo", "localhost", false, true)
            .expect("bootstrap tls");
        assert!(record.cert_path.exists());
        assert!(record.key_path.exists());
        let loaded = load_tls_bootstrap_from_dir(temp.path(), "binding-demo")
            .expect("load metadata")
            .expect("metadata record");
        assert_eq!(loaded.binding_id, "binding-demo");
        assert_eq!(loaded.endpoint_host, "localhost");
        assert!(!loaded.system_trust_installed);
    }

    #[tokio::test]
    async fn proxy_router_forwards_requests_to_upstream() {
        let upstream_app = Router::new().route(
            "/hello",
            get(|| async { (axum::http::StatusCode::OK, [("x-upstream", "ok")], "world") }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
        let upstream_task = tokio::spawn(async move {
            axum::serve(upstream_listener, upstream_app.into_make_service())
                .await
                .expect("serve upstream")
        });

        let router = build_proxy_router(ProxyState {
            client: reqwest::Client::new(),
            upstream_base: reqwest::Url::parse(&format!(
                "http://127.0.0.1:{}/",
                upstream_addr.port()
            ))
            .expect("upstream url"),
        });

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/hello")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("proxy response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("x-upstream").unwrap(), "ok");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        assert_eq!(&body[..], b"world");

        upstream_task.abort();
    }
}
