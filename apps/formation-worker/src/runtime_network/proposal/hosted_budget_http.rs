//! Narrow managed-worker transport for a single hosted Search and round.
//! The receiving service must authenticate the managed principal, verify its
//! pinned binary and combine owner consent, worker, lease and round fences in
//! the same write. This client never turns its managed token into an owner token.
use super::budget::{BudgetSnapshot, JournalEvent};
use super::hosted_anthropic::HostedInferenceTransport;
use super::hosted_budget::{AppendOutcome, HostedJournalTransport};
use anyhow::{Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};

const MAX_RESPONSE_BYTES: u64 = 65536;
#[derive(Clone, Serialize)]
struct Lease {
    holder: String,
    claimant: String,
    generation: u64,
}
#[derive(Serialize)]
struct EventRequest<'a> {
    owner: &'a str,
    lease: &'a Lease,
    id: String,
    round: u64,
    round_fence: &'a str,
    event: &'a JournalEvent,
}
#[derive(Serialize)]
struct SnapshotRequest<'a> {
    owner: &'a str,
    lease: &'a Lease,
}
#[derive(Serialize)]
struct InferenceRequest<'a> {
    owner: &'a str,
    lease: &'a Lease,
    cell: &'a str,
    round: u64,
    round_fence: &'a str,
    timeout_ms: u64,
    body_base64: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppendResponse {
    appended: bool,
    revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotResponse {
    revision: u64,
    snapshot: BudgetSnapshot,
}

/// Server-issued scope; none of these values comes from the repository or model.
#[derive(Clone, Copy)]
pub struct HostedJournalScope<'a> {
    pub owner: &'a str,
    pub search: &'a str,
    pub holder: &'a str,
    pub claimant: &'a str,
    pub generation: u64,
    pub round: u64,
    pub round_fence: &'a str,
}

pub struct HostedJournalHttp {
    client: Client,
    endpoint: Url,
    token: String,
    owner: String,
    lease: Lease,
    round: u64,
    round_fence: String,
}
fn safe_id(s: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
impl HostedJournalHttp {
    /// `api` and token come from the trusted managed worker configuration;
    /// never use a repository, model or owner-provided endpoint here.
    pub fn new(api: &str, token: &str, scope: HostedJournalScope<'_>) -> Result<Self> {
        Self::build(api, token, scope, false)
    }
    fn build(api: &str, token: &str, scope: HostedJournalScope<'_>, mock: bool) -> Result<Self> {
        let HostedJournalScope {
            owner,
            search,
            holder,
            claimant,
            generation,
            round,
            round_fence,
        } = scope;
        let mut endpoint = Url::parse(api)?;
        let loopback = endpoint.scheme() == "http"
            && matches!(endpoint.host_str(), Some("127.0.0.1" | "[::1]"));
        ensure!(
            (endpoint.scheme() == "https" && !mock) || (mock && loopback),
            "invalid hosted ledger origin"
        );
        ensure!(
            endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.path() == "/",
            "invalid hosted ledger origin"
        );
        ensure!(
            safe_id(owner, 1, 256)
                && safe_id(search, 8, 128)
                && uuid(holder)
                && uuid(claimant)
                && uuid(round_fence)
                && generation > 0
                && round > 0
                && !token.is_empty()
                && token.len() <= 512
                && token.bytes().all(|b| b.is_ascii_graphic()),
            "invalid hosted ledger scope"
        );
        endpoint.set_path(&format!(
            "/v1/internal/formation/explorations/{search}/provider-journal/"
        ));
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            endpoint,
            token: token.into(),
            owner: owner.into(),
            lease: Lease {
                holder: holder.into(),
                claimant: claimant.into(),
                generation,
            },
            round,
            round_fence: round_fence.into(),
        })
    }
    fn call<T: serde::de::DeserializeOwned>(
        &self,
        operation: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let bytes = self.call_raw(operation, body, MAX_RESPONSE_BYTES, None)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("hosted ledger response invalid; outcome unknown"))
    }
    fn call_raw(
        &self,
        operation: &str,
        body: &impl Serialize,
        cap: u64,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>> {
        let url = self
            .endpoint
            .join(operation)
            .map_err(|_| anyhow::anyhow!("hosted ledger endpoint invalid"))?;
        let mut request = self.client.post(url).bearer_auth(&self.token).json(body);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let mut response = request
            .send()
            .map_err(|_| anyhow::anyhow!("hosted ledger transport failed; outcome unknown"))?;
        if !response.status().is_success() {
            bail!("hosted ledger refused the request");
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("hosted ledger response unreadable; outcome unknown"))?;
        ensure!(
            bytes.len() as u64 <= cap,
            "hosted ledger response too large"
        );
        Ok(bytes)
    }
}
impl HostedInferenceTransport for HostedJournalHttp {
    fn send(&self, cell: &str, round: u32, timeout_ms: u64, body: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            safe_id(cell, 1, 128)
                && u64::from(round) == self.round
                && (1..=30_000).contains(&timeout_ms)
                && !body.is_empty()
                && body.len() <= 65_536,
            "invalid hosted inference scope"
        );
        self.call_raw(
            "../provider-inference/anthropic",
            &InferenceRequest {
                owner: &self.owner,
                lease: &self.lease,
                cell,
                round: self.round,
                round_fence: &self.round_fence,
                timeout_ms,
                body_base64: BASE64.encode(body),
            },
            128 * 1024,
            Some(Duration::from_millis(timeout_ms)),
        )
    }
}
impl HostedJournalTransport for HostedJournalHttp {
    fn append(&self, event: &JournalEvent) -> Result<AppendOutcome> {
        let canonical = serde_jcs::to_vec(&(self.round, &self.round_fence, event))?;
        let id = format!("{:x}", Sha256::digest(canonical));
        let result: AppendResponse = self.call(
            "append",
            &EventRequest {
                owner: &self.owner,
                lease: &self.lease,
                id,
                round: self.round,
                round_fence: &self.round_fence,
                event,
            },
        )?;
        ensure!(
            result.revision > 0,
            "hosted ledger response invalid; outcome unknown"
        );
        Ok(if result.appended {
            AppendOutcome::Appended
        } else {
            AppendOutcome::AlreadyPresent
        })
    }
    fn snapshot(&self) -> Result<BudgetSnapshot> {
        let result: SnapshotResponse = self.call(
            "snapshot",
            &SnapshotRequest {
                owner: &self.owner,
                lease: &self.lease,
            },
        )?;
        let _revision = result.revision;
        Ok(result.snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    const HOLDER: &str = "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa";
    const CLAIMANT: &str = "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb";
    const FENCE: &str = "cccccccc-cccc-4ccc-cccc-cccccccccccc";
    #[test]
    fn sends_scoped_stable_event_identity_and_never_places_token_in_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut received = vec![];
            for n in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut data = vec![];
                let mut buf = [0u8; 4096];
                let end = loop {
                    let read = stream.read(&mut buf).unwrap();
                    assert!(read > 0);
                    data.extend_from_slice(&buf[..read]);
                    if let Some(pos) = data.windows(4).position(|b| b == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let headers = String::from_utf8(data[..end].to_vec()).unwrap();
                let size: usize = headers
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|v| v.parse().ok())
                    })
                    .unwrap();
                while data.len() < end + size {
                    let read = stream.read(&mut buf).unwrap();
                    assert!(read > 0);
                    data.extend_from_slice(&buf[..read]);
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&data[end..end + size]).unwrap();
                assert!(!String::from_utf8_lossy(&data[end..end + size]).contains("test-token"));
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer test-token")
                );
                received.push((headers.lines().next().unwrap().to_string(), body));
                let response = match n {
                    0 => r#"{"appended":true,"revision":1}"#,
                    1 => r#"{"appended":false,"revision":1}"#,
                    2 => r#"{"revision":1,"snapshot":{"cells":{},"stopped":false}}"#,
                    _ => r#"{"content":[]}"#,
                };
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
            }
            received
        });
        let transport = HostedJournalHttp::build(&origin, "test-token", scope(2), true).unwrap();
        let event = JournalEvent::Halt { halt: true };
        assert_eq!(transport.append(&event).unwrap(), AppendOutcome::Appended);
        assert_eq!(
            transport.append(&event).unwrap(),
            AppendOutcome::AlreadyPresent
        );
        assert!(!transport.snapshot().unwrap().stopped);
        assert_eq!(
            transport.send("call_1", 1, 1000, b"model bytes").unwrap(),
            b"{\"content\":[]}"
        );
        assert!(transport.send("call_1", 2, 1000, b"model bytes").is_err());
        let seen = server.join().unwrap();
        assert!(seen[0].0.starts_with(
            "POST /v1/internal/formation/explorations/search_123/provider-journal/append "
        ));
        assert!(seen[2].0.starts_with(
            "POST /v1/internal/formation/explorations/search_123/provider-journal/snapshot "
        ));
        assert!(seen[3].0.starts_with(
            "POST /v1/internal/formation/explorations/search_123/provider-inference/anthropic "
        ));
        assert_eq!(
            BASE64
                .decode(seen[3].1["body_base64"].as_str().unwrap())
                .unwrap(),
            b"model bytes"
        );
        assert_eq!(seen[0].1, seen[1].1);
        assert_eq!(seen[0].1["lease"]["generation"], 2);
        assert_eq!(seen[0].1["round_fence"], FENCE);
        assert_eq!(
            seen[0].1["id"],
            "31d1836715e2396171683042b0c50cf8a103f92f42ba052c89e4549fad999593"
        );
    }
    fn scope(generation: u64) -> HostedJournalScope<'static> {
        HostedJournalScope {
            owner: "owner_a",
            search: "search_123",
            holder: HOLDER,
            claimant: CLAIMANT,
            generation,
            round: 1,
            round_fence: FENCE,
        }
    }
    #[test]
    fn rejects_untrusted_origins_and_malformed_scope_before_network_use() {
        for origin in [
            "http://example.com",
            "http://127.0.0.1",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com/?query=1",
        ] {
            assert!(HostedJournalHttp::new(origin, "token", scope(1)).is_err());
        }
        let mut invalid = scope(1);
        invalid.owner = "../owner";
        assert!(HostedJournalHttp::new("https://example.com", "token", invalid).is_err());
        assert!(HostedJournalHttp::new("https://example.com", "token", scope(0)).is_err());
    }
}
