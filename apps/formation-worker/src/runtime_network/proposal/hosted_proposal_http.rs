//! Managed-worker proposal capability. Authentication never uses an owner token.
use super::super::RuntimeConstraintWire;
use super::ProposalCoordinator;
use anyhow::{Result, bail, ensure};
use reqwest::{Url, blocking::Client};
use serde_json::{Value, json};
use std::{
    io::Read,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct HostedProposalHttp {
    client: Client,
    endpoint: Url,
    token: String,
    owner: String,
    satisfy: String,
    holder: String,
    claimant: String,
    generation: u64,
    deadline_ms: Option<u64>,
}

pub struct HostedProposalScope<'a> {
    pub owner: &'a str,
    pub search: &'a str,
    pub satisfy: &'a str,
    pub holder: &'a str,
    pub claimant: &'a str,
    pub generation: u64,
}

fn safe_id(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

impl HostedProposalHttp {
    #[cfg(test)]
    pub(crate) fn for_test(api: &str, token: &str, scope: HostedProposalScope<'_>) -> Result<Self> {
        Self::build(api, token, scope, true)
    }
    pub fn new(api: &str, token: &str, scope: HostedProposalScope<'_>) -> Result<Self> {
        Self::build(api, token, scope, false)
    }
    fn build(api: &str, token: &str, scope: HostedProposalScope<'_>, mock: bool) -> Result<Self> {
        let mut endpoint = Url::parse(api)?;
        let loopback = endpoint.scheme() == "http"
            && matches!(endpoint.host_str(), Some("127.0.0.1" | "[::1]"));
        ensure!(
            (endpoint.scheme() == "https" && !mock) || (mock && loopback),
            "invalid hosted coordinator origin"
        );
        ensure!(
            endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.path() == "/",
            "invalid hosted coordinator origin"
        );
        ensure!(
            safe_id(scope.owner, 1, 256)
                && safe_id(scope.search, 8, 128)
                && safe_id(scope.satisfy, 1, 256)
                && uuid(scope.holder)
                && uuid(scope.claimant)
                && scope.generation > 0
                && !token.is_empty()
                && token.len() <= 512
                && token.bytes().all(|b| b.is_ascii_graphic()),
            "invalid hosted coordinator scope"
        );
        endpoint.set_path(&format!(
            "/v1/internal/formation/explorations/{}/proposal/",
            scope.search
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
            owner: scope.owner.into(),
            satisfy: scope.satisfy.into(),
            holder: scope.holder.into(),
            claimant: scope.claimant.into(),
            generation: scope.generation,
            deadline_ms: None,
        })
    }
    fn call(&self, operation: &str, fields: Value) -> Result<Value> {
        let mut body = json!({"owner":self.owner,"satisfy_id":self.satisfy,
            "lease":{"holder":self.holder,"claimant":self.claimant,"generation":self.generation}});
        let extra = fields
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("invalid hosted coordinator fields"))?;
        for (key, value) in extra {
            body[key] = value.clone();
        }
        let mut request = self
            .client
            .post(self.endpoint.join(operation)?)
            .bearer_auth(&self.token)
            .json(&body);
        if let Some(deadline) = self.deadline_ms {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
            ensure!(
                u128::from(deadline) > now,
                "hosted proposal deadline expired"
            );
            let remaining = u64::try_from(u128::from(deadline) - now)?.min(10_000);
            request = request.timeout(Duration::from_millis(remaining));
        }
        let mut response = request
            .send()
            .map_err(|_| anyhow::anyhow!("hosted coordinator transport failed; outcome unknown"))?;
        if !response.status().is_success() {
            bail!("hosted coordinator refused the request");
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                anyhow::anyhow!("hosted coordinator response unreadable; outcome unknown")
            })?;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "hosted coordinator response too large"
        );
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("hosted coordinator response invalid; outcome unknown"))
    }
    pub fn status(&self) -> Result<Value> {
        self.call("status", json!({}))
    }
    fn check_id(&self, id: &str) -> Result<()> {
        ensure!(id == self.satisfy, "hosted coordinator request mismatch");
        Ok(())
    }
}

impl ProposalCoordinator for HostedProposalHttp {
    fn bounded(&self, deadline_ms: u64) -> Box<dyn ProposalCoordinator> {
        let mut bounded = self.clone();
        bounded.deadline_ms = Some(
            self.deadline_ms
                .map_or(deadline_ms, |old| old.min(deadline_ms)),
        );
        Box::new(bounded)
    }
    fn claim_proposal(&self, id: &str, revision: u64, claimant_id: &str) -> Result<Value> {
        self.check_id(id)?;
        ensure!(
            claimant_id == self.claimant,
            "hosted coordinator claimant mismatch"
        );
        self.call(
            "claim",
            json!({"revision":revision,"claimant_id":claimant_id}),
        )
    }
    fn complete_proposal(&self, id: &str, bytes: &[u8]) -> Result<Value> {
        self.check_id(id)?;
        ensure!(bytes.len() <= 192 * 1024, "hosted proposal too large");
        let submission: Value = serde_json::from_slice(bytes)?;
        self.call("complete", json!({"submission":submission}))
    }
    fn report_completion(&self, id: &str, bytes: &[u8]) -> Result<Value> {
        self.complete_proposal(id, bytes)
    }
    fn variable_metadata(&self, _application: &str, _search: &str) -> Result<Vec<Value>> {
        bail!("hosted variable metadata unavailable")
    }
    fn runtime_capabilities(&self, _constraint: &RuntimeConstraintWire) -> Result<Vec<Value>> {
        bail!("hosted runtime capabilities unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, thread};
    const HOLDER: &str = "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa";
    const CLAIMANT: &str = "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb";
    fn scope() -> HostedProposalScope<'static> {
        HostedProposalScope {
            owner: "owner_a",
            search: "search_123",
            satisfy: "sat_1",
            holder: HOLDER,
            claimant: CLAIMANT,
            generation: 2,
        }
    }
    #[test]
    fn scopes_operations_and_never_places_worker_token_in_json() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut data = Vec::new();
                let mut chunk = [0u8; 4096];
                let start = loop {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                    if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8(data[..start].to_vec()).unwrap();
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|v| v.parse().ok())
                    })
                    .unwrap();
                while data.len() < start + size {
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&chunk[..n]);
                }
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer test-token")
                );
                assert!(
                    !String::from_utf8_lossy(&data[start..start + size]).contains("test-token")
                );
                seen.push((
                    headers.lines().next().unwrap().to_owned(),
                    serde_json::from_slice::<Value>(&data[start..start + size]).unwrap(),
                ));
                let response = "{}";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .unwrap();
            }
            seen
        });
        let client = HostedProposalHttp::build(&origin, "test-token", scope(), true).unwrap();
        assert!(client.claim_proposal("other", 1, CLAIMANT).is_err());
        assert!(client.claim_proposal("sat_1", 1, HOLDER).is_err());
        assert!(
            client
                .bounded(1)
                .claim_proposal("sat_1", 1, CLAIMANT)
                .is_err()
        );
        client.status().unwrap();
        client.claim_proposal("sat_1", 3, CLAIMANT).unwrap();
        client
            .complete_proposal("sat_1", br#"{"revision":4,"outcome":"timeout"}"#)
            .unwrap();
        let seen = server.join().unwrap();
        assert!(
            seen[0].0.starts_with(
                "POST /v1/internal/formation/explorations/search_123/proposal/status "
            )
        );
        assert!(seen[1].0.contains("/proposal/claim "));
        assert_eq!(seen[1].1["claimant_id"], CLAIMANT);
        assert_eq!(seen[1].1["lease"]["generation"], 2);
        assert_eq!(seen[2].1["submission"]["outcome"], "timeout");
    }
    #[test]
    fn rejects_untrusted_origin_and_scope() {
        for origin in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com/?q=1",
        ] {
            assert!(HostedProposalHttp::new(origin, "token", scope()).is_err());
        }
        let mut bad = scope();
        bad.satisfy = "../other";
        assert!(HostedProposalHttp::new("https://example.com", "token", bad).is_err());
    }
}
