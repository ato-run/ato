//! Scoped transport for read-only hosted source preparation. A claim does not
//! permit inference or execution. Source interpretation stays in Rust intake.
use super::{SourceFetcher, source_fetch};
use anyhow::{Result, bail, ensure};
use reqwest::{Url, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationClaim {
    pub id: String,
    pub owner: String,
    pub search_id: String,
    pub holder: String,
    pub generation: u64,
    pub expires_at: String,
    pub intent: Value,
    pub source_origin: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimResult {
    work: Option<PreparationClaim>,
}
pub struct HostedPreparationApi {
    client: Client,
    endpoint: Url,
    token: String,
}
fn identifier(value: &str, min: usize, max: usize) -> bool {
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
impl HostedPreparationApi {
    pub fn new(api: &str, token: &str) -> Result<Self> {
        Self::build(api, token, false)
    }
    fn build(api: &str, token: &str, mock: bool) -> Result<Self> {
        let mut endpoint = Url::parse(api)?;
        let local = endpoint.scheme() == "http"
            && matches!(endpoint.host_str(), Some("127.0.0.1" | "[::1]"));
        ensure!(
            (!mock && endpoint.scheme() == "https") || (mock && local),
            "hosted preparation origin invalid"
        );
        ensure!(
            endpoint.host_str().is_some()
                && endpoint.path() == "/"
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && !token.is_empty()
                && token.len() <= 512
                && token.bytes().all(|b| b.is_ascii_graphic()),
            "hosted preparation configuration invalid"
        );
        endpoint.set_path("/v1/internal/formation/exploration-preparations/");
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            endpoint,
            token: token.into(),
        })
    }
    pub fn claim(&self, holder: &str) -> Result<Option<PreparationClaim>> {
        ensure!(uuid(holder), "hosted preparation holder invalid");
        let response = self
            .client
            .post(self.endpoint.join("claim")?)
            .bearer_auth(&self.token)
            .json(&json!({"holder":holder}))
            .send()
            .map_err(|_| anyhow::anyhow!("hosted preparation claim outcome unknown"))?;
        ensure!(
            response.status().is_success(),
            "hosted preparation claim refused"
        );
        let mut bytes = Vec::new();
        response.take(65537).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 65536,
            "hosted preparation claim response too large"
        );
        let result: ClaimResult = serde_json::from_slice(&bytes)?;
        if let Some(work) = &result.work {
            ensure!(
                work.id.strip_prefix("hprep_").is_some_and(uuid)
                    && work.holder == holder
                    && work.generation > 0
                    && identifier(&work.owner, 1, 256)
                    && identifier(&work.search_id, 8, 128),
                "hosted preparation claim response invalid"
            );
        }
        Ok(result.work)
    }
    pub fn source_fetcher<'a>(
        &'a self,
        claim: &'a PreparationClaim,
    ) -> HostedPreparationSource<'a> {
        HostedPreparationSource { api: self, claim }
    }
    pub fn prepare_one(&self, holder: &str, work_root: &Path) -> Result<bool> {
        let Some(claim) = self.claim(holder)? else {
            return Ok(false);
        };
        let submission = match prepare_claim(&self.source_fetcher(&claim), &claim, work_root) {
            Ok(submission) => submission,
            Err(error) => {
                let _ = self.fail(&claim);
                return Err(error);
            }
        };
        self.complete(&claim, &submission.request)?;
        Ok(true)
    }

    /// A lost response is ambiguous. Do not produce or send a replacement;
    /// the owner reads the durable preparation status from the service.
    pub fn complete(
        &self,
        claim: &PreparationClaim,
        request: &crate::runtime_network::SatisfyRequest,
    ) -> Result<()> {
        self.complete_value(claim, serde_json::to_value(request)?)
    }
    fn complete_value(&self, claim: &PreparationClaim, request: Value) -> Result<()> {
        ensure!(
            claim.id.strip_prefix("hprep_").is_some_and(uuid)
                && uuid(&claim.holder)
                && claim.generation > 0,
            "hosted preparation claim invalid"
        );
        let body = json!({"id":claim.id,"holder":claim.holder,"generation":claim.generation,"request":request});
        ensure!(
            serde_json::to_vec(&body)?.len() <= 256 * 1024,
            "hosted preparation result too large"
        );
        let response = self
            .client
            .post(self.endpoint.join("complete")?)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .map_err(|_| anyhow::anyhow!("hosted preparation completion outcome unknown"))?;
        ensure!(
            response.status().is_success(),
            "hosted preparation completion refused"
        );
        let mut bytes = Vec::new();
        response.take(4097).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 4096
                && serde_json::from_slice::<Value>(&bytes)? == json!({"prepared":true}),
            "hosted preparation completion response invalid; outcome unknown"
        );
        Ok(())
    }
    fn fail(&self, claim: &PreparationClaim) -> Result<()> {
        ensure!(
            claim.id.strip_prefix("hprep_").is_some_and(uuid)
                && uuid(&claim.holder)
                && claim.generation > 0,
            "hosted preparation claim invalid"
        );
        let response = self
            .client
            .post(self.endpoint.join("fail")?)
            .bearer_auth(&self.token)
            .json(&json!({"id":claim.id,"holder":claim.holder,"generation":claim.generation}))
            .send()
            .map_err(|_| anyhow::anyhow!("hosted preparation failure report outcome unknown"))?;
        ensure!(
            response.status().is_success(),
            "hosted preparation failure report refused"
        );
        let mut bytes = Vec::new();
        response.take(4097).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 4096
                && serde_json::from_slice::<Value>(&bytes)? == json!({"failed":true}),
            "hosted preparation failure report response invalid"
        );
        Ok(())
    }
}
pub struct HostedPreparationSource<'a> {
    api: &'a HostedPreparationApi,
    claim: &'a PreparationClaim,
}

/// Fixed first hosted profile; the Coordinator and provider proxy independently
/// enforce the same ceilings and runtime admission remains authoritative.
pub fn provider_profile() -> (
    crate::runtime_network::proposal::hosted_anthropic::HostedAnthropicConfig,
    crate::runtime_network::proposal::budget::BudgetPlan,
) {
    use crate::runtime_network::proposal::{
        budget::BudgetPlan, deepseek::PROMPT_VERSION_V3, hosted_anthropic::HostedAnthropicConfig,
    };
    (
        HostedAnthropicConfig {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            endpoint: "https://api.anthropic.com".into(),
            prompt_version: PROMPT_VERSION_V3.into(),
            max_output_tokens: 1024,
            timeout_ms: 30_000,
        },
        BudgetPlan {
            max_calls: 1,
            input_token_cap: 32768,
            output_token_cap: 1024,
            input_price: 3_000_000,
            output_price: 15_000_000,
            ceiling_usd_micros: 113_664,
        },
    )
}

/// No request is posted here. The caller must save the result under the live
/// preparation claim and wait for the owner's exact Search confirmation.
pub fn prepare_claim(
    fetcher: &dyn SourceFetcher,
    claim: &PreparationClaim,
    work_root: &Path,
) -> Result<crate::runtime_network::Submission> {
    use super::source_intake::prepare_source_exploration;
    use crate::runtime_network::{
        RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy,
        exploration_driver::hosted_budget_binding,
    };
    use std::collections::BTreeMap;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Intent {
        path: String,
        status: u16,
    }
    let intent: Intent = serde_json::from_value(claim.intent.clone())?;
    ensure!(
        intent.status == 200
            && !intent.path.is_empty()
            && intent.path.len() <= 512
            && intent.path.starts_with('/')
            && intent
                .path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_~.%-".contains(&b)),
        "hosted preparation intent invalid"
    );
    let source = &claim.source_origin["result"];
    let descriptor = &source["archive_descriptor"];
    let job_id = source["job_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("hosted source job missing"))?;
    let digest = descriptor["source_archive_digest"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("hosted source digest missing"))?;
    let tree = descriptor["source_tree_digest"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("hosted source tree missing"))?;
    let closure = descriptor["source_closure_ref"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("hosted source closure missing"))?;
    ensure!(
        identifier(job_id, 6, 128)
            && job_id.starts_with("fjob_")
            && descriptor["subdirectory"] == "",
        "hosted source selection invalid"
    );
    let job: ato_ipc::formation::FormationJobV1 = serde_json::from_value(json!({
        "protocol":"ato.formation-job.v1","job_id":"fjob_hosted_preparation","request_idempotency_key":"hosted-preparation-source",
        "source":{"kind":"uploaded_archive","archive_ref":format!("formation-source-archive:{job_id}"),"expected_archive_digest":digest,
            "expected_source_tree_digest":tree,"subdirectory":""},
        "requested_outputs":["process_workspace"],"target":{"triple":"x86_64-linux-gnu","workspace_guest_root":"/app"},
        "policy":{"policy_version":"formation-v1","network":"denied","publish_enabled":false}
    }))?;
    let contract = serde_json::from_value(
        json!({"schema":"ato.contract/1","requirements":[{"id":"root","verifier":"ato.contract.http@1",
        "port":"app.http","method":"GET","path":intent.path,"status":intent.status}]}),
    )?;
    let (config, plan) = provider_profile();
    let exploration = serde_json::from_value(
        json!({"ceiling":{},"formation":{"max_rounds":1,"max_retries":0},
        "max_provider_calls":1,"max_inspections":0,"max_provider_cost_usd_micros":plan.ceiling_usd_micros,
        "max_provider_input_tokens":plan.input_token_cap,"max_provider_output_tokens":plan.output_token_cap,
        "provider_configuration_ref":config.configuration_ref(&plan)?,"provider_budget_binding_ref":hosted_budget_binding(&claim.owner,&claim.search_id)?}),
    )?;
    let policy = SatisfyPolicy {
        network: "denied".into(),
        allow_managed: true,
        decision: None,
        generation: None,
        proposal: None,
        exploration: Some(exploration),
    };
    let budget = SatisfyBudget {
        mode: "first_pass".into(),
        max_attempts: 1,
        deadline_seconds: 600,
        max_transfer_bytes: 16 * 1024 * 1024,
        max_expanded_bytes: 128 * 1024 * 1024,
        max_stored_bytes: 128 * 1024 * 1024,
    };
    let submission = prepare_source_exploration(
        fetcher,
        &job,
        contract,
        work_root,
        RuntimeConstraintWire::Any,
        policy,
        budget,
        &claim.search_id,
        BTreeMap::from([("python".into(), "3.12.7".into())]),
    )?;
    ensure!(
        submission.request.source.closure_ref == closure
            && submission.request.source.archive_digest == digest
            && submission.request.authorized_derivations.is_empty(),
        "hosted prepared source mismatch"
    );
    Ok(submission)
}
impl SourceFetcher for HostedPreparationSource<'_> {
    fn fetch(&self, _job_id: &str, _source: &Value) -> Result<Vec<u8>> {
        bail!("hosted preparation requires bounded file intake")
    }
    fn fetch_file(
        &self,
        _job_id: &str,
        source: &Value,
        work_root: &Path,
        maximum: u64,
        deadline: Instant,
    ) -> Result<(File, String)> {
        let result = &self.claim.source_origin["result"];
        let descriptor = &result["archive_descriptor"];
        let job = result["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted preparation source invalid"))?;
        let digest = descriptor["source_archive_digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted preparation source invalid"))?;
        let tree = descriptor["source_tree_digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted preparation source invalid"))?;
        let size = descriptor["size_bytes"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("hosted preparation source invalid"))?;
        ensure!(
            identifier(job, 1, 128)
                && job.starts_with("fjob_")
                && source["archive_ref"] == format!("formation-source-archive:{job}")
                && source["expected_archive_digest"] == digest
                && source["expected_source_tree_digest"] == tree
                && source["subdirectory"] == ""
                && size > 0
                && size <= maximum
                && size <= 16 * 1024 * 1024,
            "hosted preparation source mismatch"
        );
        let request=self.api.client.post(self.api.endpoint.join("source")?).bearer_auth(&self.api.token)
            .json(&json!({"id":self.claim.id,"holder":self.claim.holder,"generation":self.claim.generation}));
        let (file, observed) = source_fetch::download_file(request, work_root, size, deadline)?;
        ensure!(
            file.metadata()?.len() == size && observed == digest,
            "hosted preparation source digest mismatch"
        );
        Ok((file, observed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{io::Write, net::TcpListener, thread};
    const HOLDER: &str = "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa";
    #[test]
    fn streams_only_the_claimed_source_and_rechecks_actual_digest() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let digest = format!("sha256:{:x}", Sha256::digest(b"abc"));
        let expected = digest.clone();
        let tree = format!("sha256:{}", "d".repeat(64));
        let expected_tree = tree.clone();
        let server = thread::spawn(move || {
            let mut seen = Vec::new();
            for turn in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                let start = loop {
                    let n = stream.read(&mut buf).unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&buf[..n]);
                    if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let headers = String::from_utf8(data[..start].to_vec()).unwrap();
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|s| s.parse().ok())
                    })
                    .unwrap();
                while data.len() < start + size {
                    let n = stream.read(&mut buf).unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&buf[..n]);
                }
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer worker-token")
                );
                assert!(
                    !String::from_utf8_lossy(&data[start..start + size]).contains("worker-token")
                );
                seen.push((
                    headers.lines().next().unwrap().to_owned(),
                    serde_json::from_slice::<Value>(&data[start..start + size]).unwrap(),
                ));
                let response = if turn == 0 {
                    serde_json::to_string(&json!({"work":{"id":format!("hprep_{HOLDER}"),"owner":"owner_a","search_id":"search_123",
                    "holder":HOLDER,"generation":1,"expires_at":"2026-10-31T00:00:00Z","intent":{"path":"/","status":200},
                    "source_origin":{"result":{"job_id":"fjob_source","archive_descriptor":{"source_archive_digest":digest,"source_tree_digest":tree,"size_bytes":3}}}}})).unwrap()
                } else if turn == 1 {
                    "abc".into()
                } else {
                    r#"{"prepared":true}"#.into()
                };
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
        let api = HostedPreparationApi::build(&origin, "worker-token", true).unwrap();
        let claim = api.claim(HOLDER).unwrap().unwrap();
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let root = tempfile::tempdir_in(scratch).unwrap();
        let source = json!({"archive_ref":"formation-source-archive:fjob_source","expected_archive_digest":expected,"expected_source_tree_digest":expected_tree,"subdirectory":""});
        let fetcher = api.source_fetcher(&claim);
        let mut bad = source.clone();
        bad["expected_archive_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
        assert!(
            fetcher
                .fetch_file(
                    "fjob",
                    &bad,
                    root.path(),
                    100,
                    Instant::now() + Duration::from_secs(5)
                )
                .is_err()
        );
        let (mut file, observed) = fetcher
            .fetch_file(
                "fjob",
                &source,
                root.path(),
                100,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abc");
        assert_eq!(observed, expected);
        api.complete_value(&claim, json!({"fixture":"request"}))
            .unwrap();
        let seen = server.join().unwrap();
        assert!(seen[0].0.contains("/exploration-preparations/claim "));
        assert!(seen[1].0.contains("/exploration-preparations/source "));
        assert_eq!(seen[1].1["generation"], 1);
        assert!(seen[2].0.contains("/exploration-preparations/complete "));
        assert_eq!(seen[2].1["request"]["fixture"], "request");
    }
    #[test]
    fn rejects_untrusted_origins() {
        for origin in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com/?q=1",
        ] {
            assert!(HostedPreparationApi::new(origin, "token").is_err());
        }
    }
    #[test]
    fn preparation_failure_reports_no_source_diagnostics() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut reported = Value::Null;
            for turn in 0..2 {
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
                    if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
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
                if turn == 1 {
                    assert!(
                        headers
                            .lines()
                            .next()
                            .unwrap()
                            .contains("/exploration-preparations/fail ")
                    );
                    reported = serde_json::from_slice(&data[start..start + size]).unwrap();
                }
                let response = if turn == 0 {
                    serde_json::to_string(&json!({"work":{"id":format!("hprep_{HOLDER}"),"owner":"owner_a","search_id":"search_123",
                    "holder":HOLDER,"generation":1,"expires_at":"2026-10-31T00:00:00Z","intent":{"path":"https://untrusted.invalid","status":200},"source_origin":{}}})).unwrap()
                } else {
                    r#"{"failed":true}"#.to_owned()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .unwrap();
            }
            reported
        });
        let api = HostedPreparationApi::build(&origin, "worker-token", true).unwrap();
        assert!(api.prepare_one(HOLDER, Path::new(".")).is_err());
        assert_eq!(
            server.join().unwrap(),
            json!({"id":format!("hprep_{HOLDER}"),"holder":HOLDER,"generation":1})
        );
    }
}
