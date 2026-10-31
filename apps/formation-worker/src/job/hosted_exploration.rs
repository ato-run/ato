//! Scoped source, lease and journal transport for a confirmed hosted Search.
use super::{SourceFetcher, source_fetch};
use anyhow::{Result, bail, ensure};
use reqwest::{Url, blocking::Client};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorationWork {
    pub owner: String,
    pub search: String,
    pub satisfy_id: String,
    pub holder: String,
    pub claimant: String,
    pub generation: u64,
    pub expires_at: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkResponse {
    work: Option<ExplorationWork>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorationContext {
    pub request: crate::runtime_network::SatisfyRequest,
    pub source_origin: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseResponse {
    claimant_id: String,
    lease_holder: String,
    lease_generation: u64,
    lease_expires_at: String,
}
pub struct HostedExplorationApi {
    client: Client,
    origin: String,
    endpoint: Url,
    token: String,
    #[cfg(test)]
    mock: bool,
}
fn id(value: &str, min: usize, max: usize) -> bool {
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
impl HostedExplorationApi {
    pub fn new(api: &str, token: &str) -> Result<Self> {
        Self::build(api, token, false)
    }
    fn build(api: &str, token: &str, mock: bool) -> Result<Self> {
        let mut endpoint = Url::parse(api)?;
        let local = endpoint.scheme() == "http"
            && matches!(endpoint.host_str(), Some("127.0.0.1" | "[::1]"));
        ensure!(
            (!mock && endpoint.scheme() == "https") || (mock && local),
            "hosted exploration origin invalid"
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
            "hosted exploration configuration invalid"
        );
        let origin = endpoint.origin().ascii_serialization();
        endpoint.set_path("/v1/internal/formation/explorations/");
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
            origin,
            endpoint,
            token: token.into(),
            #[cfg(test)]
            mock,
        })
    }
    fn call<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        cap: u64,
    ) -> Result<T> {
        let response = self
            .client
            .post(self.endpoint.join(path)?)
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .map_err(|_| anyhow::anyhow!("hosted exploration transport outcome unknown"))?;
        ensure!(
            response.status().is_success(),
            "hosted exploration operation refused"
        );
        let mut bytes = Vec::new();
        response.take(cap + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= cap,
            "hosted exploration response too large"
        );
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("hosted exploration response invalid"))
    }
    pub fn claim(&self, holder: &str) -> Result<Option<ExplorationWork>> {
        ensure!(uuid(holder), "hosted exploration holder invalid");
        let response: WorkResponse = self.call("claim-next", &json!({"holder":holder}), 65536)?;
        if let Some(work) = &response.work {
            work.validate()?;
            ensure!(work.holder == holder, "hosted exploration holder mismatch");
        }
        Ok(response.work)
    }
    pub fn context(&self, work: &ExplorationWork) -> Result<ExplorationContext> {
        work.validate()?;
        let context: ExplorationContext = self.call(
            &format!("{}/context", work.search),
            &work.scoped(),
            256 * 1024,
        )?;
        ensure!(
            context.request.search_id == work.search,
            "hosted exploration context mismatch"
        );
        Ok(context)
    }
    /// A renewal that changes generation is a takeover, not authority to
    /// continue this process's existing proposal or journal operation.
    pub fn renew(&self, work: &ExplorationWork) -> Result<()> {
        work.validate()?;
        let response: LeaseResponse = self.call(
            &format!("{}/provider-journal/claim", work.search),
            &json!({"owner":work.owner,"holder":work.holder}),
            4096,
        )?;
        ensure!(
            response.lease_holder == work.holder
                && response.claimant_id == work.claimant
                && response.lease_generation == work.generation
                && !response.lease_expires_at.is_empty(),
            "hosted exploration lease changed"
        );
        Ok(())
    }
    pub fn initialize(&self, work: &ExplorationWork) -> Result<()> {
        work.validate()?;
        let (config, plan) = super::hosted_preparation::provider_profile();
        let prompt = format!(
            "sha256:{:x}",
            Sha256::digest(crate::runtime_network::proposal::deepseek::PROMPT_V3.as_bytes())
        );
        let body = json!({"owner":work.owner,"lease":work.lease(),"plan":plan,"provider_config":config,"prompt_digest":prompt});
        let result: Value = self.call(
            &format!("{}/provider-journal/initialize", work.search),
            &body,
            4096,
        )?;
        ensure!(
            result["created"].is_boolean() && result["revision"].as_u64().is_some(),
            "hosted exploration journal invalid"
        );
        Ok(())
    }
    pub fn source_fetcher<'a>(
        &'a self,
        work: &'a ExplorationWork,
        context: &'a ExplorationContext,
    ) -> ExplorationSource<'a> {
        ExplorationSource {
            api: self,
            work,
            context,
        }
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn token(&self) -> &str {
        &self.token
    }

    /// One Search at a time. All decisions, claims and verification stay on
    /// the common Coordinator/Rust paths. A local stop only ends this driver;
    /// it does not invent a durable cancellation or a successful receipt.
    pub fn drive_one(
        &self,
        holder: &str,
        work_root: &Path,
        should_stop: impl Fn() -> bool,
    ) -> Result<bool> {
        use crate::runtime_network::{
            exploration_driver::{DriverStep, drive_step},
            proposal::{
                hosted_anthropic::{ClaimBoundAnthropicProducer, ClaimBoundScope},
                hosted_proposal_http::{HostedProposalHttp, HostedProposalScope},
                serve_general_proposal,
            },
        };
        use std::{
            sync::Arc,
            time::{SystemTime, UNIX_EPOCH},
        };
        if should_stop() {
            return Ok(false);
        }
        let Some(work) = self.claim(holder)? else {
            return Ok(false);
        };
        if should_stop() {
            return Ok(true);
        }
        let context = self.context(&work)?;
        let mut submission = self.prepare_submission(&work, &context, work_root)?;
        self.renew(&work)?;
        self.initialize(&work)?;
        let scope = || HostedProposalScope {
            owner: &work.owner,
            search: &work.search,
            satisfy: &work.satisfy_id,
            holder: &work.holder,
            claimant: &work.claimant,
            generation: work.generation,
        };
        #[cfg(test)]
        let coordinator = if self.mock {
            HostedProposalHttp::for_test(self.origin(), self.token(), scope())?
        } else {
            HostedProposalHttp::new(self.origin(), self.token(), scope())?
        };
        #[cfg(not(test))]
        let coordinator = HostedProposalHttp::new(self.origin(), self.token(), scope())?;
        let mut fixed_deadline: Option<(u64, Instant)> = None;
        loop {
            if should_stop() {
                return Ok(true);
            }
            self.renew(&work)?;
            let status = coordinator.status()?;
            ensure!(
                status["search_id"] == work.search && status["satisfy_id"] == work.satisfy_id,
                "hosted exploration status mismatch"
            );
            let deadline = status["search_state"]["deadline_ms"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("hosted exploration deadline missing"))?;
            let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
            if let Some((original, _)) = fixed_deadline {
                ensure!(deadline == original, "hosted exploration deadline changed");
            } else {
                let remaining =
                    u64::try_from(u128::from(deadline).saturating_sub(now_ms)).unwrap_or(u64::MAX);
                fixed_deadline = Some((
                    deadline,
                    Instant::now() + Duration::from_millis(remaining.min(7 * 24 * 60 * 60 * 1000)),
                ));
            }
            let monotonic = fixed_deadline.expect("set above").1;
            let (config, plan) = super::hosted_preparation::provider_profile();
            let producer = Arc::new(ClaimBoundAnthropicProducer::new(
                config,
                plan,
                ClaimBoundScope {
                    api: self.origin(),
                    token: self.token(),
                    owner: &work.owner,
                    search: &work.search,
                    holder: &work.holder,
                    claimant: &work.claimant,
                    generation: work.generation,
                },
            )?);
            let step = drive_step(
                &status,
                || {
                    Ok(should_stop()
                        || Instant::now() >= monotonic
                        || SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
                            >= u128::from(deadline))
                },
                || Ok(()),
                || {
                    serve_general_proposal(
                        &coordinator,
                        &work.satisfy_id,
                        &status,
                        &mut submission,
                        &work.claimant,
                        producer,
                    )?;
                    Ok(())
                },
            )?;
            if !matches!(step, DriverStep::Pending) {
                return Ok(true);
            }
            for _ in 0..8 {
                if should_stop() {
                    return Ok(true);
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }

    fn prepare_submission(
        &self,
        work: &ExplorationWork,
        context: &ExplorationContext,
        root: &Path,
    ) -> Result<crate::runtime_network::Submission> {
        use super::source_intake::prepare_source_proposal;
        let result = &context.source_origin["result"];
        let d = &result["archive_descriptor"];
        let job_id = result["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted source job missing"))?;
        let digest = d["source_archive_digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted source digest missing"))?;
        let tree = d["source_tree_digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted source tree missing"))?;
        ensure!(
            id(job_id, 6, 128) && job_id.starts_with("fjob_") && d["subdirectory"] == "",
            "hosted source selection invalid"
        );
        let job: ato_ipc::formation::FormationJobV1 = serde_json::from_value(json!({
            "protocol":"ato.formation-job.v1","job_id":"fjob_hosted_exploration","request_idempotency_key":"hosted-exploration-source",
            "source":{"kind":"uploaded_archive","archive_ref":format!("formation-source-archive:{job_id}"),"expected_archive_digest":digest,
                "expected_source_tree_digest":tree,"subdirectory":""},
            "requested_outputs":["process_workspace"],"target":{"triple":"x86_64-linux-gnu","workspace_guest_root":"/app"},
            "policy":{"policy_version":"formation-v1","network":"denied","publish_enabled":false}
        }))?;
        prepare_source_proposal(
            &self.source_fetcher(work, context),
            &job,
            context.request.clone(),
            root,
        )
    }
}
impl ExplorationWork {
    fn validate(&self) -> Result<()> {
        ensure!(
            id(&self.owner, 1, 256)
                && id(&self.search, 8, 128)
                && id(&self.satisfy_id, 1, 256)
                && uuid(&self.holder)
                && uuid(&self.claimant)
                && self.generation > 0
                && !self.expires_at.is_empty(),
            "hosted exploration work invalid"
        );
        Ok(())
    }
    fn lease(&self) -> Value {
        json!({"holder":self.holder,"claimant":self.claimant,"generation":self.generation})
    }
    fn scoped(&self) -> Value {
        json!({"owner":self.owner,"lease":self.lease(),"satisfy_id":self.satisfy_id})
    }
}
pub struct ExplorationSource<'a> {
    api: &'a HostedExplorationApi,
    work: &'a ExplorationWork,
    context: &'a ExplorationContext,
}
impl SourceFetcher for ExplorationSource<'_> {
    fn fetch(&self, _job_id: &str, _source: &Value) -> Result<Vec<u8>> {
        bail!("hosted exploration requires bounded file intake")
    }
    fn fetch_file(
        &self,
        _job_id: &str,
        source: &Value,
        root: &Path,
        maximum: u64,
        deadline: Instant,
    ) -> Result<(File, String)> {
        self.work.validate()?;
        let result = &self.context.source_origin["result"];
        let descriptor = &result["archive_descriptor"];
        let job = result["job_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted source invalid"))?;
        let digest = descriptor["source_archive_digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted source invalid"))?;
        let tree = descriptor["source_tree_digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted source invalid"))?;
        let size = descriptor["size_bytes"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("hosted source invalid"))?;
        ensure!(
            id(job, 6, 128)
                && job.starts_with("fjob_")
                && source["archive_ref"] == format!("formation-source-archive:{job}")
                && source["expected_archive_digest"] == digest
                && source["expected_source_tree_digest"] == tree
                && source["subdirectory"] == ""
                && size > 0
                && size <= maximum
                && size <= 16 * 1024 * 1024,
            "hosted source mismatch"
        );
        let request = self
            .api
            .client
            .post(
                self.api
                    .endpoint
                    .join(&format!("{}/source", self.work.search))?,
            )
            .bearer_auth(&self.api.token)
            .json(&self.work.scoped());
        let (file, observed) = source_fetch::download_file(request, root, size, deadline)?;
        ensure!(
            file.metadata()?.len() == size && observed == digest,
            "hosted source digest mismatch"
        );
        Ok((file, observed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, thread};
    const HOLDER: &str = "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa";
    const CLAIMANT: &str = "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb";
    #[test]
    fn confirmed_search_uses_scoped_context_source_lease_and_journal() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let digest = format!("sha256:{:x}", Sha256::digest(b"abc"));
        let expected = digest.clone();
        let tree = format!("sha256:{}", "d".repeat(64));
        let expected_tree = tree.clone();
        let server = thread::spawn(move || {
            let mut seen = Vec::new();
            for turn in 0..5 {
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
                let response=match turn {
                    0=>json!({"work":{"owner":"owner_a","search":"search_123","satisfy_id":"sat_1","holder":HOLDER,"claimant":CLAIMANT,"generation":1,"expires_at":"later"}}).to_string(),
                    1=>{let mut request:Value=serde_json::from_str(include_str!("../../tests/fixtures/hosted-prepared-request.json")).unwrap();
                        request["source"]["content_ref"]=json!(digest);request["source"]["archive_digest"]=json!(digest);request["source"]["archive_bytes"]=json!(3);
                        request["initial_source"]["archive_digest"]=json!(digest);
                        json!({"request":request,"source_origin":{"result":{"job_id":"fjob_source","archive_descriptor":{
                            "source_archive_digest":digest,"source_tree_digest":tree,"size_bytes":3}}}}).to_string()},
                    2=>json!({"claimant_id":CLAIMANT,"lease_holder":HOLDER,"lease_generation":1,"lease_expires_at":"later"}).to_string(),
                    3=>json!({"created":true,"revision":0}).to_string(),_=>"abc".into(),
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
        let api = HostedExplorationApi::build(&origin, "worker-token", true).unwrap();
        let work = api.claim(HOLDER).unwrap().unwrap();
        let context = api.context(&work).unwrap();
        api.renew(&work).unwrap();
        api.initialize(&work).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&root).unwrap();
        let root = tempfile::tempdir_in(root).unwrap();
        let source = json!({"archive_ref":"formation-source-archive:fjob_source","expected_archive_digest":expected,"expected_source_tree_digest":expected_tree,"subdirectory":""});
        let (mut file, observed) = api
            .source_fetcher(&work, &context)
            .fetch_file(
                "job",
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
        let seen = server.join().unwrap();
        assert!(seen[0].0.contains("/explorations/claim-next "));
        assert!(seen[1].0.contains("/explorations/search_123/context "));
        assert!(seen[2].0.contains("/provider-journal/claim "));
        assert!(seen[3].0.contains("/provider-journal/initialize "));
        assert!(seen[4].0.contains("/explorations/search_123/source "));
        assert_eq!(seen[3].1["plan"]["ceiling_usd_micros"], 113664);
    }
    #[test]
    fn rejects_untrusted_origins() {
        for origin in [
            "http://example.com",
            "https://user@example.com",
            "https://example.com/path",
            "https://example.com/?q=1",
        ] {
            assert!(HostedExplorationApi::new(origin, "token").is_err());
        }
    }
    #[test]
    fn stop_before_claim_never_contacts_the_service() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let api = HostedExplorationApi::build(
            &format!("http://{}", listener.local_addr().unwrap()),
            "worker-token",
            true,
        )
        .unwrap();
        assert!(!api.drive_one(HOLDER, Path::new("."), || true).unwrap());
        assert!(listener.accept().is_err());
    }
    #[test]
    fn rebuilds_the_frozen_search_and_stops_on_a_terminal_observation() {
        use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let wire: Value =
                serde_json::from_str(include_str!("../../tests/fixtures/hosted-source-wire.json"))
                    .unwrap();
            let archive = BASE64
                .decode(wire["archive_base64"].as_str().unwrap())
                .unwrap();
            let request: Value = serde_json::from_str(include_str!(
                "../../tests/fixtures/hosted-prepared-request.json"
            ))
            .unwrap();
            let mut paths = Vec::new();
            for turn in 0..7 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
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
                paths.push(headers.lines().next().unwrap().to_owned());
                let response:Vec<u8>=match turn {
                    0=>json!({"work":{"owner":"owner_a","search":"search_123","satisfy_id":"sat_1","holder":HOLDER,"claimant":CLAIMANT,"generation":1,"expires_at":"later"}}).to_string().into_bytes(),
                    1=>json!({"request":request,"source_origin":{"result":{"job_id":"fjob_source","archive_descriptor":{
                        "source_archive_digest":wire["archive_digest"],"source_tree_digest":wire["source_tree_digest"],"source_closure_ref":wire["source_closure_ref"],
                        "size_bytes":wire["size_bytes"],"subdirectory":""}}}}).to_string().into_bytes(),
                    2=>archive.clone(),
                    3|5=>json!({"claimant_id":CLAIMANT,"lease_holder":HOLDER,"lease_generation":1,"lease_expires_at":"later"}).to_string().into_bytes(),
                    4=>json!({"created":true,"revision":0}).to_string().into_bytes(),
                    _=>{let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        json!({"search_id":"search_123","satisfy_id":"sat_1","status":"satisfied","search_state":{"deadline_ms":now+60000}}).to_string().into_bytes()},
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                )
                .unwrap();
                stream.write_all(&response).unwrap();
            }
            paths
        });
        let api = HostedExplorationApi::build(&origin, "worker-token", true).unwrap();
        let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let root = tempfile::tempdir_in(scratch).unwrap();
        assert!(api.drive_one(HOLDER, root.path(), || false).unwrap());
        let paths = server.join().unwrap();
        assert!(paths[2].contains("/source "));
        assert!(paths[4].contains("/provider-journal/initialize "));
        assert!(paths[6].contains("/proposal/status "));
        assert!(!paths.iter().any(|p| p.contains("provider-inference")));
    }
}
