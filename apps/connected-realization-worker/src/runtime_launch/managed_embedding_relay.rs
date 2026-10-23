//! Fixed-operation transport on one authorized service's existing internal
//! egress bridge. The API owns provider keys; this relay holds only the same
//! short per-Run capability that the explicitly bound service already has.
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use ato_ipc::runtime_launch_v2::{RuntimeLaunchSpec, RuntimeLaunchSpecV2};
use reqwest::blocking::Client;
use serde::Deserialize;
use url::Url;

use super::lease::NetworkAuthorization;
use super::network_broker::{ConnectionGroup, ConnectionPermit, EGRESS_BROKER_PORT};
use super::resolved::ResolvedRuntimeLaunchContext;

pub const RUNTIME_FEATURE: &str = "network=ato.managed-embedding@1";
const MAX_GRANTS: usize = 8;
const MAX_HEADERS: usize = 8 * 1024;
const MAX_BODY: usize = 32 * 1024;
const MAX_RESPONSE: usize = 128 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);
const BROKER_PATH: &str = "/v1/managed-secrets";

/// Physical lease authority, never part of Capsule/K/D identity. Origins are
/// minted from the control plane's trusted configuration, never app input.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedEmbeddingGrant {
    pub service_id: String,
    pub api_key_binding_id: String,
    pub base_url_binding_id: String,
    pub broker_origin: String,
}

fn origin(value: &str) -> Result<Url> {
    let parsed = Url::parse(value).context("invalid managed Broker origin")?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.origin().ascii_serialization() == value,
        "managed Broker origin must be normalized HTTPS"
    );
    Ok(parsed)
}

pub fn validate_grants(spec: &RuntimeLaunchSpec, authority: &NetworkAuthorization) -> Result<()> {
    validate_group_grants(
        spec.service_group_spec()
            .context("managed relay requires OCI service group")?,
        authority,
    )
}

pub fn validate_group_grants(
    spec: &RuntimeLaunchSpecV2,
    authority: &NetworkAuthorization,
) -> Result<()> {
    ensure!(
        authority.managed_embeddings.len() <= MAX_GRANTS,
        "too many managed embedding relays"
    );
    let group = spec.service_group();
    let mut services = BTreeSet::new();
    for grant in &authority.managed_embeddings {
        origin(&grant.broker_origin)?;
        let service = group
            .services
            .iter()
            .find(|s| s.name == grant.service_id)
            .context("managed relay service absent")?;
        ensure!(
            services.insert(&grant.service_id),
            "duplicate managed relay service"
        );
        ensure!(
            !authority
                .egress
                .iter()
                .any(|g| g.service_id == grant.service_id),
            "managed embedding service cannot have direct TCP egress"
        );
        for (id, name) in [
            (&grant.api_key_binding_id, "OPENAI_API_KEY"),
            (&grant.base_url_binding_id, "OPENAI_BASE_URL"),
        ] {
            ensure!(
                !id.is_empty()
                    && id.len() <= 64
                    && id.bytes().enumerate().all(|(i, b)| b.is_ascii_lowercase()
                        || (i > 0 && (b.is_ascii_digit() || b == b'_')))
                    && service
                        .secret_grants
                        .iter()
                        .filter(
                            |g| g.name == name && g.grant_ref == format!("portable-binding:{id}")
                        )
                        .count()
                        == 1
                    && !service.public_env.iter().any(|v| v.name == name),
                "managed relay requires exact declared scalar Bindings"
            );
        }
        ensure!(
            grant.api_key_binding_id != grant.base_url_binding_id,
            "managed relay Binding IDs must differ"
        );
    }
    Ok(())
}

pub struct ManagedEmbeddingRelay {
    endpoint: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    connections: Arc<ConnectionGroup>,
}

impl ManagedEmbeddingRelay {
    /// Resolve and match both authorized scalar values BEFORE creating a
    /// network or workload. Callers must retain this validation order.
    pub fn credentials(
        grant: &ManagedEmbeddingGrant,
        context: &ResolvedRuntimeLaunchContext,
    ) -> Result<(String, String)> {
        origin(&grant.broker_origin)?;
        let names = BTreeSet::from(["OPENAI_API_KEY", "OPENAI_BASE_URL"]);
        let values = context.secret_environment_for(&names)?;
        let token = &values["OPENAI_API_KEY"];
        let base = &values["OPENAI_BASE_URL"];
        ensure!(
            token.len() == 43
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "invalid short managed capability"
        );
        ensure!(
            *base == format!("{}{BROKER_PATH}", grant.broker_origin),
            "managed Broker Binding pair mismatch"
        );
        Ok((token.clone(), base.clone()))
    }

    pub fn start(gateway: IpAddr, grant: &ManagedEmbeddingGrant, token: String) -> Result<Self> {
        ensure!(gateway.is_ipv4(), "managed relay gateway must be IPv4");
        let endpoint = SocketAddr::new(gateway, EGRESS_BROKER_PORT);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(2))
            .build()?;
        Self::start_with_client(endpoint, grant, token, client)
    }

    fn start_with_client(
        endpoint: SocketAddr,
        grant: &ManagedEmbeddingGrant,
        token: String,
        client: Client,
    ) -> Result<Self> {
        let mut upstream = origin(&grant.broker_origin)?;
        upstream.set_path(&format!("{BROKER_PATH}/embeddings"));
        let listener =
            TcpListener::bind(endpoint).context("bind managed relay on isolated bridge")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let connections = Arc::new(ConnectionGroup::default());
        let group = Arc::clone(&connections);
        let active = Arc::new(AtomicUsize::new(0));
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let Some(permit) = ConnectionPermit::acquire_bounded(&active, 8) else {
                            continue;
                        };
                        let client = client.clone();
                        let upstream = upstream.clone();
                        let token = token.clone();
                        group.spawn(stream, permit, move |_, mut stream, _| {
                            let result = serve(&mut stream, &client, &upstream, &token);
                            if result.is_err() {
                                let _ = respond(
                                    &mut stream,
                                    400,
                                    b"{\"error\":\"managed_relay_refused\"}",
                                );
                                // Complete the small refusal without a TCP reset discarding
                                // its bytes when a bounded request body was already queued.
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
                                        Ok(n) => drained += n,
                                    }
                                }
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            endpoint,
            stop,
            worker: Some(worker),
            connections,
        })
    }

    pub fn binding_value(&self) -> String {
        format!("http://{}", self.endpoint)
    }
}

impl Drop for ManagedEmbeddingRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.connections.cancel_and_join();
    }
}

fn serve(stream: &mut TcpStream, client: &Client, upstream: &Url, token: &str) -> Result<()> {
    let deadline = Instant::now() + DEADLINE;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut header = Vec::with_capacity(1024);
    while !header.ends_with(b"\r\n\r\n") {
        ensure!(header.len() < MAX_HEADERS, "relay headers too large");
        stream.set_read_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ))?;
        ensure!(Instant::now() < deadline, "relay deadline");
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header)?;
    let mut lines = header.split("\r\n");
    ensure!(
        lines.next() == Some("POST /embeddings HTTP/1.1"),
        "relay only accepts fixed embedding POST"
    );
    let mut length = None;
    let mut authorization = None;
    let mut content_type = None;
    for line in lines.filter(|s| !s.is_empty()) {
        let (name, value) = line.split_once(':').context("malformed relay header")?;
        ensure!(
            !name.is_empty()
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !value.bytes().any(|b| b < 32 && b != b'\t'),
            "malformed relay header"
        );
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(
                length.is_none() && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                "ambiguous relay length"
            );
            length = Some(value.parse::<usize>()?);
        } else if name.eq_ignore_ascii_case("authorization") {
            ensure!(authorization.is_none(), "duplicate authorization");
            authorization = Some(value);
        } else if name.eq_ignore_ascii_case("content-type") {
            ensure!(content_type.is_none(), "duplicate type");
            content_type = Some(value);
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("content-encoding")
        {
            anyhow::bail!("encoded relay request refused");
        }
    }
    ensure!(
        authorization.is_some_and(|value| {
            let expected = format!("Bearer {token}");
            value.len() == expected.len()
                && value
                    .bytes()
                    .zip(expected.bytes())
                    .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                    == 0
        }),
        "relay capability mismatch"
    );
    ensure!(
        content_type == Some("application/json"),
        "relay requires JSON"
    );
    let length = length.context("relay length required")?;
    ensure!(length > 0 && length <= MAX_BODY, "relay body too large");
    let mut body = vec![0; length];
    let mut at = 0;
    while at < length {
        ensure!(Instant::now() < deadline, "relay deadline");
        stream.set_read_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1)),
        ))?;
        let got = stream.read(&mut body[at..])?;
        ensure!(got > 0, "truncated relay body");
        at += got;
    }
    // Header names/values from the workload are deliberately never forwarded.
    ensure!(Instant::now() < deadline, "relay deadline");
    let mut response = client
        .post(upstream.clone())
        .timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(2)),
        )
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .header("Accept-Encoding", "identity")
        .body(body)
        .send()
        .map_err(|_| anyhow::anyhow!("managed Broker unavailable"))?;
    ensure!(
        !response.status().is_redirection(),
        "managed Broker redirect refused"
    );
    ensure!(
        response
            .headers()
            .get("content-encoding")
            .is_none_or(|v| v == "identity"),
        "encoded Broker response refused"
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|v| v <= MAX_RESPONSE as u64),
        "Broker response too large"
    );
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("managed Broker response unavailable"))?;
    ensure!(bytes.len() <= MAX_RESPONSE, "Broker response too large");
    respond(stream, status, &bytes)
}

fn respond(stream: &mut TcpStream, status: u16, bytes: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} Response\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    )?;
    stream.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const FIXTURE: &str = include_str!(
        "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v3/service-group-volume.json"
    );

    fn grant() -> ManagedEmbeddingGrant {
        ManagedEmbeddingGrant {
            service_id: "backend".into(),
            api_key_binding_id: "embedding_key".into(),
            base_url_binding_id: "embedding_url".into(),
            broker_origin: "https://127.0.0.1:1".into(),
        }
    }
    fn authority() -> NetworkAuthorization {
        NetworkAuthorization {
            egress: vec![],
            fixed_tcp: vec![],
            managed_embeddings: vec![grant()],
        }
    }
    fn spec() -> RuntimeLaunchSpec {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        value["realization"]["services"][0]["secret_grants"] = serde_json::json!([
            {"name":"OPENAI_API_KEY","grant_ref":"portable-binding:embedding_key"},
            {"name":"OPENAI_BASE_URL","grant_ref":"portable-binding:embedding_url"}
        ]);
        RuntimeLaunchSpec::parse(&serde_json::to_string(&value).unwrap()).unwrap()
    }
    #[test]
    fn grant_requires_exact_declared_service_bindings_and_no_direct_egress() {
        let spec = spec();
        let mut auth = authority();
        validate_grants(&spec, &auth).unwrap();
        auth.managed_embeddings[0].service_id = "web".into();
        assert!(validate_grants(&spec, &auth).is_err());
        auth = authority();
        auth.managed_embeddings[0].base_url_binding_id = "other".into();
        assert!(validate_grants(&spec, &auth).is_err());
        auth = authority();
        auth.managed_embeddings.push(grant());
        assert!(validate_grants(&spec, &auth).is_err());
        auth = authority();
        auth.egress.push(super::super::lease::TcpEgressGrant {
            grant_id: "egr_test".into(),
            binding_id: "network".into(),
            environment: "ATO_BINDING_NETWORK".into(),
            service_id: "backend".into(),
            destination_cidr: "1.1.1.1/32".into(),
            ports: vec![443],
            generation: 1,
        });
        assert!(validate_grants(&spec, &auth).is_err());
    }
    #[test]
    fn redeemed_pair_must_match_the_authorized_origin_and_legacy_wire_stays_valid() {
        let legacy: NetworkAuthorization =
            serde_json::from_str(r#"{"egress":[],"fixed_tcp":[]}"#).unwrap();
        assert!(legacy.managed_embeddings.is_empty());
        let root = tempfile::tempdir().unwrap();
        let context = |token: &str, base: &str| {
            ResolvedRuntimeLaunchContext::new(
                root.path().to_path_buf(),
                "",
                std::collections::BTreeMap::new(),
                vec![
                    super::super::resolved::ResolvedSecret::new("OPENAI_API_KEY", token),
                    super::super::resolved::ResolvedSecret::new("OPENAI_BASE_URL", base),
                ],
                vec![],
                vec![],
            )
            .unwrap()
        };
        ManagedEmbeddingRelay::credentials(
            &grant(),
            &context(TOKEN, "https://127.0.0.1:1/v1/managed-secrets"),
        )
        .unwrap();
        assert!(
            ManagedEmbeddingRelay::credentials(
                &grant(),
                &context("sk-upstream-key", "https://127.0.0.1:1/v1/managed-secrets")
            )
            .is_err()
        );
        assert!(
            ManagedEmbeddingRelay::credentials(
                &grant(),
                &context(TOKEN, "https://wrong.example/v1/managed-secrets")
            )
            .is_err()
        );
    }

    #[test]
    fn origin_must_be_exact_normalized_https_not_a_target_url() {
        assert!(origin("https://broker.example").is_ok());
        for value in [
            "http://broker.example",
            "https://broker.example/",
            "https://BROKER.example",
            "https://broker.example:443",
            "https://user@broker.example",
            "https://broker.example/embeddings",
            "https://broker.example?x=1",
            "https://broker.example#x",
        ] {
            assert!(origin(value).is_err(), "{value}");
        }
    }
    #[test]
    fn malformed_requests_never_reach_a_broker_and_stop_releases_listener() {
        let relay = ManagedEmbeddingRelay::start_with_client(
            "127.0.0.1:0".parse().unwrap(),
            &grant(),
            TOKEN.into(),
            Client::builder().no_proxy().build().unwrap(),
        )
        .unwrap();
        for request in [
            "GET /embeddings HTTP/1.1\r\n\r\n".to_owned(),
            "POST https://evil.example/embeddings HTTP/1.1\r\n\r\n".to_owned(),
            "POST /chat/completions HTTP/1.1\r\n\r\n".to_owned(),
            "POST /embeddings HTTP/1.1\r\nAuthorization: Bearer wrong\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}".to_owned(),
            format!("POST /embeddings HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{{}}"),
            format!("POST /embeddings HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n"),
            format!("POST /embeddings HTTP/1.1\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: 32769\r\n\r\n"),
        ] {
            let mut stream=TcpStream::connect(relay.endpoint).unwrap();stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();stream.write_all(request.as_bytes()).unwrap();let mut bytes=Vec::new();let result=stream.read_to_end(&mut bytes);
            assert!(result.is_ok() || result.is_err_and(|e|e.kind()==std::io::ErrorKind::ConnectionReset));assert!(bytes.starts_with(b"HTTP/1.1 400"));assert!(!String::from_utf8_lossy(&bytes).contains(TOKEN));
        }
        let endpoint = relay.endpoint;
        drop(relay);
        assert!(TcpStream::connect(endpoint).is_err());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod docker_acceptance {
    use super::*;
    use ato_adapter_oci::OciNetwork;
    use std::collections::BTreeMap;
    use std::process::Command;

    #[test]
    #[ignore = "requires own CPU Docker fixture and local production Broker TLS stub"]
    fn official_sdk_container_uses_only_owned_relay_and_releases_resources() {
        let root = std::path::PathBuf::from(std::env::var("ATO_MANAGED_SDK_FIXTURE_DIR").unwrap());
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("fixture.json")).unwrap()).unwrap();
        let base = fixture["OPENAI_BASE_URL"].as_str().unwrap();
        let url = Url::parse(base).unwrap();
        let grant = ManagedEmbeddingGrant {
            service_id: "sdk".into(),
            api_key_binding_id: "key".into(),
            base_url_binding_id: "url".into(),
            broker_origin: url.origin().ascii_serialization(),
        };
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .add_root_certificate(
                reqwest::Certificate::from_pem(&std::fs::read(root.join("ca.pem")).unwrap())
                    .unwrap(),
            )
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let control = client.clone();
        let labels = BTreeMap::from([(
            "run.ato.dev/managed-sdk-test".to_owned(),
            std::process::id().to_string(),
        )]);
        let network = OciNetwork::create_egress("managed-sdk-acceptance", &labels).unwrap();
        let name = network.name().to_owned();
        let gateway = network.gateway_address().unwrap();
        let relay = ManagedEmbeddingRelay::start_with_client(
            SocketAddr::new(gateway, EGRESS_BROKER_PORT),
            &grant,
            fixture["OPENAI_API_KEY"].as_str().unwrap().into(),
            client,
        )
        .unwrap();
        let output = Command::new("docker")
            .args([
                "run",
                "--rm",
                "--read-only",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--memory",
                "128m",
                "--cpus",
                "0.5",
                "--pids-limit",
                "32",
                "--network",
                &name,
                "--label",
                "run.ato.dev/managed-sdk-test=true",
                "--mount",
                &format!(
                    "type=bind,source={},target=/app,readonly",
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../tests/managed-embedding-relay")
                        .display()
                ),
                "-e",
                "OPENAI_API_KEY",
                "-e",
                "OPENAI_BASE_URL",
                "-e",
                "RELAY_GATEWAY",
                "ato-managed-embedding-sdk:acceptance-20261024",
            ])
            .env(
                "OPENAI_API_KEY",
                fixture["OPENAI_API_KEY"].as_str().unwrap(),
            )
            .env("OPENAI_BASE_URL", relay.binding_value())
            .env("RELAY_GATEWAY", gateway.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "SDK failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        println!("{}", String::from_utf8_lossy(&output.stdout));
        let local = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(7))
            .build()
            .unwrap();
        let request =
            || {
                local.post(format!("{}/embeddings", relay.binding_value()))
            .bearer_auth(fixture["OPENAI_API_KEY"].as_str().unwrap())
            .header("Cookie", "workload-header-must-not-forward")
            .header("X-Disallowed", "workload-header-must-not-forward")
            .json(&serde_json::json!({"input":"one bounded document","encoding_format":"float"}))
            };
        // No additional provider call: all failure modes terminate in the test-only
        // Broker fixture before reaching its production embedding handler.
        for mode in ["redirect", "oversize", "timeout"] {
            assert!(
                control
                    .post(format!("{}/_fixture/mode", grant.broker_origin))
                    .json(&serde_json::json!({"mode":mode}))
                    .send()
                    .unwrap()
                    .status()
                    .is_success()
            );
            let at = Instant::now();
            assert_eq!(request().send().unwrap().status().as_u16(), 400, "{mode}");
            assert!(
                at.elapsed() < Duration::from_secs(6),
                "{mode} exceeded deadline"
            );
        }
        assert!(
            control
                .post(format!("{}/_fixture/mode", grant.broker_origin))
                .json(&serde_json::json!({"mode":"normal"}))
                .send()
                .unwrap()
                .status()
                .is_success()
        );
        assert!(
            control
                .post(format!("{}/_fixture/stop", grant.broker_origin))
                .send()
                .unwrap()
                .status()
                .is_success()
        );
        assert_eq!(request().send().unwrap().status().as_u16(), 403);
        println!(
            "redirect_oversize_timeout_refused=true headers_stripped=true stopped_run_refused=true"
        );
        let endpoint = relay.endpoint;
        drop(relay);
        assert!(TcpStream::connect_timeout(&endpoint, Duration::from_secs(1)).is_err());
        network.remove().unwrap();
        assert!(
            !Command::new("docker")
                .args(["network", "inspect", &name])
                .output()
                .unwrap()
                .status
                .success()
        );
        println!("relay_listener_released=true owned_network_removed=true");
    }
}
