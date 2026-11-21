//! Fixed-operation relay from one authorized service to Ato's own OpenID
//! Provider.
//!
//! An OIDC relying party's server performs four back-channel operations:
//! discovery, JWKS, token and userinfo. The Coordinator is the only OpenID
//! Provider such a service may reach, but Run networks are --internal and the
//! TCP egress broker deliberately refuses loopback/private destinations. This
//! relay is the one purpose-built path across that boundary: it listens on a
//! dedicated per-service egress bridge, forwards exactly those four operations
//! to the grant's trusted issuer origin from the Runner host, and rewrites only
//! the discovery document's back-channel endpoints.
//!
//! The issuer origin is always minted by the control plane from its trusted
//! configuration. The workload cannot choose a destination, add a path, send a
//! redirect, or learn the upstream origin from a response.
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use ato_ipc::runtime_launch_v2::{RuntimeLaunchSpec, RuntimeLaunchSpecV2};
use reqwest::Method;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

use super::lease::NetworkAuthorization;
use super::network_broker::{ConnectionGroup, ConnectionPermit};

pub const RUNTIME_FEATURE: &str = "network=ato.oidc-issuer@1";
/// Fixed listener port on the service's dedicated egress bridge gateway.
/// Distinct from the TCP egress broker's (and managed embedding relay's) 1080
/// so a service can hold this authority next to an ordinary egress grant.
pub const RELAY_PORT: u16 = 8091;
/// The issuer path every Coordinator-minted issuer ends with.
const ISSUER_PATH_SUFFIX: &str = "/v1/selfhost/oidc";
const MAX_GRANTS: usize = 8;
const MAX_ISSUER_BYTES: usize = 2048;
const MAX_HEADERS: usize = 8 * 1024;
const MAX_BODY: usize = 16 * 1024;
const MAX_RESPONSE: usize = 256 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 8;

/// Physical lease authority, never part of Capsule/K/D identity. Every field
/// is minted by the control plane: the workload cannot name its own issuer or
/// environment.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcIssuerGrant {
    pub service_id: String,
    pub binding_id: String,
    pub environment: String,
    pub issuer: String,
}

/// A normalized issuer URL: an absolute HTTPS origin (HTTP only for a loopback
/// host, for tests) whose path ends with the Coordinator's issuer path.
fn issuer(value: &str) -> Result<Url> {
    ensure!(
        value.len() <= MAX_ISSUER_BYTES,
        "OIDC issuer is longer than {MAX_ISSUER_BYTES} bytes"
    );
    let parsed = Url::parse(value).context("invalid OIDC issuer")?;
    ensure!(
        parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.host().is_some(),
        "OIDC issuer must be an absolute URL without userinfo, query or fragment"
    );
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    ensure!(
        parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback),
        "OIDC issuer must be https except on a loopback host"
    );
    // The exact normalized spelling is what the Coordinator mints; accepting an
    // alternate form here would let two different strings name one authority.
    ensure!(parsed.as_str() == value, "OIDC issuer must be normalized");
    ensure!(
        parsed.path().starts_with('/') && parsed.path().ends_with(ISSUER_PATH_SUFFIX),
        "OIDC issuer path must end with {ISSUER_PATH_SUFFIX}"
    );
    ensure!(
        parsed.path()[1..]
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != ".."),
        "OIDC issuer path must be normalized"
    );
    Ok(parsed)
}

pub fn validate_grants(spec: &RuntimeLaunchSpec, authority: &NetworkAuthorization) -> Result<()> {
    validate_group_grants(
        spec.service_group_spec()
            .context("OIDC issuer relay requires OCI service group")?,
        authority,
    )
}

/// Validate every OIDC issuer grant before any network or workload exists.
pub fn validate_group_grants(
    spec: &RuntimeLaunchSpecV2,
    authority: &NetworkAuthorization,
) -> Result<()> {
    ensure!(
        authority.oidc_issuers.len() <= MAX_GRANTS,
        "too many OIDC issuer relays"
    );
    let group = spec.service_group();
    let mut services = BTreeSet::new();
    let mut environments = BTreeSet::new();
    for grant in &authority.oidc_issuers {
        issuer(&grant.issuer)?;
        let service = group
            .services
            .iter()
            .find(|service| service.name == grant.service_id)
            .context("OIDC issuer relay service absent")?;
        ensure!(
            services.insert(&grant.service_id),
            "duplicate OIDC issuer relay service"
        );
        ensure!(
            super::lease::valid_environment_name(&grant.environment),
            "invalid OIDC issuer environment name"
        );
        ensure!(
            environments.insert(&grant.environment),
            "duplicate OIDC issuer environment name"
        );
        ensure!(
            !grant.binding_id.is_empty()
                && grant.binding_id.len() <= 128
                && !grant
                    .binding_id
                    .bytes()
                    .any(|byte| byte <= 32 || byte == b'/'),
            "invalid OIDC issuer Binding id"
        );
        ensure!(
            !service
                .public_env
                .iter()
                .any(|entry| entry.name == grant.environment)
                && !service
                    .secret_grants
                    .iter()
                    .any(|entry| entry.name == grant.environment),
            "OIDC issuer environment conflicts with an existing service environment"
        );
        ensure!(
            !authority.egress.iter().any(|egress| {
                egress.service_id == grant.service_id && egress.environment == grant.environment
            }),
            "OIDC issuer environment duplicates a TCP egress Binding"
        );
    }
    Ok(())
}

pub struct OidcIssuerRelay {
    endpoint: SocketAddr,
    issuer_path: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    connections: Arc<ConnectionGroup>,
}

impl OidcIssuerRelay {
    /// The Runner trusts its Coordinator through ATO_RUNNER_API_CA_FILE; the
    /// issuer is the same Coordinator, so the relay reuses that trust
    /// configuration instead of inventing a second one.
    fn client() -> Result<Client> {
        crate::control_plane_client(
            Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(2)),
        )
    }

    pub fn start(gateway: IpAddr, grant: &OidcIssuerGrant) -> Result<Self> {
        ensure!(gateway.is_ipv4(), "OIDC issuer relay gateway must be IPv4");
        let client = Self::client()?;
        Self::start_with_client(SocketAddr::new(gateway, RELAY_PORT), grant, client)
    }

    fn start_with_client(
        endpoint: SocketAddr,
        grant: &OidcIssuerGrant,
        client: Client,
    ) -> Result<Self> {
        let issuer_url = issuer(&grant.issuer)?;
        let issuer_path = issuer_url.path().to_owned();
        let upstream_origin = issuer_url.origin().ascii_serialization();
        let issuer_value = grant.issuer.clone();
        let listener =
            TcpListener::bind(endpoint).context("bind OIDC issuer relay on isolated bridge")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?;
        let relay_base = format!("http://{endpoint}");
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let connections = Arc::new(ConnectionGroup::default());
        let group = Arc::clone(&connections);
        let active = Arc::new(AtomicUsize::new(0));
        let worker_issuer_path = issuer_path.clone();
        let worker_relay_base = relay_base.clone();
        let worker = thread::spawn(move || {
            let issuer_path = worker_issuer_path;
            let relay_base = worker_relay_base;
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let Some(permit) =
                            ConnectionPermit::acquire_bounded(&active, MAX_CONNECTIONS)
                        else {
                            continue;
                        };
                        let client = client.clone();
                        let upstream_origin = upstream_origin.clone();
                        let issuer_path = issuer_path.clone();
                        let relay_base = relay_base.clone();
                        let issuer_value = issuer_value.clone();
                        group.spawn(stream, permit, move |_, mut stream, _| {
                            let result = serve(
                                &mut stream,
                                &client,
                                &upstream_origin,
                                &issuer_path,
                                &relay_base,
                                &issuer_value,
                            );
                            if let Err(refusal) = result {
                                let _ = respond(
                                    &mut stream,
                                    refusal.status(),
                                    Some("application/json"),
                                    Some("no-store"),
                                    None,
                                    None,
                                    &refusal.body(),
                                );
                                // Complete the small refusal without a TCP reset
                                // discarding its bytes when a bounded request body
                                // was already queued.
                                let _ = stream.shutdown(Shutdown::Write);
                                let until = Instant::now() + Duration::from_millis(20);
                                let mut drained = 0;
                                let mut scratch = [0; 1024];
                                while Instant::now() < until && drained < MAX_BODY + MAX_HEADERS {
                                    let _ = stream.set_read_timeout(Some(
                                        until
                                            .saturating_duration_since(Instant::now())
                                            .max(Duration::from_millis(1)),
                                    ));
                                    match stream.read(&mut scratch) {
                                        Ok(0) | Err(_) => break,
                                        Ok(read) => drained += read,
                                    }
                                }
                            }
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            endpoint,
            issuer_path,
            stop,
            worker: Some(worker),
            connections,
        })
    }

    /// The relay's own origin, used only for discovery rewrites.
    fn relay_base(&self) -> String {
        format!("http://{}", self.endpoint)
    }

    /// The back-channel issuer base the service receives: the public issuer's
    /// path on the relay, so an OIDC client's discovery URL keeps the shape it
    /// would have used against the Coordinator.
    pub fn binding_value(&self) -> String {
        format!("{}{}", self.relay_base(), self.issuer_path)
    }
}

impl Drop for OidcIssuerRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.connections.cancel_and_join();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Discovery,
    Jwks,
    Token,
    Userinfo,
}

impl Operation {
    fn resolve(method: &str, target: &str, issuer_path: &str) -> Option<Self> {
        let path = |suffix: &str| format!("{issuer_path}{suffix}");
        match method {
            "GET" if target == path("/.well-known/openid-configuration") => Some(Self::Discovery),
            "GET" if target == path("/jwks") => Some(Self::Jwks),
            "POST" if target == path("/token") => Some(Self::Token),
            "GET" | "POST" if target == path("/userinfo") => Some(Self::Userinfo),
            _ => None,
        }
    }
}

/// Why the relay refused a request before or instead of forwarding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    BadRequest(&'static str),
    NotFound,
    BadGateway(&'static str),
}

impl Refusal {
    fn status(self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::NotFound => 404,
            Self::BadGateway(_) => 502,
        }
    }

    fn body(self) -> Vec<u8> {
        let code = match self {
            Self::BadRequest(code) | Self::BadGateway(code) => code,
            Self::NotFound => "operation_not_exposed",
        };
        format!("{{\"error\":\"{code}\"}}").into_bytes()
    }
}

/// Refuse with the relay's own status instead of a plain error.
macro_rules! ensure_refusal {
    ($condition:expr, $refusal:expr) => {
        if !$condition {
            return Err($refusal);
        }
    };
}

struct RequestHead {
    method: String,
    target: String,
    authorization: Option<String>,
    content_type: Option<String>,
    accept: Option<String>,
    content_length: Option<usize>,
}

fn serve(
    stream: &mut TcpStream,
    client: &Client,
    upstream_origin: &str,
    issuer_path: &str,
    relay_base: &str,
    issuer_value: &str,
) -> Result<(), Refusal> {
    let deadline = Instant::now() + DEADLINE;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let header = read_head(stream, deadline)?;
    let request = parse_head(&header)?;
    let Some(operation) = Operation::resolve(&request.method, &request.target, issuer_path) else {
        return Err(Refusal::NotFound);
    };
    let length = request.content_length.unwrap_or(0);
    ensure_refusal!(
        length <= MAX_BODY,
        Refusal::BadRequest("request_body_too_large")
    );
    let post = request.method == "POST";
    ensure_refusal!(
        length == 0 || post,
        Refusal::BadRequest("get_request_must_not_carry_a_body")
    );
    if post {
        ensure_refusal!(
            request.content_length.is_some(),
            Refusal::BadRequest("content_length_required")
        );
    }
    if operation == Operation::Token {
        ensure_refusal!(
            request
                .content_type
                .as_deref()
                .and_then(|value| value.split(';').next())
                .is_some_and(|value| value
                    .trim()
                    .eq_ignore_ascii_case("application/x-www-form-urlencoded")),
            Refusal::BadRequest("token_requires_form_encoding")
        );
        ensure_refusal!(
            length > 0,
            Refusal::BadRequest("token_request_body_required")
        );
    }
    let body = read_body(stream, length, deadline)?;
    let upstream = Url::parse(&format!("{upstream_origin}{}", request.target))
        .map_err(|_| Refusal::BadGateway("upstream_url_invalid"))?;
    ensure_refusal!(
        Instant::now() < deadline,
        Refusal::BadGateway("upstream_deadline")
    );
    let mut outgoing = client
        .request(if post { Method::POST } else { Method::GET }, upstream)
        .timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(2)),
        );
    for (name, value) in [
        ("Authorization", request.authorization),
        ("Content-Type", request.content_type),
        ("Accept", request.accept),
    ] {
        if let Some(value) = value {
            outgoing = outgoing.header(name, value);
        }
    }
    if post {
        outgoing = outgoing.body(body);
    }
    let mut response = outgoing
        .send()
        .map_err(|_| Refusal::BadGateway("upstream_unavailable"))?;
    ensure_refusal!(
        !response.status().is_redirection(),
        Refusal::BadGateway("upstream_redirect_refused")
    );
    ensure_refusal!(
        response
            .headers()
            .get("content-encoding")
            .is_none_or(|value| value == "identity"),
        Refusal::BadGateway("upstream_encoded_response_refused")
    );
    ensure_refusal!(
        response
            .content_length()
            .is_none_or(|value| value <= MAX_RESPONSE as u64),
        Refusal::BadGateway("upstream_response_too_large")
    );
    let status = response.status().as_u16();
    let content_type = response_header(&response, "content-type");
    let cache_control = response_header(&response, "cache-control");
    let pragma = response_header(&response, "pragma");
    let www_authenticate = response_header(&response, "www-authenticate");
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Refusal::BadGateway("upstream_response_unavailable"))?;
    ensure_refusal!(
        bytes.len() <= MAX_RESPONSE,
        Refusal::BadGateway("upstream_response_too_large")
    );
    if operation == Operation::Discovery {
        // The public issuer and authorize endpoint stay exactly as the
        // Coordinator minted them; only the back-channel endpoints move onto
        // the relay the service can actually reach.
        bytes = rewrite_discovery(&bytes, issuer_value, upstream_origin, relay_base)?;
        ensure_refusal!(
            bytes.len() <= MAX_RESPONSE,
            Refusal::BadGateway("upstream_response_too_large")
        );
    }
    respond(
        stream,
        status,
        content_type.as_deref(),
        cache_control.as_deref(),
        pragma.as_deref(),
        www_authenticate.as_deref(),
        &bytes,
    )
}

fn response_header(response: &reqwest::blocking::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn rewrite_discovery(
    body: &[u8],
    issuer_value: &str,
    issuer_origin: &str,
    relay_base: &str,
) -> Result<Vec<u8>, Refusal> {
    let mut document: Value =
        serde_json::from_slice(body).map_err(|_| Refusal::BadGateway("discovery_not_json"))?;
    let object = document
        .as_object_mut()
        .ok_or(Refusal::BadGateway("discovery_not_an_object"))?;
    ensure_refusal!(
        object.get("issuer").and_then(Value::as_str) == Some(issuer_value),
        Refusal::BadGateway("discovery_issuer_mismatch")
    );
    for field in ["token_endpoint", "jwks_uri", "userinfo_endpoint"] {
        let Some(value) = object.get_mut(field) else {
            continue;
        };
        let endpoint = value
            .as_str()
            .ok_or(Refusal::BadGateway("discovery_endpoint_not_a_string"))?;
        if endpoint.starts_with(issuer_value) {
            // Keep the public path and replace only the origin: the back-channel
            // issuer base is the relay name plus the same issuer path.
            let path = endpoint
                .strip_prefix(issuer_origin)
                .ok_or(Refusal::BadGateway("discovery_endpoint_mismatch"))?;
            *value = Value::String(format!("{relay_base}{path}"));
        }
    }
    serde_json::to_vec(&document).map_err(|_| Refusal::BadGateway("discovery_not_serializable"))
}

fn read_head(stream: &mut TcpStream, deadline: Instant) -> Result<Vec<u8>, Refusal> {
    let mut header = Vec::with_capacity(1024);
    while !header.ends_with(b"\r\n\r\n") {
        ensure_refusal!(
            header.len() < MAX_HEADERS,
            Refusal::BadRequest("request_headers_too_large")
        );
        let _ = stream.set_read_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ));
        ensure_refusal!(
            Instant::now() < deadline,
            Refusal::BadRequest("request_deadline")
        );
        let mut byte = [0];
        stream
            .read_exact(&mut byte)
            .map_err(|_| Refusal::BadRequest("request_unreadable"))?;
        header.push(byte[0]);
    }
    Ok(header)
}

fn parse_head(header: &[u8]) -> Result<RequestHead, Refusal> {
    let header =
        std::str::from_utf8(header).map_err(|_| Refusal::BadRequest("request_headers_not_utf8"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines
        .next()
        .ok_or(Refusal::BadRequest("request_line_missing"))?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Refusal::BadRequest("request_line_malformed"));
    };
    if version != "HTTP/1.1"
        || !matches!(method, "GET" | "POST")
        || target.is_empty()
        || target.contains('#')
    {
        return Err(Refusal::BadRequest("request_line_malformed"));
    }
    let mut authorization = None;
    let mut content_type = None;
    let mut accept = None;
    let mut content_length = None;
    let mut headers = 0usize;
    for line in lines.filter(|line| !line.is_empty()) {
        headers += 1;
        ensure_refusal!(
            headers <= 64,
            Refusal::BadRequest("request_headers_too_many")
        );
        let (name, value) = line
            .split_once(':')
            .ok_or(Refusal::BadRequest("request_header_malformed"))?;
        ensure_refusal!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !value
                    .bytes()
                    .any(|byte| (byte < 32 && byte != b'\t') || byte == 127 || byte >= 128),
            Refusal::BadRequest("request_header_malformed")
        );
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            ensure_refusal!(
                content_length.is_none()
                    && !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit()),
                Refusal::BadRequest("request_length_ambiguous")
            );
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| Refusal::BadRequest("request_length_ambiguous"))?,
            );
        } else if name.eq_ignore_ascii_case("authorization") {
            ensure_refusal!(
                authorization.is_none(),
                Refusal::BadRequest("request_authorization_duplicated")
            );
            authorization = Some(value);
        } else if name.eq_ignore_ascii_case("content-type") {
            ensure_refusal!(
                content_type.is_none(),
                Refusal::BadRequest("request_content_type_duplicated")
            );
            content_type = Some(value);
        } else if name.eq_ignore_ascii_case("accept") {
            ensure_refusal!(
                accept.is_none(),
                Refusal::BadRequest("request_accept_duplicated")
            );
            accept = Some(value);
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Refusal::BadRequest("request_transfer_encoding_refused"));
        } else if name.eq_ignore_ascii_case("content-encoding") {
            return Err(Refusal::BadRequest("request_content_encoding_refused"));
        }
    }
    Ok(RequestHead {
        method: method.to_owned(),
        target: target.to_owned(),
        authorization: authorization.map(str::to_owned),
        content_type: content_type.map(str::to_owned),
        accept: accept.map(str::to_owned),
        content_length,
    })
}

fn read_body(stream: &mut TcpStream, length: usize, deadline: Instant) -> Result<Vec<u8>, Refusal> {
    let mut body = vec![0; length];
    let mut at = 0;
    while at < length {
        let _ = stream.set_read_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ));
        ensure_refusal!(
            Instant::now() < deadline,
            Refusal::BadRequest("request_deadline")
        );
        let read = stream
            .read(&mut body[at..])
            .map_err(|_| Refusal::BadRequest("request_body_unreadable"))?;
        ensure_refusal!(read > 0, Refusal::BadRequest("request_body_truncated"));
        at += read;
    }
    Ok(body)
}

#[allow(clippy::too_many_arguments)]
fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: Option<&str>,
    cache_control: Option<&str>,
    pragma: Option<&str>,
    www_authenticate: Option<&str>,
    body: &[u8],
) -> Result<(), Refusal> {
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Response");
    let header = |name: &str, value: &str| format!("{name}: {}", value.replace(['\r', '\n'], " "));
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in [
        ("Content-Type", content_type),
        ("Cache-Control", cache_control),
        ("Pragma", pragma),
        ("WWW-Authenticate", www_authenticate),
    ] {
        if let Some(value) = value {
            head.push_str(&header(name, value));
            head.push_str("\r\n");
        }
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    stream
        .write_all(head.as_bytes())
        .map_err(|_| Refusal::BadGateway("response_unwritable"))?;
    stream
        .write_all(body)
        .map_err(|_| Refusal::BadGateway("response_unwritable"))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;

    const FIXTURE: &str = include_str!(
        "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v3/service-group-volume.json"
    );

    fn spec() -> RuntimeLaunchSpec {
        RuntimeLaunchSpec::parse(FIXTURE).expect("v3 fixture")
    }

    fn grant(issuer: &str) -> OidcIssuerGrant {
        OidcIssuerGrant {
            service_id: "backend".into(),
            binding_id: "oidc_issuer".into(),
            environment: "OIDC_ISSUER".into(),
            issuer: issuer.into(),
        }
    }

    fn authority(grants: Vec<OidcIssuerGrant>) -> NetworkAuthorization {
        NetworkAuthorization {
            egress: vec![],
            fixed_tcp: vec![],
            managed_embeddings: vec![],
            oidc_issuers: grants,
        }
    }

    #[test]
    fn legacy_authorization_still_parses_without_oidc_grants() {
        let legacy: NetworkAuthorization =
            serde_json::from_str(r#"{"egress":[],"fixed_tcp":[]}"#).unwrap();
        assert!(legacy.oidc_issuers.is_empty());
        let explicit: NetworkAuthorization =
            serde_json::from_str(r#"{"egress":[],"fixed_tcp":[],"oidc_issuers":[]}"#).unwrap();
        assert!(explicit.oidc_issuers.is_empty());
        let grants: NetworkAuthorization = serde_json::from_str(
            r#"{"egress":[],"fixed_tcp":[],"oidc_issuers":[{"service_id":"backend","binding_id":"oidc","environment":"OIDC_ISSUER","issuer":"https://issuer.example/v1/selfhost/oidc"}]}"#,
        )
        .unwrap();
        assert_eq!(grants.oidc_issuers.len(), 1);
        // Unknown fields stay refused.
        assert!(
            serde_json::from_str::<NetworkAuthorization>(
                r#"{"egress":[],"fixed_tcp":[],"oidc_issuer":[]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn issuer_must_be_normalized_https_with_the_coordinator_path() {
        for valid in [
            "https://issuer.example/v1/selfhost/oidc",
            "https://issuer.example/coordinator/v1/selfhost/oidc",
            "http://127.0.0.1:8090/v1/selfhost/oidc",
            "http://[::1]:8090/v1/selfhost/oidc",
            "http://localhost:8090/v1/selfhost/oidc",
        ] {
            assert!(issuer(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "http://issuer.example/v1/selfhost/oidc",
            "https://issuer.example/v1/selfhost",
            "https://issuer.example/v1/selfhost/oidc/",
            "https://issuer.example/other",
            "https://issuer.example/v1/selfhost/oidc?x=1",
            "https://issuer.example/v1/selfhost/oidc#fragment",
            "https://user@issuer.example/v1/selfhost/oidc",
            "https://issuer.example:443/v1/selfhost/oidc",
            "HTTPS://issuer.example/v1/selfhost/oidc",
            "/v1/selfhost/oidc",
            "not a url",
        ] {
            assert!(issuer(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn grants_are_bounded_to_known_services_and_valid_environments() {
        let spec = spec();
        let issuer = "https://issuer.example/v1/selfhost/oidc";
        let valid = authority(vec![grant(issuer)]);
        validate_grants(&spec, &valid).unwrap();

        let mut unknown_service = valid.clone();
        unknown_service.oidc_issuers[0].service_id = "ghost".into();
        assert!(validate_grants(&spec, &unknown_service).is_err());

        let mut duplicate_service = valid.clone();
        let mut second = grant(issuer);
        second.environment = "OIDC_ISSUER_TWO".into();
        duplicate_service.oidc_issuers.push(second);
        assert!(validate_grants(&spec, &duplicate_service).is_err());

        let mut duplicate_environment = valid.clone();
        let mut second = grant(issuer);
        second.service_id = "web".into();
        duplicate_environment.oidc_issuers.push(second);
        assert!(validate_grants(&spec, &duplicate_environment).is_err());

        for environment in ["oidc_issuer", "OIDC-ISSUER", "", "OIDC ISSUER"] {
            let mut bad = valid.clone();
            bad.oidc_issuers[0].environment = environment.into();
            assert!(
                validate_grants(&spec, &bad).is_err(),
                "accepted environment {environment:?}"
            );
        }

        let mut public_conflict = valid.clone();
        public_conflict.oidc_issuers[0].environment = "MODE".into();
        assert!(validate_grants(&spec, &public_conflict).is_err());

        let mut bad_issuer = valid.clone();
        bad_issuer.oidc_issuers[0].issuer = "https://issuer.example/other".into();
        assert!(validate_grants(&spec, &bad_issuer).is_err());

        let mut too_many = valid;
        for index in 0..8 {
            let mut extra = grant(issuer);
            extra.service_id = format!("service{index}");
            too_many.oidc_issuers.push(extra);
        }
        assert!(validate_grants(&spec, &too_many).is_err());
    }

    #[derive(Debug, Clone)]
    struct Recorded {
        head: String,
        body: Vec<u8>,
    }

    /// A fake issuer reply: status, literal headers and body.
    type FixtureResponse = (u16, Vec<(String, String)>, Vec<u8>);

    struct FakeIssuer {
        address: SocketAddr,
        requests: Arc<Mutex<Vec<Recorded>>>,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl FakeIssuer {
        fn start(responses: Vec<FixtureResponse>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            Self::from_listener(listener, responses)
        }

        fn from_listener(listener: TcpListener, responses: Vec<FixtureResponse>) -> Self {
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&requests);
            let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let answer = responses
                                .lock()
                                .ok()
                                .and_then(|mut responses| responses.pop_front())
                                .unwrap_or_else(|| json_response(200, b"{}"));
                            let recorded = Arc::clone(&recorded);
                            thread::spawn(move || {
                                // A listener accepted in non-blocking mode can
                                // hand the connection that flag on BSD; the
                                // handler needs ordinary blocking reads.
                                let _ = stream.set_nonblocking(false);
                                serve_fixture(&mut stream, &recorded, answer);
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(_) => thread::sleep(Duration::from_millis(1)),
                    }
                }
            });
            Self {
                address,
                requests,
                stop,
                worker: Some(worker),
            }
        }

        fn issuer(&self) -> String {
            format!("http://{}/v1/selfhost/oidc", self.address)
        }

        fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for FakeIssuer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn serve_fixture(
        stream: &mut TcpStream,
        requests: &Arc<Mutex<Vec<Recorded>>>,
        response: FixtureResponse,
    ) {
        let Ok(mut stream) = stream.try_clone() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut head = Vec::new();
        let mut byte = [0];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read_exact(&mut byte) {
                Ok(()) => head.push(byte[0]),
                Err(_) => return,
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let length = head
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        if length > 0 && stream.read_exact(&mut body).is_err() {
            return;
        }
        if let Ok(mut recorded) = requests.lock() {
            recorded.push(Recorded { head, body });
        }
        let (status, headers, body) = response;
        let reason = reqwest::StatusCode::from_u16(status)
            .ok()
            .and_then(|status| status.canonical_reason())
            .unwrap_or("Response");
        let mut reply = format!("HTTP/1.1 {status} {reason}\r\n");
        for (name, value) in headers {
            reply.push_str(&format!("{name}: {value}\r\n"));
        }
        reply.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ));
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.write_all(&body);
    }

    fn json_response(status: u16, body: &[u8]) -> FixtureResponse {
        (
            status,
            vec![("Content-Type".to_owned(), "application/json".to_owned())],
            body.to_vec(),
        )
    }

    fn discovery_body(issuer: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/jwks"),
            "userinfo_endpoint": format!("{issuer}/userinfo"),
            "scopes_supported": ["openid"],
            "unrelated": "kept",
        }))
        .unwrap()
    }

    fn send(relay: &OidcIssuerRelay, request: &str) -> String {
        let mut stream = TcpStream::connect(relay.endpoint).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn relay_for(issuer: &FakeIssuer) -> OidcIssuerRelay {
        let grant = grant(&issuer.issuer());
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        OidcIssuerRelay::start_with_client("127.0.0.1:0".parse().unwrap(), &grant, client).unwrap()
    }

    #[test]
    fn allowed_operations_forward_only_the_declared_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let discovery = discovery_body(&format!("http://{address}/v1/selfhost/oidc"));
        let issuer = FakeIssuer::from_listener(
            listener,
            vec![
                json_response(200, &discovery),
                json_response(200, b"{\"keys\":[]}"),
                json_response(200, b"{\"access_token\":\"t\"}"),
                json_response(200, b"{\"sub\":\"alice\"}"),
                json_response(200, b"{\"sub\":\"alice\"}"),
            ],
        );
        let relay = relay_for(&issuer);
        let path = "/v1/selfhost/oidc";
        let cases = [
            format!(
                "GET {path}/.well-known/openid-configuration HTTP/1.1\r\nHost: ignored\r\nAuthorization: Bearer public\r\nAccept: application/json\r\nCookie: must-not-forward\r\nX-Disallowed: must-not-forward\r\n\r\n"
            ),
            format!(
                "GET {path}/jwks HTTP/1.1\r\nHost: ignored\r\nX-Disallowed: must-not-forward\r\n\r\n"
            ),
            format!(
                "POST {path}/token HTTP/1.1\r\nHost: ignored\r\nAuthorization: Bearer public\r\nContent-Type: application/x-www-form-urlencoded\r\nAccept: application/json\r\nCookie: must-not-forward\r\nContent-Length: 15\r\n\r\ngrant_type=code"
            ),
            format!(
                "GET {path}/userinfo HTTP/1.1\r\nHost: ignored\r\nAuthorization: Bearer at\r\nX-Disallowed: must-not-forward\r\n\r\n"
            ),
            format!(
                "POST {path}/userinfo HTTP/1.1\r\nHost: ignored\r\nAuthorization: Bearer at\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 13\r\n\r\naccess_token="
            ),
        ];
        for request in &cases {
            let response = send(&relay, request);
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(
                response.contains("Connection: close"),
                "response must close the connection: {response}"
            );
            assert!(
                !response.contains("Set-Cookie"),
                "unlisted response header leaked: {response}"
            );
        }
        let recorded = issuer.requests();
        assert_eq!(recorded.len(), cases.len());
        for entry in &recorded {
            let lower = entry.head.to_ascii_lowercase();
            assert!(!lower.contains("cookie"), "{:?}", entry.head);
            assert!(!lower.contains("x-disallowed"), "{:?}", entry.head);
        }
        let first = recorded[0].head.to_ascii_lowercase();
        assert!(
            first.contains("authorization: bearer public"),
            "{:?}",
            recorded[0].head
        );
        assert!(
            first.contains("accept: application/json"),
            "{:?}",
            recorded[0].head
        );
        assert_eq!(
            recorded[2]
                .head
                .to_ascii_lowercase()
                .matches("content-type:")
                .count(),
            1
        );
        assert_eq!(
            String::from_utf8_lossy(&recorded[2].body),
            "grant_type=code"
        );
    }

    #[test]
    fn discovery_is_rewritten_but_keeps_the_public_issuer_and_authorize_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let issuer_value = format!("http://{address}/v1/selfhost/oidc");
        let body = discovery_body(&issuer_value);
        let issuer = FakeIssuer::from_listener(listener, vec![json_response(200, &body)]);
        let relay = relay_for(&issuer);
        let response = send(
            &relay,
            "GET /v1/selfhost/oidc/.well-known/openid-configuration HTTP/1.1\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let (_, payload) = response.split_once("\r\n\r\n").unwrap();
        let document: Value = serde_json::from_str(payload).unwrap();
        assert_eq!(document["issuer"], issuer.issuer());
        assert_eq!(
            document["authorization_endpoint"],
            format!("{}/authorize", issuer.issuer())
        );
        assert_eq!(
            document["token_endpoint"],
            format!("{}/v1/selfhost/oidc/token", relay.relay_base())
        );
        assert_eq!(
            document["jwks_uri"],
            format!("{}/v1/selfhost/oidc/jwks", relay.relay_base())
        );
        assert_eq!(
            document["userinfo_endpoint"],
            format!("{}/v1/selfhost/oidc/userinfo", relay.relay_base())
        );
        assert_eq!(document["unrelated"], "kept");
        assert_eq!(document["scopes_supported"][0], "openid");
    }

    #[test]
    fn discovery_with_a_foreign_issuer_is_a_bad_gateway() {
        let foreign = discovery_body("https://attacker.example/v1/selfhost/oidc");
        let issuer = FakeIssuer::start(vec![json_response(200, &foreign)]);
        let relay = relay_for(&issuer);
        let response = send(
            &relay,
            "GET /v1/selfhost/oidc/.well-known/openid-configuration HTTP/1.1\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 502"), "{response}");
        assert!(response.contains("discovery_issuer_mismatch"), "{response}");
    }

    #[test]
    fn disallowed_operations_get_404_and_are_never_forwarded() {
        let issuer = FakeIssuer::start(vec![]);
        let relay = relay_for(&issuer);
        for request in [
            "POST /v1/selfhost/oidc/.well-known/openid-configuration HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
            "GET /v1/selfhost/oidc/token HTTP/1.1\r\n\r\n",
            "POST /v1/selfhost/oidc/jwks HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
            "GET /v1/selfhost/oidc/authorize HTTP/1.1\r\n\r\n",
            "GET /v1/selfhost/oidc/other HTTP/1.1\r\n\r\n",
            "GET /v1/other/.well-known/openid-configuration HTTP/1.1\r\n\r\n",
            "GET http://attacker.example/v1/selfhost/oidc/jwks HTTP/1.1\r\n\r\n",
        ] {
            let response = send(&relay, request);
            assert!(
                response.starts_with("HTTP/1.1 404"),
                "{request} => {response}"
            );
        }
        assert!(issuer.requests().is_empty());
    }

    #[test]
    fn oversized_encoded_and_streamed_bodies_are_refused_before_forwarding() {
        let issuer = FakeIssuer::start(vec![]);
        let relay = relay_for(&issuer);
        for request in [
            format!(
                "POST /v1/selfhost/oidc/token HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY + 1
            ),
            "POST /v1/selfhost/oidc/token HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_owned(),
            "POST /v1/selfhost/oidc/token HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".to_owned(),
            "POST /v1/selfhost/oidc/token HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 0\r\n\r\n".to_owned(),
            "GET /v1/selfhost/oidc/userinfo HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}".to_owned(),
        ] {
            let response = send(&relay, &request);
            assert!(
                response.starts_with("HTTP/1.1 400"),
                "{request} => {response}"
            );
        }
        assert!(issuer.requests().is_empty());
    }

    #[test]
    fn one_connection_serves_exactly_one_request() {
        let issuer = FakeIssuer::start(vec![json_response(200, b"{}")]);
        let relay = relay_for(&issuer);
        let mut stream = TcpStream::connect(relay.endpoint).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /v1/selfhost/oidc/jwks HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut first = Vec::new();
        stream.read_to_end(&mut first).unwrap();
        assert!(first.starts_with(b"HTTP/1.1 200"), "{first:?}");
        let _ = stream.write_all(b"GET /v1/selfhost/oidc/jwks HTTP/1.1\r\n\r\n");
        let mut second = Vec::new();
        let _ = stream.read_to_end(&mut second);
        assert!(second.is_empty(), "second response must not be served");
        assert_eq!(issuer.requests().len(), 1);
    }

    #[test]
    fn upstream_transport_failure_is_a_bad_gateway() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let grant = grant(&format!("http://{address}/v1/selfhost/oidc"));
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let relay =
            OidcIssuerRelay::start_with_client("127.0.0.1:0".parse().unwrap(), &grant, client)
                .unwrap();
        let response = send(&relay, "GET /v1/selfhost/oidc/jwks HTTP/1.1\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 502"), "{response}");
    }
}
