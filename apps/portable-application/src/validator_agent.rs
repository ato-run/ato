use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_formation::authoring::HTTP_CONTRACT_VERIFIER;
use ato_formation::verify::{
    ContractVerificationReceipt, RuntimeHttpObservation, RuntimeObservation,
    VerificationTargetKind, verify_runtime,
};
use ato_materializer_static_web::{
    STATIC_WEB_MANIFEST_V1_SCHEMA, StaticWebFileV1, StaticWebManifestV1, StaticWebRoutingV1,
    StaticWebSecurityV1,
};
use base64::Engine;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};

use crate::{
    PortableRealizationKind, ValidatedPortableApplication, bundle_sha256, validate_bytes_all,
    validate_bytes_for_derivation,
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
        bundle_id: String,
    },
    Rejected {
        bundle_id: String,
        rejection_code: String,
    },
    HostedVerified {
        bundle_id: String,
        fully_satisfied: bool,
    },
}

pub struct ValidatorAgent {
    api: HttpValidatorApi,
    #[allow(dead_code)]
    work_root: PathBuf,
    poll_interval: Duration,
}

impl ValidatorAgent {
    pub fn new(config: ValidatorAgentConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.work_root).with_context(|| {
            format!("create validator work root {}", config.work_root.display())
        })?;
        Ok(Self {
            api: HttpValidatorApi::new(&config.api_url, config.token, config.agent_id)?,
            work_root: config.work_root,
            poll_interval: config.poll_interval,
        })
    }

    pub fn run_once(&self) -> Result<ValidatorRunOutcome> {
        if let Some(job) = self.api.claim()? {
            return self.validate_bundle(job);
        }
        if let Some(job) = self.api.claim_runtime()? {
            return self.verify_hosted(job);
        }
        Ok(ValidatorRunOutcome::Idle)
    }

    fn validate_bundle(&self, job: ValidationJob) -> Result<ValidatorRunOutcome> {
        let bytes = self.api.download(&job)?;
        let result = (|| -> Result<PortableBundleVerificationReport> {
            if bytes.len() as u64 != job.size_bytes {
                bail!("bundle size mismatch");
            }
            let digest = bundle_sha256(&bytes);
            if digest != job.transport_digest {
                bail!("bundle transport digest mismatch");
            }
            let (bundle, validated) = validate_bytes_all(&bytes)?;
            let static_route = unique_static_route(&validated)?;
            for entry in &static_route.tree.entries {
                let reference = ato_computation::ContentRef::parse(&entry.content_ref)?;
                self.api.upload_blob(
                    &job,
                    &entry.content_ref,
                    &bundle.payload_bytes(&reference)?,
                )?;
            }
            report(&job, &bundle, &validated, bytes.len() as u64)
        })();
        match result {
            Ok(report) => {
                self.api.ack_verified(&job, &report)?;
                Ok(ValidatorRunOutcome::Verified {
                    bundle_id: job.bundle_id,
                })
            }
            Err(error) => {
                let rejection_code = classify_rejection(&error).to_owned();
                self.api.ack_rejected(&job, &rejection_code)?;
                Ok(ValidatorRunOutcome::Rejected {
                    bundle_id: job.bundle_id,
                    rejection_code,
                })
            }
        }
    }

    fn verify_hosted(&self, job: ValidationJob) -> Result<ValidatorRunOutcome> {
        let bytes = self.api.download(&job)?;
        if bytes.len() as u64 != job.size_bytes {
            bail!("bundle size mismatch");
        }
        if bundle_sha256(&bytes) != job.transport_digest {
            bail!("bundle transport digest mismatch");
        }
        let selected_derivation_ref = job
            .selected_derivation_ref
            .as_deref()
            .context("runtime job omitted selected_derivation_ref")?;
        let (_, validated) = validate_bytes_for_derivation(&bytes, selected_derivation_ref)?;
        if validated.realization != PortableRealizationKind::StaticWeb {
            bail!("hosted runtime supports only an explicitly selected static-web derivation");
        }
        let mut runtime = RuntimeObservation {
            input_refs: validated
                .derivation
                .inputs
                .iter()
                .map(|input| (input.id.clone(), input.content_ref.clone()))
                .collect::<BTreeMap<_, _>>(),
            http: Vec::new(),
        };
        for requirement in &validated.contract.requirements {
            if requirement.verifier != HTTP_CONTRACT_VERIFIER {
                continue;
            }
            let port = requirement.port.as_deref().unwrap_or_default();
            let method = requirement.method.as_deref().unwrap_or("GET");
            let path = requirement.path.as_deref().unwrap_or("/");
            let observed = self.api.observe(&job, port, method, path)?;
            runtime.http.push(RuntimeHttpObservation::from_response(
                observed.port,
                observed.method,
                observed.path,
                observed.status,
                &observed.body,
            ));
        }
        let verification = verify_runtime(&validated.contract, &runtime);
        let receipt = ContractVerificationReceipt::from_runtime(
            &job.transport_digest,
            validated.contract_ref.to_string(),
            validated.derivation_ref.to_string(),
            VerificationTargetKind::AtoRunHosted,
            &validated.contract,
            &runtime,
            verification,
        );
        self.api.ack_runtime(&job, &receipt)?;
        Ok(ValidatorRunOutcome::HostedVerified {
            bundle_id: job.bundle_id,
            fully_satisfied: receipt.fully_satisfied,
        })
    }

    pub fn run_forever(&self) -> Result<()> {
        loop {
            if self.run_once()? == ValidatorRunOutcome::Idle {
                thread::sleep(self.poll_interval);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBundleVerificationReport {
    pub format_version: u32,
    pub bundle_sha256: String,
    pub profile: String,
    pub root_contract_ref: String,
    pub application_ref: String,
    pub derivation_refs: Vec<String>,
    pub static_derivation_ref: String,
    pub title: String,
    pub surface: PortableSurfaceReport,
    pub artifact: PortableStaticArtifactReport,
    pub object_count: usize,
    pub decoded_size: u64,
    pub validation: ValidationStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableSurfaceReport {
    pub id: String,
    pub port: String,
    pub artifact_ref: String,
    pub entry: String,
    pub spa_fallback: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableStaticArtifactReport {
    pub manifest_digest: String,
    pub manifest_base64: String,
    pub files: Vec<PortableStaticFileReport>,
    pub file_count: usize,
    pub total_size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableStaticFileReport {
    pub path: String,
    pub digest: String,
    pub size: u64,
    pub media_type: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationStatus {
    pub status: &'static str,
}

fn report(
    job: &ValidationJob,
    bundle: &ato_objects::PortableApplicationBundle,
    validated: &[ValidatedPortableApplication],
    decoded_size: u64,
) -> Result<PortableBundleVerificationReport> {
    let validated = unique_static_route(validated)?;
    let materialization_id = format!("portable_{}", job.bundle_id);
    let surface = &validated.application.surfaces[0];
    let files = validated
        .tree
        .entries
        .iter()
        .map(|entry| {
            (
                entry.path.clone(),
                StaticWebFileV1 {
                    blob: entry.content_ref.clone(),
                    size: entry.size,
                    media_type: entry.media_type.clone(),
                },
            )
        })
        .collect();
    let manifest = StaticWebManifestV1 {
        schema: STATIC_WEB_MANIFEST_V1_SCHEMA.to_owned(),
        materialization_id,
        entry_path: surface.entry.clone(),
        routing: StaticWebRoutingV1 {
            spa_fallback: surface.spa_fallback,
        },
        files,
        security: StaticWebSecurityV1::producer_policy(Vec::new())?,
    };
    let manifest_bytes = manifest.canonical_bytes()?;
    let manifest_digest = bundle_sha256(&manifest_bytes);
    let artifact_files = validated
        .tree
        .entries
        .iter()
        .map(|entry| PortableStaticFileReport {
            path: entry.path.clone(),
            digest: entry.content_ref.clone(),
            size: entry.size,
            media_type: entry.media_type.clone(),
        })
        .collect::<Vec<_>>();
    Ok(PortableBundleVerificationReport {
        format_version: 3,
        bundle_sha256: job.transport_digest.clone(),
        profile: bundle.index.profile.clone(),
        root_contract_ref: validated.contract_ref.to_string(),
        application_ref: validated.application_ref.to_string(),
        derivation_refs: bundle.index.derivations.clone(),
        static_derivation_ref: validated.derivation_ref.to_string(),
        title: validated.application.title.clone(),
        surface: PortableSurfaceReport {
            id: surface.id.clone(),
            port: surface.port.clone(),
            artifact_ref: surface.artifact_ref.clone(),
            entry: surface.entry.clone(),
            spa_fallback: surface.spa_fallback,
        },
        artifact: PortableStaticArtifactReport {
            manifest_digest,
            manifest_base64: base64::engine::general_purpose::STANDARD.encode(manifest_bytes),
            file_count: artifact_files.len(),
            total_size: artifact_files.iter().map(|file| file.size).sum(),
            files: artifact_files,
        },
        object_count: bundle.index.objects.len(),
        decoded_size,
        validation: ValidationStatus { status: "valid" },
    })
}

fn unique_static_route(
    validated: &[ValidatedPortableApplication],
) -> Result<&ValidatedPortableApplication> {
    let routes = validated
        .iter()
        .filter(|route| route.realization == PortableRealizationKind::StaticWeb)
        .collect::<Vec<_>>();
    let [route] = routes.as_slice() else {
        bail!(
            "portable hosted profile requires exactly one static-web derivation; found {}",
            routes.len()
        );
    };
    Ok(*route)
}

struct HttpValidatorApi {
    client: Client,
    base_url: String,
    token: String,
    agent_id: HeaderValue,
}

impl HttpValidatorApi {
    fn new(base_url: &str, token: String, agent_id: String) -> Result<Self> {
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
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(180))
                .build()?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            agent_id: HeaderValue::from_str(&agent_id)?,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authenticated(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.token)
    }

    fn claimed(&self, request: RequestBuilder, claim_id: &str) -> RequestBuilder {
        self.authenticated(request)
            .header("x-ato-validator-agent-id", self.agent_id.clone())
            .header("x-ato-validation-claim-id", claim_id)
    }

    fn claim(&self) -> Result<Option<ValidationJob>> {
        let response = self
            .authenticated(
                self.client
                    .post(self.url("/v1/capsule-bundles/validation-jobs/claim"))
                    .json(&serde_json::json!({
                        "agent_id": self.agent_id.to_str().expect("validated agent id"),
                        "format_version": 3
                    })),
            )
            .send()?;
        if response.status().as_u16() == 204 {
            return Ok(None);
        }
        Ok(Some(
            decode_json::<ValidationJobEnvelope>(response, "validator claim")?.job,
        ))
    }

    fn claim_runtime(&self) -> Result<Option<ValidationJob>> {
        let response = self
            .authenticated(
                self.client
                    .post(self.url("/v1/capsule-bundles/runtime-verification-jobs/claim"))
                    .json(&serde_json::json!({
                        "agent_id": self.agent_id.to_str().expect("validated agent id")
                    })),
            )
            .send()?;
        if response.status().as_u16() == 204 {
            return Ok(None);
        }
        Ok(Some(
            decode_json::<ValidationJobEnvelope>(response, "runtime verifier claim")?.job,
        ))
    }

    fn download(&self, job: &ValidationJob) -> Result<Vec<u8>> {
        let response = self
            .claimed(self.client.get(self.url(&job.download_url)), &job.claim_id)
            .send()?;
        decode_bytes(response, "bundle download")
    }

    fn upload_blob(&self, job: &ValidationJob, digest: &str, bytes: &[u8]) -> Result<()> {
        let path = format!(
            "/v1/capsule-bundles/validation-jobs/{}/blobs/{}",
            job.job_id, digest
        );
        let response = self
            .claimed(
                self.client
                    .put(self.url(&path))
                    .header("content-type", "application/octet-stream")
                    .body(bytes.to_vec()),
                &job.claim_id,
            )
            .send()?;
        if !response.status().is_success() {
            bail!(
                "blob upload returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        Ok(())
    }

    fn ack_verified(
        &self,
        job: &ValidationJob,
        report: &PortableBundleVerificationReport,
    ) -> Result<()> {
        self.ack(job, &ValidationAck::Verified { report })
    }

    fn ack_rejected(&self, job: &ValidationJob, rejection_code: &str) -> Result<()> {
        self.ack(
            job,
            &ValidationAck::Rejected {
                rejection_code: rejection_code.to_owned(),
            },
        )
    }

    fn ack(&self, job: &ValidationJob, body: &ValidationAck<'_>) -> Result<()> {
        let path = format!("/v1/capsule-bundles/validation-jobs/{}/ack", job.job_id);
        let response = self
            .claimed(self.client.post(self.url(&path)).json(body), &job.claim_id)
            .send()?;
        if !response.status().is_success() {
            bail!(
                "validator ack returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        Ok(())
    }

    fn observe(
        &self,
        job: &ValidationJob,
        port: &str,
        method: &str,
        path: &str,
    ) -> Result<HostedObservation> {
        let observe_url = job
            .observe_url
            .as_deref()
            .context("runtime job omitted observe_url")?;
        let response = self
            .claimed(
                self.client.get(self.url(observe_url)).query(&[
                    ("port", port),
                    ("method", method),
                    ("path", path),
                ]),
                &job.claim_id,
            )
            .send()?;
        let wire = decode_json::<HostedObservationWire>(response, "hosted observation")?;
        Ok(HostedObservation {
            port: wire.port,
            method: wire.method,
            path: wire.path,
            status: wire.status,
            body: base64::engine::general_purpose::STANDARD
                .decode(wire.body_base64)
                .context("hosted observation body is not canonical base64")?,
        })
    }

    fn ack_runtime(
        &self,
        job: &ValidationJob,
        receipt: &ContractVerificationReceipt,
    ) -> Result<()> {
        let path = format!(
            "/v1/capsule-bundles/runtime-verification-jobs/{}/ack",
            job.job_id
        );
        let response = self
            .claimed(
                self.client
                    .post(self.url(&path))
                    .json(&serde_json::json!({ "receipt": receipt })),
                &job.claim_id,
            )
            .send()?;
        if !response.status().is_success() {
            bail!(
                "runtime verifier ack returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationJobEnvelope {
    job: ValidationJob,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationJob {
    job_id: String,
    claim_id: String,
    #[allow(dead_code)]
    claim_expires_at: String,
    bundle_id: String,
    transport_digest: String,
    size_bytes: u64,
    #[serde(default)]
    #[allow(dead_code)]
    claimed_parent_root: Option<String>,
    download_url: String,
    #[serde(default)]
    observe_url: Option<String>,
    #[serde(default)]
    selected_derivation_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostedObservationWire {
    port: String,
    method: String,
    path: String,
    status: u16,
    body_base64: String,
    #[allow(dead_code)]
    instance_id: String,
}

struct HostedObservation {
    port: String,
    method: String,
    path: String,
    status: u16,
    body: Vec<u8>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ValidationAck<'a> {
    Verified {
        report: &'a PortableBundleVerificationReport,
    },
    Rejected {
        rejection_code: String,
    },
}

fn decode_json<T: for<'de> Deserialize<'de>>(response: Response, operation: &str) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        bail!(
            "{operation} returned {status}: {}",
            response.text().unwrap_or_default()
        );
    }
    response
        .json()
        .with_context(|| format!("{operation} returned malformed JSON"))
}

fn decode_bytes(response: Response, operation: &str) -> Result<Vec<u8>> {
    let status = response.status();
    if !status.is_success() {
        bail!(
            "{operation} returned {status}: {}",
            response.text().unwrap_or_default()
        );
    }
    Ok(response.bytes()?.to_vec())
}

fn classify_rejection(error: &anyhow::Error) -> &'static str {
    let message = format!("{error:#}");
    if message.contains("digest") || message.contains("identity") {
        "hash_mismatch"
    } else if message.contains("closure") || message.contains("unreachable") {
        "unreachable_objects"
    } else if message.contains("version") || message.contains("profile") {
        "unsupported_version"
    } else if message.contains("size") || message.contains("too many") {
        "excessive_decoded_size"
    } else {
        "validator_failed"
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::build_static_bundle;

    #[test]
    fn report_contains_portable_identity_and_static_artifact() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
        let (bytes, bundle) = build_static_bundle(&source, "Ato portability proof").unwrap();
        let (_, validated) = validate_bytes_all(&bytes).unwrap();
        let job = ValidationJob {
            job_id: "bvj_test".to_owned(),
            claim_id: "claim".to_owned(),
            claim_expires_at: "2026-09-16T00:00:00Z".to_owned(),
            bundle_id: "bnd_01TEST".to_owned(),
            transport_digest: bundle_sha256(&bytes),
            size_bytes: bytes.len() as u64,
            claimed_parent_root: None,
            download_url: "/bundle".to_owned(),
            observe_url: None,
            selected_derivation_ref: None,
        };
        let report = report(&job, &bundle, &validated, bytes.len() as u64).unwrap();
        assert_eq!(report.profile, "ato.portable-application/1");
        assert_eq!(report.root_contract_ref, bundle.index.root_contract_ref);
        assert_eq!(report.derivation_refs, bundle.index.derivations);
        assert_eq!(report.artifact.files.len(), 2);
    }
}
