use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::application::pipeline::producer::PublishDryRunStageResult;
use crate::application::ports::publish::{
    DestinationSpec, PublishableArtifact, PublishedLocation, SharedDestinationPort,
};

use crate::publish_artifact::ArtifactManifestInfo;

#[derive(Debug, Clone, Serialize)]
pub struct PrivatePublishResult {
    pub scoped_id: String,
    pub version: String,
    pub artifact_url: String,
    pub file_name: String,
    pub sha256: String,
    pub blake3: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub already_existed: bool,
    pub registry_url: String,
}

#[derive(Debug, Clone)]
pub struct PrivatePublishSummary {
    pub source: &'static str,
    pub scoped_id: String,
    pub version: String,
    pub allow_existing: bool,
}

#[derive(Debug, Clone)]
struct PreparedPrivatePublishArtifact {
    artifact_path: PathBuf,
    scoped_id: String,
    version: String,
}

#[derive(Debug, Clone)]
enum ResolvedPrivatePublishInput {
    Build {
        manifest_path: PathBuf,
        name: String,
        version: String,
        scoped_id: String,
    },
    Artifact {
        artifact_path: PathBuf,
        scoped_id: String,
        version: String,
    },
}

#[derive(Debug, Clone)]
pub struct PrivatePublishRequest {
    pub registry_url: String,
    pub publisher_hint: Option<String>,
    pub artifact_path: Option<PathBuf>,
    pub force_large_payload: bool,
    pub scoped_id: Option<String>,
    pub allow_existing: bool,
}

#[derive(Debug, Clone)]
pub struct OfficialPublishRequest<'a> {
    pub cwd: &'a Path,
    pub registry_url: &'a str,
    pub fix: bool,
}

#[derive(Debug, Clone)]
pub struct OfficialPublishOutcome {
    pub route: crate::publish_official::PublishRoutePlan,
    pub fix_result: crate::publish_official::WorkflowFixResult,
    pub diagnosis: crate::publish_official::OfficialPublishDiagnosis,
}

#[derive(Debug, Clone)]
pub struct PublishPhaseRequest {
    pub artifact: PublishableArtifact,
    pub destination: DestinationSpec,
}

pub struct PublishPhase {
    destination: SharedDestinationPort,
}

impl PublishPhase {
    pub fn new(destination: SharedDestinationPort) -> Self {
        Self { destination }
    }

    pub async fn execute(&self, request: &PublishPhaseRequest) -> Result<PublishedLocation> {
        self.destination
            .publish(&request.artifact, &request.destination)
            .await
    }
}

pub fn summarize_private_publish(request: &PrivatePublishRequest) -> Result<PrivatePublishSummary> {
    let resolved = resolve_private_publish_input(request)?;
    let (source, scoped_id, version) = match resolved {
        ResolvedPrivatePublishInput::Build {
            scoped_id, version, ..
        } => ("build", scoped_id, version),
        ResolvedPrivatePublishInput::Artifact {
            scoped_id, version, ..
        } => ("artifact", scoped_id, version),
    };

    Ok(PrivatePublishSummary {
        source,
        scoped_id,
        version,
        allow_existing: request.allow_existing,
    })
}

#[allow(dead_code)]
pub fn run_private_publish_phase(request: PrivatePublishRequest) -> Result<PrivatePublishResult> {
    futures::executor::block_on(run_private_publish_phase_async(request))
}

pub async fn run_private_publish_phase_async(
    request: PrivatePublishRequest,
) -> Result<PrivatePublishResult> {
    let prepared = prepare_private_publish_artifact(&request)?;
    let artifact_bytes = std::fs::read(&prepared.artifact_path).with_context(|| {
        format!(
            "Failed to read artifact: {}",
            prepared.artifact_path.display()
        )
    })?;

    run_direct_publish_phase_async(
        &DirectPublishRequest {
        artifact_path: prepared.artifact_path.clone(),
        registry_url: request.registry_url,
        scoped_id: prepared.scoped_id.clone(),
        version: prepared.version,
        normalized_file_name: prepared
            .artifact_path
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| format!("{}.capsule", prepared.scoped_id.replace('/', "-"))),
        content_hash: crate::artifact_hash::compute_blake3_label(&artifact_bytes),
        allow_existing: request.allow_existing,
        force_large_payload: request.force_large_payload,
        },
        artifact_bytes,
    )
    .await
}

fn prepare_private_publish_artifact(
    request: &PrivatePublishRequest,
) -> Result<PreparedPrivatePublishArtifact> {
    match resolve_private_publish_input(request)? {
        ResolvedPrivatePublishInput::Build {
            manifest_path,
            name,
            version,
            scoped_id,
        } => {
            let artifact_path =
                crate::publish_ci::build_capsule_artifact(&manifest_path, &name, &version)
                    .with_context(|| "Failed to build artifact for private registry publish")?;

            Ok(PreparedPrivatePublishArtifact {
                artifact_path,
                scoped_id,
                version,
            })
        }
        ResolvedPrivatePublishInput::Artifact {
            artifact_path,
            scoped_id,
            version,
        } => Ok(PreparedPrivatePublishArtifact {
            artifact_path,
            scoped_id,
            version,
        }),
    }
}

fn resolve_private_publish_input(
    request: &PrivatePublishRequest,
) -> Result<ResolvedPrivatePublishInput> {
    if let Some(artifact_path) = &request.artifact_path {
        let info = crate::publish_artifact::inspect_artifact_manifest(artifact_path)?;
        let slug = manifest_slug(&info.name)?;
        let version = resolve_manifest_publish_version(&info.version);
        let scoped_id = resolve_scoped_id_for_artifact(
            request.publisher_hint.as_deref(),
            request.scoped_id.as_deref(),
            &info,
            &slug,
        )?;
        return Ok(ResolvedPrivatePublishInput::Artifact {
            artifact_path: artifact_path.clone(),
            scoped_id,
            version,
        });
    }

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let manifest_path = cwd.join("capsule.toml");
    let manifest_raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_raw)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml: {}", err))?;

    let slug = manifest_slug(&manifest.name)?;
    let publisher = resolve_private_publisher(request.publisher_hint.as_deref(), &manifest_raw);
    let scoped_id = format!("{}/{}", publisher, slug);
    let version = resolve_manifest_publish_version(&manifest.version);

    Ok(ResolvedPrivatePublishInput::Build {
        manifest_path,
        name: manifest.name,
        version,
        scoped_id,
    })
}

fn resolve_manifest_publish_version(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        "auto".to_string()
    } else {
        trimmed.to_string()
    }
}

fn resolve_scoped_id_for_artifact(
    publisher_hint: Option<&str>,
    override_scoped_id: Option<&str>,
    info: &ArtifactManifestInfo,
    slug: &str,
) -> Result<String> {
    if let Some(publisher_hint) = publisher_hint {
        if let Some(explicit) = override_scoped_id {
            let scoped = crate::install::parse_capsule_ref(explicit)?;
            if scoped.slug != slug {
                anyhow::bail!(
                    "--scoped-id slug '{}' must match artifact manifest.name '{}'",
                    scoped.slug,
                    slug
                );
            }
            if scoped.publisher != publisher_hint {
                anyhow::bail!(
                    "--scoped-id publisher '{}' must match publisher '{}'",
                    scoped.publisher,
                    publisher_hint
                );
            }
        }
        return Ok(format!("{}/{}", publisher_hint, slug));
    }

    if let Some(explicit) = override_scoped_id {
        let scoped = crate::install::parse_capsule_ref(explicit)?;
        if scoped.slug != slug {
            anyhow::bail!(
                "--scoped-id slug '{}' must match artifact manifest.name '{}'",
                scoped.slug,
                slug
            );
        }
        return Ok(format!("{}/{}", scoped.publisher, scoped.slug));
    }

    let publisher = info
        .repository_owner
        .as_deref()
        .map(normalize_segment)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "local".to_string());
    Ok(format!("{}/{}", publisher, slug))
}

fn resolve_private_publisher(publisher_hint: Option<&str>, manifest_raw: &str) -> String {
    if let Some(publisher_hint) = publisher_hint {
        return publisher_hint.to_string();
    }

    if let Some(repo_owner) = manifest_repository_owner(manifest_raw) {
        return repo_owner;
    }

    if let Ok(origin) = crate::publish_preflight::run_git(&["remote", "get-url", "origin"]) {
        if let Some(repo) = crate::publish_preflight::normalize_origin_to_repo(&origin) {
            if let Some((owner, _)) = repo.split_once('/') {
                let normalized = normalize_segment(owner);
                if !normalized.is_empty() {
                    return normalized;
                }
            }
        }
    }

    "local".to_string()
}

fn manifest_repository_owner(manifest_raw: &str) -> Option<String> {
    let raw = crate::publish_preflight::find_manifest_repository(manifest_raw)?;
    let normalized = crate::publish_preflight::normalize_repository_value(&raw).ok()?;
    let (owner, _) = normalized.split_once('/')?;
    let owner = normalize_segment(owner);
    if owner.is_empty() {
        None
    } else {
        Some(owner)
    }
}

fn manifest_slug(raw: &str) -> Result<String> {
    let slug = raw.trim();
    if slug.is_empty() {
        anyhow::bail!("capsule.toml name is empty");
    }
    let parsed = crate::install::parse_capsule_ref(&format!("local/{}", slug))
        .with_context(|| "capsule.toml name must be lowercase kebab-case")?;
    if parsed.slug != slug {
        anyhow::bail!("capsule.toml name must be lowercase kebab-case");
    }
    Ok(slug.to_string())
}

fn normalize_segment(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;

    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            prev_dash = false;
            continue;
        }

        if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }

    out.trim_matches('-').to_string()
}

#[derive(Debug, Clone)]
pub struct DirectPublishRequest {
    #[allow(dead_code)]
    pub artifact_path: PathBuf,
    pub registry_url: String,
    pub scoped_id: String,
    pub version: String,
    pub normalized_file_name: String,
    pub content_hash: String,
    pub allow_existing: bool,
    pub force_large_payload: bool,
}

#[allow(dead_code)]
pub fn run_direct_publish_phase(request: &DirectPublishRequest) -> Result<PrivatePublishResult> {
    futures::executor::block_on(run_direct_publish_phase_async(
        request,
        std::fs::read(&request.artifact_path).with_context(|| {
            format!(
                "Failed to read artifact: {}",
                request.artifact_path.display()
            )
        })?,
    ))
}

async fn run_direct_publish_phase_async(
    request: &DirectPublishRequest,
    artifact_bytes: Vec<u8>,
) -> Result<PrivatePublishResult> {
    crate::payload_guard::ensure_payload_bytes_size(
        artifact_bytes.len() as u64,
        request.force_large_payload,
        "--force-large-payload",
    )?;
    let phase = PublishPhase::new(Arc::new(
        crate::adapters::publish::destination::remote_api::RemoteRegistryDestination,
    ));
    let published = phase
        .execute(&PublishPhaseRequest {
        artifact: PublishableArtifact {
            bytes: artifact_bytes,
            scoped_id: request.scoped_id.clone(),
            version: request.version.clone(),
            normalized_file_name: request.normalized_file_name.clone(),
            content_hash: request.content_hash.clone(),
        },
        destination: DestinationSpec::RemoteRegistry {
            registry_url: request.registry_url.clone(),
            scoped_id: request.scoped_id.clone(),
            version: request.version.clone(),
            allow_existing: request.allow_existing,
            force_large_payload: request.force_large_payload,
        },
        })
        .await?;
    let metadata = published
        .metadata
        .context("missing remote publish metadata from destination port")?;

    Ok(PrivatePublishResult {
        scoped_id: request.scoped_id.clone(),
        version: request.version.clone(),
        artifact_url: published.locator,
        file_name: metadata.file_name,
        sha256: metadata.sha256,
        blake3: metadata.blake3,
        size_bytes: metadata.size_bytes,
        already_existed: metadata.already_existed,
        registry_url: request.registry_url.clone(),
    })
}

pub fn run_official_publish_phase(
    request: &OfficialPublishRequest<'_>,
) -> Result<OfficialPublishOutcome> {
    let route = crate::publish_official::build_route_plan(request.registry_url);

    let mut fix_result = crate::publish_official::WorkflowFixResult::default();
    let mut diagnosis =
        crate::publish_official::diagnose_official(request.cwd, request.registry_url);
    if request.fix && diagnosis.needs_workflow_fix {
        fix_result = crate::publish_official::apply_workflow_fix_once(request.cwd)
            .with_context(|| "Failed to apply official publish workflow fix")?;
        diagnosis = crate::publish_official::diagnose_official(request.cwd, request.registry_url);
    }

    Ok(OfficialPublishOutcome {
        route,
        fix_result,
        diagnosis,
    })
}

pub fn official_publish_diagnosis_lines(outcome: &OfficialPublishOutcome) -> Vec<String> {
    let mut lines = vec![format!(
        "🔎 official publish route registry={} route={:?}",
        outcome.route.registry_url, outcome.route.route
    )];
    lines.extend(outcome.diagnosis.stages.iter().map(|stage| {
        let icon = if stage.ok { "✅" } else { "❌" };
        format!("{} {:<14} {}", icon, stage.key, stage.message)
    }));
    if outcome.fix_result.attempted {
        if outcome.fix_result.applied {
            let label = if outcome.fix_result.created {
                "created"
            } else {
                "updated"
            };
            lines.push(format!("🛠️  workflow {} via --fix", label));
        } else {
            lines.push("🛠️  --fix requested, but workflow was already up-to-date".to_string());
        }
    }
    lines
}

pub fn official_publish_failure_action(outcome: &OfficialPublishOutcome) -> String {
    crate::publish_official::collect_issue_actions(&outcome.diagnosis.issues)
        .into_iter()
        .next()
        .unwrap_or_else(|| "ato publish --deploy --registry https://api.ato.run".to_string())
}

pub fn official_publish_issue_lines(outcome: &OfficialPublishOutcome) -> Vec<String> {
    outcome
        .diagnosis
        .issues
        .iter()
        .map(|issue| format!(" - [{}] {}", issue.stage, issue.message))
        .collect()
}

pub struct DirectPublishDryRunRequest<'a> {
    pub registry_url: &'a str,
    pub scoped_id: &'a str,
    pub version: &'a str,
    pub artifact_version: &'a str,
    pub allow_existing: bool,
    pub requires_session_token: bool,
}

pub fn run_direct_publish_dry_run_phase(
    request: &DirectPublishDryRunRequest<'_>,
) -> Result<PublishDryRunStageResult> {
    let registry =
        crate::registry::http::normalize_registry_url(request.registry_url, "--registry")?;
    let scoped = crate::install::parse_capsule_ref(request.scoped_id)?;
    let upload_endpoint = build_direct_publish_upload_endpoint(
        &registry,
        request.scoped_id,
        request.version,
        upload_file_name_for_artifact(&scoped.slug, request.artifact_version).as_deref(),
        request.allow_existing,
    )?;
    probe_registry_reachability(&registry)?;

    let auth_ready = if request.requires_session_token {
        crate::auth::current_session_token().is_some()
    } else {
        crate::registry::http::current_ato_token().is_some()
    };

    Ok(PublishDryRunStageResult {
        kind: "direct_preflight",
        diagnosis: None,
        registry: Some(registry),
        upload_endpoint: Some(upload_endpoint),
        reachable: Some(true),
        auth_ready: Some(auth_ready),
        permission_check: Some("local_prereq_only".to_string()),
    })
}

pub fn direct_publish_dry_run_is_ready(
    result: &PublishDryRunStageResult,
    requires_session_token: bool,
) -> bool {
    let reachable = result.reachable.unwrap_or(false);
    let auth_ready = result.auth_ready.unwrap_or(false);
    if requires_session_token {
        reachable && auth_ready
    } else {
        reachable
    }
}

pub fn direct_publish_dry_run_failure_message(
    result: &PublishDryRunStageResult,
    requires_session_token: bool,
) -> String {
    if !result.reachable.unwrap_or(false) {
        return "registry reachability probe failed".to_string();
    }
    if requires_session_token && !result.auth_ready.unwrap_or(false) {
        return "Personal Dock publish dry-run requires an active session token".to_string();
    }
    if !requires_session_token && !result.auth_ready.unwrap_or(false) {
        return "publish preflight completed without ATO_TOKEN; continuing with local prereq-only readiness".to_string();
    }
    "publish preflight failed".to_string()
}

fn upload_file_name_for_artifact(slug: &str, manifest_version: &str) -> Option<String> {
    let version = manifest_version.trim();
    if version.is_empty() {
        None
    } else {
        Some(format!("{}-{}.capsule", slug, version))
    }
}

fn build_direct_publish_upload_endpoint(
    registry_url: &str,
    scoped_id: &str,
    version: &str,
    file_name: Option<&str>,
    allow_existing: bool,
) -> Result<String> {
    let scoped = crate::install::parse_capsule_ref(scoped_id)?;
    let mut endpoint = format!(
        "{}/v1/local/capsules/{}/{}/{}",
        registry_url,
        urlencoding::encode(&scoped.publisher),
        urlencoding::encode(&scoped.slug),
        urlencoding::encode(version)
    );
    if let Some(file_name) = file_name.filter(|value| !value.trim().is_empty()) {
        endpoint.push_str(&format!("?file_name={}", urlencoding::encode(file_name)));
    }
    if allow_existing {
        endpoint.push_str(if endpoint.contains('?') {
            "&allow_existing=true"
        } else {
            "?allow_existing=true"
        });
    }
    Ok(endpoint)
}

fn probe_registry_reachability(registry_url: &str) -> Result<()> {
    let client = crate::registry::http::blocking_client_builder(registry_url)
        .build()
        .map_err(|err| anyhow::anyhow!("Failed to create registry preflight client: {}", err))?;
    client
        .get(registry_url)
        .send()
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("Failed to reach registry {}: {}", registry_url, err))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;
    use tar::Builder;

    use super::{
        normalize_segment, resolve_private_publisher, summarize_private_publish,
        PrivatePublishRequest, PublishPhase, PublishPhaseRequest,
    };
    use crate::application::ports::publish::{
        DestinationPort, DestinationSpec, PublishableArtifact, PublishedLocation,
    };

    struct CwdGuard {
        previous: std::path::PathBuf,
    }

    impl CwdGuard {
        fn set_to(path: &std::path::Path) -> Self {
            let previous = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(path).expect("set current dir");
            Self { previous }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    fn write_test_artifact(path: &Path, name: &str, version: &str, repository: Option<&str>) {
        let repo_line = repository
            .map(|value| format!("\n[metadata]\nrepository = \"{}\"\n", value))
            .unwrap_or_default();
        let manifest = format!(
            r#"schema_version = "0.2"
name = "{name}"
version = "{version}"
type = "app"
default_target = "cli"
{repo_line}
[targets.cli]
runtime = "source"
driver = "deno"
entrypoint = "main.ts"
"#
        );
        let mut bytes = Vec::<u8>::new();
        {
            let mut builder = Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(manifest.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "capsule.toml", manifest.as_bytes())
                .expect("append manifest");
            let mut sig_header = tar::Header::new_gnu();
            let sig = "{}";
            sig_header.set_mode(0o644);
            sig_header.set_size(sig.len() as u64);
            sig_header.set_cksum();
            builder
                .append_data(&mut sig_header, "signature.json", sig.as_bytes())
                .expect("append signature");
            builder.finish().expect("finish tar");
        }
        let mut file = std::fs::File::create(path).expect("create artifact");
        file.write_all(&bytes).expect("write artifact");
    }

    #[derive(Debug)]
    struct StubDestination;

    #[async_trait]
    impl DestinationPort for StubDestination {
        async fn publish(
            &self,
            artifact: &PublishableArtifact,
            destination: &DestinationSpec,
        ) -> anyhow::Result<PublishedLocation> {
            Ok(PublishedLocation {
                destination: destination.clone(),
                receipt: format!("published {}", artifact.normalized_file_name),
                locator: "memory://published".to_string(),
                metadata: None,
            })
        }
    }

    #[tokio::test]
    async fn publish_phase_routes_artifact_to_destination_port() {
        let phase = PublishPhase::new(Arc::new(StubDestination));
        let request = PublishPhaseRequest {
            artifact: PublishableArtifact {
                bytes: b"capsule".to_vec(),
                scoped_id: "capsules/demo".to_string(),
                version: "0.1.0".to_string(),
                normalized_file_name: "demo-0.1.0.capsule".to_string(),
                content_hash: "blake3:demo".to_string(),
            },
            destination: DestinationSpec::RemoteRegistry {
                registry_url: "https://example.invalid".to_string(),
                scoped_id: "capsules/demo".to_string(),
                version: "0.1.0".to_string(),
                allow_existing: false,
                force_large_payload: false,
            },
        };

        let published = phase.execute(&request).await.expect("publish");

        assert_eq!(published.receipt, "published demo-0.1.0.capsule");
        assert_eq!(published.locator, "memory://published");
    }

    #[test]
    fn summarize_artifact_mode_does_not_require_cwd_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let artifact_path = tmp.path().join("demo.capsule");
        write_test_artifact(
            &artifact_path,
            "demo-app",
            "1.2.3",
            Some("https://github.com/Koh0920/demo-app"),
        );

        let summary = summarize_private_publish(&PrivatePublishRequest {
            registry_url: "https://example.invalid".to_string(),
            publisher_hint: None,
            artifact_path: Some(artifact_path),
            force_large_payload: false,
            scoped_id: None,
            allow_existing: true,
        })
        .expect("summarize");

        assert_eq!(summary.source, "artifact");
        assert_eq!(summary.scoped_id, "koh0920/demo-app");
        assert_eq!(summary.version, "1.2.3");
        assert!(summary.allow_existing);
    }

    #[test]
    fn summarize_artifact_mode_prefers_explicit_scoped_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let artifact_path = tmp.path().join("demo.capsule");
        write_test_artifact(&artifact_path, "demo-app", "1.2.3", None);

        let summary = summarize_private_publish(&PrivatePublishRequest {
            registry_url: "https://example.invalid".to_string(),
            publisher_hint: None,
            artifact_path: Some(artifact_path),
            force_large_payload: false,
            scoped_id: Some("team-x/demo-app".to_string()),
            allow_existing: false,
        })
        .expect("summarize");

        assert_eq!(summary.scoped_id, "team-x/demo-app");
    }

    #[test]
    fn summarize_artifact_mode_uses_publisher_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let artifact_path = tmp.path().join("demo.capsule");
        write_test_artifact(
            &artifact_path,
            "demo-app",
            "1.2.3",
            Some("https://github.com/another-owner/demo-app"),
        );

        let summary = summarize_private_publish(&PrivatePublishRequest {
            registry_url: "https://example.invalid".to_string(),
            publisher_hint: Some("koh0920".to_string()),
            artifact_path: Some(artifact_path),
            force_large_payload: false,
            scoped_id: None,
            allow_existing: false,
        })
        .expect("summarize");

        assert_eq!(summary.scoped_id, "koh0920/demo-app");
    }

    #[test]
    fn summarize_artifact_mode_rejects_scoped_id_publisher_mismatch_for_publisher_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let artifact_path = tmp.path().join("demo.capsule");
        write_test_artifact(&artifact_path, "demo-app", "1.2.3", None);

        let err = summarize_private_publish(&PrivatePublishRequest {
            registry_url: "https://example.invalid".to_string(),
            publisher_hint: Some("koh0920".to_string()),
            artifact_path: Some(artifact_path),
            force_large_payload: false,
            scoped_id: Some("other-team/demo-app".to_string()),
            allow_existing: false,
        })
        .expect_err("must reject mismatched publisher hint");

        assert!(err.to_string().contains("must match publisher 'koh0920'"));
    }

    #[test]
    #[serial_test::serial]
    fn summarize_build_mode_uses_auto_when_manifest_version_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = tmp.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "demo-app"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
run = "deno main.ts"
"#,
        )
        .expect("write manifest");

        let _cwd_guard = CwdGuard::set_to(tmp.path());
        let summary = summarize_private_publish(&PrivatePublishRequest {
            registry_url: "https://example.invalid".to_string(),
            publisher_hint: Some("koh0920".to_string()),
            artifact_path: None,
            force_large_payload: false,
            scoped_id: None,
            allow_existing: false,
        })
        .expect("summarize");

        assert_eq!(summary.source, "build");
        assert_eq!(summary.version, "auto");
    }

    #[test]
    fn resolve_private_publisher_uses_publisher_hint_before_repository_owner() {
        let manifest_raw = r#"
schema_version = "0.2"
name = "demo-app"
version = "1.2.3"
type = "app"
default_target = "cli"

[metadata]
repository = "https://github.com/another-owner/demo-app"

[targets.cli]
runtime = "source"
driver = "deno"
"#;

        assert_eq!(
            resolve_private_publisher(Some("koh0920"), manifest_raw),
            "koh0920"
        );
    }

    #[test]
    fn resolve_private_publisher_falls_back_to_repository_owner_without_hint() {
        let manifest_raw = r#"
schema_version = "0.2"
name = "demo-app"
version = "1.2.3"
type = "app"
default_target = "cli"

[metadata]
repository = "https://github.com/another-owner/demo-app"

[targets.cli]
runtime = "source"
driver = "deno"
"#;

        assert_eq!(
            resolve_private_publisher(None, manifest_raw),
            "another-owner"
        );
    }

    #[test]
    fn normalize_segment_collapses_non_alnum_runs() {
        assert_eq!(normalize_segment("  Some_Owner  "), "some-owner");
    }
}
