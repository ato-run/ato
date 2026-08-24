use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_computation::ContentRef;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};

use crate::{
    GraphDownloadExpectation, RuntimeGraphSource, RuntimeGraphValidationReport,
    download_and_validate_graph,
};

#[derive(Debug, Clone)]
pub struct ValidatorAgentConfig {
    pub api_url: String,
    pub token: String,
    pub agent_id: String,
    pub work_root: PathBuf,
    pub poll_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatorRunOutcome {
    Idle,
    Verified {
        graph_id: String,
        bundle_id: String,
    },
    Rejected {
        graph_id: String,
        rejection_code: String,
    },
}

pub struct ValidatorAgent {
    api: HttpValidatorApi,
    work_root: PathBuf,
    poll_interval: Duration,
}

impl ValidatorAgent {
    pub fn new(config: ValidatorAgentConfig) -> Result<Self> {
        let api = HttpValidatorApi::new(&config.api_url, config.token, config.agent_id)?;
        Ok(Self {
            api,
            work_root: config.work_root,
            poll_interval: config.poll_interval,
        })
    }

    pub fn run_once(&self) -> Result<ValidatorRunOutcome> {
        let Some(job) = self.api.claim()? else {
            return Ok(ValidatorRunOutcome::Idle);
        };
        let source = ClaimedJobSource {
            api: &self.api,
            job: &job,
        };
        let expectation = GraphDownloadExpectation {
            index_digest: job.bundle_index_digest.clone(),
            root_computation_ref: job.root_computation_ref.clone(),
            object_count: job.object_count,
            logical_bytes: job.logical_bytes,
        };
        match download_and_validate_graph(&source, &expectation, &self.work_root) {
            Ok(graph) => {
                let response = self.api.ack_verified(&job, graph.report())?;
                if response.validation_status != "ready"
                    || response.root_computation_ref.as_deref()
                        != Some(job.root_computation_ref.as_str())
                {
                    bail!("validator verified ack did not preserve graph root and ready status");
                }
                Ok(ValidatorRunOutcome::Verified {
                    graph_id: job.graph_id,
                    bundle_id: response
                        .bundle_id
                        .context("validator ack omitted ready bundle id")?,
                })
            }
            Err(error) => {
                let rejection_code = classify_rejection(&error).to_owned();
                let response = self.api.ack_rejected(&job, &rejection_code)?;
                if response.validation_status != "rejected" || response.bundle_id.is_some() {
                    bail!("validator rejection ack returned an invalid terminal state");
                }
                Ok(ValidatorRunOutcome::Rejected {
                    graph_id: job.graph_id,
                    rejection_code,
                })
            }
        }
    }

    pub fn run_forever(&self) -> Result<()> {
        loop {
            if self.run_once()? == ValidatorRunOutcome::Idle {
                thread::sleep(self.poll_interval);
            }
        }
    }
}

pub struct HttpValidatorApi {
    client: Client,
    base_url: String,
    token: String,
    agent_id: HeaderValue,
}

impl HttpValidatorApi {
    pub fn new(base_url: &str, token: String, agent_id: String) -> Result<Self> {
        let parsed = reqwest::Url::parse(base_url).context("invalid validator API URL")?;
        if parsed.scheme() != "https" && parsed.host_str() != Some("localhost") {
            bail!("validator API must use HTTPS except for localhost");
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("validator API URL cannot contain a query or fragment");
        }
        if token.trim().is_empty() {
            bail!("validator token cannot be empty");
        }
        let agent_id = HeaderValue::from_str(&agent_id).context("invalid validator agent id")?;
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .context("failed to construct validator HTTP client")?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            agent_id,
        })
    }

    fn authenticated(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.token)
    }

    fn claimed(&self, request: RequestBuilder, claim_id: &str) -> RequestBuilder {
        self.authenticated(request)
            .header("x-ato-validator-agent-id", self.agent_id.clone())
            .header("x-ato-validation-claim-id", claim_id)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn claim(&self) -> Result<Option<ValidationJob>> {
        let response = self
            .authenticated(
                self.client
                    .post(self.url("/v1/capsule-object-graphs/validation-jobs/claim"))
                    .json(&serde_json::json!({
                        "agent_id": self.agent_id.to_str().expect("validated agent id")
                    })),
            )
            .send()
            .context("validator claim request failed")?;
        if response.status().as_u16() == 204 {
            return Ok(None);
        }
        let envelope: ValidationJobEnvelope = decode_json(response, "validator claim")?;
        Ok(Some(envelope.job))
    }

    fn load_index(&self, job: &ValidationJob) -> Result<Vec<u8>> {
        let response = self
            .claimed(self.client.get(self.url(&job.index_url)), &job.claim_id)
            .send()
            .context("validator index request failed")?;
        decode_bytes(response, "validator index")
    }

    fn load_object(
        &self,
        job: &ValidationJob,
        reference: &ContentRef,
        expected_size: u64,
    ) -> Result<Vec<u8>> {
        let path = format!(
            "/v1/capsule-object-graphs/validation-jobs/{}/objects/{}",
            job.job_id, reference
        );
        let response = self
            .claimed(self.client.get(self.url(&path)), &job.claim_id)
            .send()
            .with_context(|| format!("validator object request failed for {reference}"))?;
        let bytes = decode_bytes(response, "validator object")?;
        if bytes.len() as u64 != expected_size {
            bail!("validator object {reference} length mismatch");
        }
        Ok(bytes)
    }

    fn ack_verified(
        &self,
        job: &ValidationJob,
        report: &RuntimeGraphValidationReport,
    ) -> Result<ValidationAckResponse> {
        self.ack(
            job,
            &ValidationAck::Verified {
                report: report.clone(),
            },
        )
    }

    fn ack_rejected(
        &self,
        job: &ValidationJob,
        rejection_code: &str,
    ) -> Result<ValidationAckResponse> {
        self.ack(
            job,
            &ValidationAck::Rejected {
                rejection_code: rejection_code.to_owned(),
            },
        )
    }

    fn ack(&self, job: &ValidationJob, body: &ValidationAck) -> Result<ValidationAckResponse> {
        let path = format!(
            "/v1/capsule-object-graphs/validation-jobs/{}/ack",
            job.job_id
        );
        let response = self
            .claimed(self.client.post(self.url(&path)).json(body), &job.claim_id)
            .send()
            .context("validator ack request failed")?;
        decode_json(response, "validator ack")
    }
}

struct ClaimedJobSource<'a> {
    api: &'a HttpValidatorApi,
    job: &'a ValidationJob,
}

impl RuntimeGraphSource for ClaimedJobSource<'_> {
    fn load_index(&self) -> Result<Vec<u8>> {
        self.api.load_index(self.job)
    }

    fn load_object(&self, reference: &ContentRef, expected_size: u64) -> Result<Vec<u8>> {
        self.api.load_object(self.job, reference, expected_size)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationJobEnvelope {
    job: ValidationJob,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationJob {
    job_id: String,
    claim_id: String,
    graph_id: String,
    bundle_index_digest: String,
    root_computation_ref: String,
    object_count: usize,
    logical_bytes: u64,
    index_url: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ValidationAck {
    Verified {
        report: RuntimeGraphValidationReport,
    },
    Rejected {
        rejection_code: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationAckResponse {
    validation_status: String,
    #[serde(default)]
    bundle_id: Option<String>,
    #[serde(default)]
    root_computation_ref: Option<String>,
}

fn decode_json<T: for<'de> Deserialize<'de>>(response: Response, operation: &str) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!(
            "{operation} returned {status}: {}",
            body.chars().take(256).collect::<String>()
        );
    }
    response
        .json()
        .with_context(|| format!("{operation} returned malformed JSON"))
}

fn decode_bytes(response: Response, operation: &str) -> Result<Vec<u8>> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!(
            "{operation} returned {status}: {}",
            body.chars().take(256).collect::<String>()
        );
    }
    Ok(response.bytes()?.to_vec())
}

fn classify_rejection(error: &anyhow::Error) -> &'static str {
    let message = format!("{error:#}");
    if message.contains("digest") || message.contains("identity") {
        "hash_mismatch"
    } else if message.contains("root ComputationRef") || message.contains("root is") {
        "root_mismatch"
    } else if message.contains("materialization") || message.contains("RecordFrontier") {
        "invalid_materialization"
    } else if message.contains("closure") || message.contains("reference") {
        "unreachable_objects"
    } else if message.contains("version") {
        "unsupported_version"
    } else {
        "validator_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejection_codes_are_fail_closed() {
        let error = anyhow::anyhow!("VM materialization RecordFrontier closure is invalid");
        assert_eq!(classify_rejection(&error), "invalid_materialization");
        let error = anyhow::anyhow!("downloaded object digest mismatch");
        assert_eq!(classify_rejection(&error), "hash_mismatch");
        let error = anyhow::anyhow!("unexpected failure");
        assert_eq!(classify_rejection(&error), "validator_failed");
    }
}
