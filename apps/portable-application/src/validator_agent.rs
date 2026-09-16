use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_formation::authoring::HTTP_CONTRACT_VERIFIER;
use ato_formation::verify::{
    ContractVerificationReceipt, RuntimeHttpObservation, RuntimeObservation,
    VerificationExecutionEvidence, VerificationTargetKind, verify_runtime,
};
use ato_materializer_static_web::{
    STATIC_WEB_MANIFEST_V1_SCHEMA, StaticWebFileV1, StaticWebManifestV1, StaticWebRoutingV1,
    StaticWebSecurityV1,
};
use ato_objects::{PortableDependencyProfile, PortableOciArchive};
use base64::Engine;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};

use crate::dependency_transport::{discover_wheel_sources, hydrate_external_objects};
use crate::hosted_export::{
    HostedInstanceCaptureV1, build_hosted_export, portable_bundle_from_bytes,
};
use crate::instance_snapshot::{
    InstanceSnapshotAssetBindingV1, InstanceSnapshotV1, validated_snapshot,
};
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
    Exported {
        export_id: String,
        bundle_sha256: String,
    },
    ExportFailed {
        export_id: String,
        failure_code: String,
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
        if let Some(job) = self.api.claim_export()? {
            return self.export_hosted(job);
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
            if let Some(static_route) = optional_static_route(&validated)? {
                for entry in &static_route.tree.entries {
                    let reference = ato_computation::ContentRef::parse(&entry.content_ref)?;
                    self.api.upload_blob(
                        &job,
                        &entry.content_ref,
                        &bundle.payload_bytes(&reference)?,
                    )?;
                }
            }
            if let Some((_, snapshot)) = validated_snapshot(&bundle)? {
                for reference in snapshot_content_refs(&snapshot).collect::<BTreeSet<_>>() {
                    let reference = ato_computation::ContentRef::parse(reference)?;
                    self.api.upload_snapshot_blob(
                        &job,
                        reference.as_str(),
                        &bundle.payload_bytes(&reference)?,
                    )?;
                }
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
        let mut runtime = RuntimeObservation {
            input_refs: validated
                .derivation
                .inputs
                .iter()
                .map(|input| (input.id.clone(), input.content_ref.clone()))
                .collect::<BTreeMap<_, _>>(),
            http: Vec::new(),
            instance_snapshot_ref: job.instance_snapshot_ref.clone(),
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
        let mut receipt = ContractVerificationReceipt::from_runtime(
            &job.transport_digest,
            validated.contract_ref.to_string(),
            validated.derivation_ref.to_string(),
            VerificationTargetKind::AtoRunHosted,
            &validated.contract,
            &runtime,
            verification,
        );
        let mut execution =
            job.execution_evidence
                .clone()
                .unwrap_or(VerificationExecutionEvidence {
                    realization: job
                        .realization
                        .clone()
                        .unwrap_or_else(|| "static_web".to_owned()),
                    runtime_executable: None,
                    runtime_version: None,
                    pid: None,
                    container_id: None,
                    image: None,
                    platform: None,
                    endpoint: None,
                    run_id: None,
                    lease_id: None,
                    attempt_id: None,
                    dependency_fetches: Vec::new(),
                    portability_profile: None,
                    embedded_oci_image_loaded: None,
                });
        execution.endpoint = job.endpoint.clone();
        execution.run_id = job.run_id.clone();
        execution.lease_id = job.lease_id.clone();
        execution.attempt_id = job.attempt_id.clone();
        receipt.execution = Some(execution);
        self.api.ack_runtime(&job, &receipt)?;
        Ok(ValidatorRunOutcome::HostedVerified {
            bundle_id: job.bundle_id,
            fully_satisfied: receipt.fully_satisfied,
        })
    }

    fn export_hosted(&self, job: ExportJob) -> Result<ValidatorRunOutcome> {
        let result = (|| -> Result<(Vec<u8>, ato_objects::PortableApplicationBundle)> {
            let source_bytes = self.api.download_export_source(&job)?;
            if source_bytes.len() as u64 != job.source_size_bytes
                || bundle_sha256(&source_bytes) != job.source_bundle_sha256
            {
                bail!("export source bundle digest/size mismatch");
            }
            let source = portable_bundle_from_bytes(&source_bytes)?;
            let (source, external_sources) = match job.portability {
                PortableDependencyProfile::Thin => {
                    let sources = discover_wheel_sources(&source)?;
                    (source, sources)
                }
                PortableDependencyProfile::Cached | PortableDependencyProfile::Offline => {
                    (hydrate_external_objects(&source)?.0, BTreeMap::new())
                }
            };
            let archives = if job.portability == PortableDependencyProfile::Offline {
                source
                    .portability
                    .as_ref()
                    .map(|portability| portability.oci_archives.clone())
                    .unwrap_or_default()
            } else {
                Vec::<PortableOciArchive>::new()
            };
            let mut captured = BTreeMap::new();
            for (object_id, download_url) in &job.capture_downloads {
                captured.insert(
                    object_id.clone(),
                    self.api.download_export_object(&job, download_url)?,
                );
            }
            build_hosted_export(
                &source,
                job.capture.as_ref(),
                &captured,
                job.portability,
                &external_sources,
                &archives,
            )
            .map_err(Into::into)
        })();

        match result {
            Ok((bytes, _bundle)) => {
                let digest = bundle_sha256(&bytes);
                let prepared = self.api.prepare_export_output(&job, &digest, bytes.len())?;
                self.api.upload_export_output(&job, &prepared, bytes)?;
                self.api.ack_export_uploaded(&job)?;
                Ok(ValidatorRunOutcome::Exported {
                    export_id: job.job_id,
                    bundle_sha256: digest,
                })
            }
            Err(error) => {
                let failure_code = classify_export_failure(&error).to_owned();
                self.api.ack_export_failed(&job, &failure_code)?;
                Ok(ValidatorRunOutcome::ExportFailed {
                    export_id: job.job_id,
                    failure_code,
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

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBundleVerificationReport {
    pub format_version: u32,
    pub bundle_sha256: String,
    pub profile: String,
    pub root_contract_ref: String,
    pub application_ref: String,
    pub derivation_refs: Vec<String>,
    pub requirement_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_derivation_ref: Option<String>,
    pub title: String,
    pub surface: PortableSurfaceReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<PortableStaticArtifactReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_snapshot: Option<PortableInstanceSnapshotReport>,
    pub routes: Vec<PortableRouteReport>,
    pub workspace: PortableWorkspaceReport,
    pub object_count: usize,
    pub decoded_size: u64,
    pub validation: ValidationStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableSurfaceReport {
    pub id: String,
    pub port: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spa_fallback: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableRouteReport {
    pub derivation_ref: String,
    pub realization: &'static str,
    pub runtimes: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub port: String,
    pub guest_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableWorkspaceReport {
    pub tree_ref: String,
    pub artifact_digest: String,
    pub artifact_base64: String,
    pub file_count: usize,
    pub total_size: u64,
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
pub struct PortableInstanceSnapshotReport {
    pub snapshot_ref: String,
    pub resources: Vec<PortableInstanceSnapshotResourceReport>,
    pub assets: Vec<PortableInstanceSnapshotAssetReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_bindings: Vec<InstanceSnapshotAssetBindingV1>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableInstanceSnapshotResourceReport {
    pub slot: String,
    pub protocol: String,
    pub content_ref: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PortableInstanceSnapshotAssetReport {
    pub alias: String,
    pub content_ref: String,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
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
    let all_routes = validated;
    let first = validated
        .first()
        .context("bundle declares no derivations")?;
    if validated
        .iter()
        .any(|route| route.tree_ref != first.tree_ref)
    {
        bail!("portable hosted profile requires one shared workspace tree");
    }
    let static_route = optional_static_route(validated)?;
    let validated = static_route.unwrap_or(first);
    let materialization_id = format!("portable_{}", job.bundle_id);
    let surface = &validated.application.surfaces[0];
    let artifact = if static_route.is_some() {
        let entry = surface
            .entry
            .as_deref()
            .context("static surface omitted entry")?;
        let spa_fallback = surface
            .spa_fallback
            .context("static surface omitted spa_fallback")?;
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
            entry_path: entry.to_owned(),
            routing: StaticWebRoutingV1 { spa_fallback },
            files,
            security: StaticWebSecurityV1::producer_policy(Vec::new())?,
        };
        let manifest_bytes = manifest.canonical_bytes()?;
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
        Some(PortableStaticArtifactReport {
            manifest_digest: bundle_sha256(&manifest_bytes),
            manifest_base64: base64::engine::general_purpose::STANDARD.encode(manifest_bytes),
            file_count: artifact_files.len(),
            total_size: artifact_files.iter().map(|file| file.size).sum(),
            files: artifact_files,
        })
    } else {
        None
    };
    let workspace_bytes = pack_workspace(bundle, validated)?;
    let routes = validated_routes(all_routes)?;
    let instance_snapshot = snapshot_report(bundle)?;
    Ok(PortableBundleVerificationReport {
        format_version: bundle.index.version,
        bundle_sha256: job.transport_digest.clone(),
        profile: bundle.index.profile.clone(),
        root_contract_ref: validated.contract_ref.to_string(),
        application_ref: validated.application_ref.to_string(),
        derivation_refs: bundle.index.derivations.clone(),
        requirement_ids: validated
            .contract
            .requirements
            .iter()
            .map(|requirement| requirement.id.clone())
            .collect(),
        static_derivation_ref: static_route.map(|route| route.derivation_ref.to_string()),
        title: validated.application.title.clone(),
        surface: PortableSurfaceReport {
            id: surface.id.clone(),
            port: surface.port.clone(),
            path: surface.path.clone(),
            artifact_ref: surface.artifact_ref.clone(),
            entry: surface.entry.clone(),
            spa_fallback: surface.spa_fallback,
        },
        artifact,
        instance_snapshot,
        routes,
        workspace: PortableWorkspaceReport {
            tree_ref: validated.tree_ref.to_string(),
            artifact_digest: bundle_sha256(&workspace_bytes),
            artifact_base64: base64::engine::general_purpose::STANDARD.encode(&workspace_bytes),
            file_count: validated.tree.entries.len(),
            total_size: validated.tree.entries.iter().map(|entry| entry.size).sum(),
        },
        object_count: bundle.index.objects.len(),
        decoded_size,
        validation: ValidationStatus { status: "valid" },
    })
}

fn snapshot_content_refs(snapshot: &InstanceSnapshotV1) -> impl Iterator<Item = &str> {
    snapshot
        .resources
        .iter()
        .map(|resource| resource.content_ref.as_str())
        .chain(
            snapshot
                .assets
                .iter()
                .map(|asset| asset.content_ref.as_str()),
        )
}

fn snapshot_report(
    bundle: &ato_objects::PortableApplicationBundle,
) -> Result<Option<PortableInstanceSnapshotReport>> {
    let Some((snapshot_ref, snapshot)) = validated_snapshot(bundle)? else {
        return Ok(None);
    };
    let resources = snapshot
        .resources
        .into_iter()
        .map(|resource| {
            let reference = ato_computation::ContentRef::parse(&resource.content_ref)?;
            let descriptor = bundle
                .descriptor(&reference)
                .context("snapshot resource descriptor missing after validation")?;
            Ok(PortableInstanceSnapshotResourceReport {
                slot: resource.slot,
                protocol: resource.protocol,
                content_ref: resource.content_ref,
                size: descriptor.size,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let assets = snapshot
        .assets
        .into_iter()
        .map(|asset| PortableInstanceSnapshotAssetReport {
            alias: asset.alias,
            content_ref: asset.content_ref,
            filename: asset.filename,
            content_type: asset.content_type,
            size: asset.size,
        })
        .collect();
    Ok(Some(PortableInstanceSnapshotReport {
        snapshot_ref,
        resources,
        assets,
        asset_bindings: snapshot.asset_bindings,
    }))
}

fn validated_routes(
    validated: &[ValidatedPortableApplication],
) -> Result<Vec<PortableRouteReport>> {
    validated
        .iter()
        .map(|route| {
            let step = route
                .derivation
                .steps
                .first()
                .context("route omitted step")?;
            let port = route
                .derivation
                .ports
                .first()
                .context("route omitted port")?;
            Ok(PortableRouteReport {
                derivation_ref: route.derivation_ref.to_string(),
                realization: match route.realization {
                    PortableRealizationKind::StaticWeb => "static_web",
                    PortableRealizationKind::LocalProcess => "process",
                    PortableRealizationKind::OciContainer => "oci",
                },
                runtimes: route.derivation.runtimes.clone(),
                argv: step.argv.clone(),
                cwd: step.cwd.clone(),
                env: step.env.clone(),
                port: port.id.clone(),
                guest_port: port.guest_port,
            })
        })
        .collect()
}

fn pack_workspace(
    bundle: &ato_objects::PortableApplicationBundle,
    route: &ValidatedPortableApplication,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        for entry in &route.tree.entries {
            let reference = ato_computation::ContentRef::parse(&entry.content_ref)?;
            let contents = bundle.payload_bytes(&reference)?;
            let mut header = tar::Header::new_ustar();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            archive.append_data(&mut header, &entry.path, Cursor::new(contents))?;
        }
        archive.finish()?;
    }
    Ok(bytes)
}

fn optional_static_route(
    validated: &[ValidatedPortableApplication],
) -> Result<Option<&ValidatedPortableApplication>> {
    let routes = validated
        .iter()
        .filter(|route| route.realization == PortableRealizationKind::StaticWeb)
        .collect::<Vec<_>>();
    match routes.as_slice() {
        [] => Ok(None),
        [route] => Ok(Some(*route)),
        _ => bail!(
            "portable hosted profile permits at most one static-web derivation; found {}",
            routes.len()
        ),
    }
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

    fn resolved_url(&self, value: &str) -> String {
        if value.starts_with("https://") || value.starts_with("http://") {
            value.to_owned()
        } else {
            self.url(value)
        }
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
        // Keep wire v3 claimable while the v4 queue is introduced. Version is
        // explicit so an older validator cannot accidentally claim v4 bytes.
        for version in [
            ato_objects::PORTABLE_APPLICATION_BUNDLE_VERSION_V4,
            ato_objects::PORTABLE_APPLICATION_BUNDLE_VERSION,
        ] {
            let response = self
                .authenticated(
                    self.client
                        .post(self.url("/v1/capsule-bundles/validation-jobs/claim"))
                        .json(&serde_json::json!({
                            "agent_id": self.agent_id.to_str().expect("validated agent id"),
                            "format_version": version
                        })),
                )
                .send()?;
            if response.status().as_u16() == 204 {
                continue;
            }
            return Ok(Some(
                decode_json::<ValidationJobEnvelope>(response, "validator claim")?.job,
            ));
        }
        Ok(None)
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

    fn claim_export(&self) -> Result<Option<ExportJob>> {
        let response = self
            .authenticated(
                self.client
                    .post(self.url("/v1/portable-application-exports/jobs/claim"))
                    .json(&serde_json::json!({
                        "agent_id": self.agent_id.to_str().expect("validated agent id")
                    })),
            )
            .send()?;
        if response.status().as_u16() == 204 {
            return Ok(None);
        }
        Ok(Some(
            decode_json::<ExportJobEnvelope>(response, "portable export claim")?.job,
        ))
    }

    fn download(&self, job: &ValidationJob) -> Result<Vec<u8>> {
        let response = self
            .claimed(self.client.get(self.url(&job.download_url)), &job.claim_id)
            .send()?;
        decode_bytes(response, "bundle download")
    }

    fn download_export_source(&self, job: &ExportJob) -> Result<Vec<u8>> {
        let response = self
            .claimed(
                self.client.get(self.resolved_url(&job.source_download_url)),
                &job.claim_id,
            )
            .send()?;
        decode_bytes(response, "portable export source download")
    }

    fn download_export_object(&self, job: &ExportJob, path: &str) -> Result<Vec<u8>> {
        let response = self
            .claimed(self.client.get(self.resolved_url(path)), &job.claim_id)
            .send()?;
        decode_bytes(response, "portable export capture download")
    }

    fn prepare_export_output(
        &self,
        job: &ExportJob,
        digest: &str,
        size: usize,
    ) -> Result<ExportOutputPreparation> {
        let path = format!(
            "/v1/portable-application-exports/jobs/{}/output/prepare",
            job.job_id
        );
        let response = self
            .claimed(
                self.client.post(self.url(&path)).json(&serde_json::json!({
                    "transport_digest": digest,
                    "size_bytes": size,
                })),
                &job.claim_id,
            )
            .send()?;
        decode_json(response, "portable export output prepare")
    }

    fn upload_export_output(
        &self,
        job: &ExportJob,
        prepared: &ExportOutputPreparation,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let target = self.resolved_url(&prepared.upload_url);
        let mut request = self.client.put(target).body(bytes);
        for (name, value) in &prepared.upload_headers {
            request = request.header(name, value);
        }
        if !prepared.upload_direct {
            request = self.claimed(request, &job.claim_id);
        }
        let response = request.send()?;
        if !response.status().is_success() {
            bail!(
                "portable export output upload returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        Ok(())
    }

    fn ack_export_uploaded(&self, job: &ExportJob) -> Result<()> {
        self.ack_export(job, &serde_json::json!({ "status": "uploaded" }))
    }

    fn ack_export_failed(&self, job: &ExportJob, failure_code: &str) -> Result<()> {
        self.ack_export(
            job,
            &serde_json::json!({
                "status": "failed",
                "failure_code": failure_code,
            }),
        )
    }

    fn ack_export(&self, job: &ExportJob, body: &serde_json::Value) -> Result<()> {
        let path = format!("/v1/portable-application-exports/jobs/{}/ack", job.job_id);
        let response = self
            .claimed(self.client.post(self.url(&path)).json(body), &job.claim_id)
            .send()?;
        if !response.status().is_success() {
            bail!(
                "portable export acknowledgement returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            );
        }
        Ok(())
    }

    fn upload_blob(&self, job: &ValidationJob, digest: &str, bytes: &[u8]) -> Result<()> {
        self.upload_validation_blob(job, "blobs", digest, bytes)
    }

    fn upload_snapshot_blob(&self, job: &ValidationJob, digest: &str, bytes: &[u8]) -> Result<()> {
        self.upload_validation_blob(job, "snapshot-blobs", digest, bytes)
    }

    fn upload_validation_blob(
        &self,
        job: &ValidationJob,
        lane: &str,
        digest: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let path = format!(
            "/v1/capsule-bundles/validation-jobs/{}/{lane}/{}",
            job.job_id, digest,
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
struct ExportJobEnvelope {
    job: ExportJob,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportJob {
    job_id: String,
    claim_id: String,
    #[allow(dead_code)]
    claim_expires_at: String,
    source_bundle_sha256: String,
    source_size_bytes: u64,
    source_download_url: String,
    #[allow(dead_code)]
    include_saved_data: bool,
    portability: PortableDependencyProfile,
    capture: Option<HostedInstanceCaptureV1>,
    capture_downloads: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportOutputPreparation {
    #[allow(dead_code)]
    bundle_id: String,
    upload_url: String,
    upload_direct: bool,
    upload_headers: BTreeMap<String, String>,
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
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    lease_id: Option<String>,
    #[serde(default)]
    attempt_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    execution_id: Option<String>,
    #[serde(default)]
    realization: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    execution_evidence: Option<VerificationExecutionEvidence>,
    #[serde(default)]
    instance_snapshot_ref: Option<String>,
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

fn classify_export_failure(error: &anyhow::Error) -> &'static str {
    let message = format!("{error:#}");
    if message.contains("dependency unavailable") {
        "portable_export_dependency_unavailable"
    } else if message.contains("digest") || message.contains("size") {
        "portable_export_integrity_failure"
    } else if message.contains("offline OCI") {
        "portable_export_offline_incomplete"
    } else if message.contains("snapshot") || message.contains("capture") {
        "portable_export_snapshot_invalid"
    } else {
        "portable_export_failed"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use super::*;
    use crate::build_static_bundle;
    use crate::instance_snapshot::{
        DATA_JSON_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotAssetV1,
        InstanceSnapshotResourceV1, attach_instance_snapshot,
    };
    use crate::portability_export::repack_portable_dependencies;
    use ato_objects::PortableDependencyProfile;

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
            run_id: None,
            lease_id: None,
            attempt_id: None,
            execution_id: None,
            realization: None,
            endpoint: None,
            execution_evidence: None,
            instance_snapshot_ref: None,
        };
        let report = report(&job, &bundle, &validated, bytes.len() as u64).unwrap();
        assert_eq!(report.format_version, 3);
        assert_eq!(report.profile, "ato.portable-application/1");
        assert_eq!(report.root_contract_ref, bundle.index.root_contract_ref);
        assert_eq!(report.derivation_refs, bundle.index.derivations);
        assert_eq!(report.artifact.as_ref().unwrap().files.len(), 2);
    }

    #[test]
    fn v4_report_preserves_contract_and_derivation_refs() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
        let (_, original) = build_static_bundle(&source, "Ato portability proof").unwrap();
        let (bytes, bundle) = repack_portable_dependencies(
            &original,
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
        )
        .unwrap();
        let (_, validated) = validate_bytes_all(&bytes).unwrap();
        let job = ValidationJob {
            job_id: "bvj_v4".to_owned(),
            claim_id: "claim".to_owned(),
            claim_expires_at: "2026-09-16T00:00:00Z".to_owned(),
            bundle_id: "bnd_01V4".to_owned(),
            transport_digest: bundle_sha256(&bytes),
            size_bytes: bytes.len() as u64,
            claimed_parent_root: None,
            download_url: "/bundle".to_owned(),
            observe_url: None,
            selected_derivation_ref: None,
            run_id: None,
            lease_id: None,
            attempt_id: None,
            execution_id: None,
            realization: None,
            endpoint: None,
            execution_evidence: None,
            instance_snapshot_ref: None,
        };
        let report = report(&job, &bundle, &validated, bytes.len() as u64).unwrap();
        assert_eq!(report.format_version, 4);
        assert_eq!(report.profile, "ato.portable-application/2");
        assert_eq!(report.root_contract_ref, original.index.root_contract_ref);
        assert_eq!(report.derivation_refs, original.index.derivations);
        assert!(report.instance_snapshot.is_none());
    }

    #[test]
    fn v4_report_authenticates_snapshot_content_for_hosted_restore() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
        let (_, original) = build_static_bundle(&source, "Ato portability proof").unwrap();
        let (_, original) = repack_portable_dependencies(
            &original,
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
        )
        .unwrap();
        let resource = br#"{"todos":["one","two"]}"#.to_vec();
        let asset = b"portable-photo-bytes".to_vec();
        let resource_size = resource.len() as u64;
        let asset_size = asset.len() as u64;
        let resource_ref = bundle_sha256(&resource);
        let asset_ref = bundle_sha256(&asset);
        let snapshot = InstanceSnapshotV1 {
            schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
            resources: vec![InstanceSnapshotResourceV1 {
                slot: "main".to_owned(),
                protocol: DATA_JSON_PROTOCOL.to_owned(),
                content_ref: resource_ref.clone(),
            }],
            assets: vec![InstanceSnapshotAssetV1 {
                alias: "asset-1".to_owned(),
                content_ref: asset_ref.clone(),
                filename: "photo.jpg".to_owned(),
                content_type: "image/jpeg".to_owned(),
                size: asset_size,
            }],
            asset_bindings: vec![],
        };
        let (bytes, bundle) = attach_instance_snapshot(
            &original,
            snapshot,
            &BTreeMap::from([(resource_ref.clone(), resource), (asset_ref.clone(), asset)]),
        )
        .unwrap();
        let (_, validated) = validate_bytes_all(&bytes).unwrap();
        let job = ValidationJob {
            job_id: "bvj_snapshot".to_owned(),
            claim_id: "claim".to_owned(),
            claim_expires_at: "2026-09-17T00:00:00Z".to_owned(),
            bundle_id: "bnd_01SNAPSHOT".to_owned(),
            transport_digest: bundle_sha256(&bytes),
            size_bytes: bytes.len() as u64,
            claimed_parent_root: None,
            download_url: "/bundle".to_owned(),
            observe_url: None,
            selected_derivation_ref: None,
            run_id: None,
            lease_id: None,
            attempt_id: None,
            execution_id: None,
            realization: None,
            endpoint: None,
            execution_evidence: None,
            instance_snapshot_ref: None,
        };

        let report = report(&job, &bundle, &validated, bytes.len() as u64).unwrap();
        let restored = report.instance_snapshot.unwrap();
        assert_eq!(
            Some(restored.snapshot_ref.as_str()),
            bundle.index.instance_snapshot_ref.as_deref()
        );
        assert_eq!(restored.resources[0].content_ref, resource_ref);
        assert_eq!(restored.resources[0].size, resource_size);
        assert_eq!(restored.assets[0].content_ref, asset_ref);
        assert_eq!(restored.assets[0].size, asset_size);
    }
}
