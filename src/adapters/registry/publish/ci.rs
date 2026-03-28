use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use capsule_core::CapsuleReporter;
use ed25519_dalek::Signer;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

use crate::application::producer_input::resolve_producer_authoritative_input;

use crate::artifact_hash::{compute_blake3_label, compute_sha256_hex};

const DEFAULT_STORE_API_URL: &str = "https://api.ato.run";
const ENV_STORE_API_URL: &str = "ATO_STORE_API_URL";
const OIDC_AUDIENCE: &str = "api.ato.run";

#[derive(Debug, Clone)]
pub struct PublishCiArgs {
    pub json_output: bool,
    pub force_large_payload: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishCiResult {
    pub publish_event_id: String,
    pub release_id: String,
    pub artifact_id: String,
    pub verification_status: String,
    pub rejection_reason: Option<String>,
    pub capsule_scoped_id: String,
    pub version: String,
    pub artifact_sha256: Option<String>,
    pub artifact_blake3: Option<String>,
    pub urls: PublishUrls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishUrls {
    pub store: String,
    pub playground: Option<String>,
}

#[derive(Debug)]
struct GitHubContext {
    repository: String,
    r#ref: String,
    ref_type: String,
    sha: String,
    workflow_ref: String,
    run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DidSignaturePayload {
    algorithm: String,
    public_key: String,
    content_hash: String,
    signature: String,
    signed_at: i64,
}

#[derive(Debug, Serialize)]
struct CiMetadataPayload {
    capsule_slug: String,
    version: String,
    source_repo: String,
    source_commit: String,
    workflow_ref: String,
    workflow_run_id: Option<String>,
    builder_identity: String,
    idempotency_key: String,
    did_signature: DidSignaturePayload,
    artifact_sha256: String,
    artifact_blake3: String,
    file_name: String,
    platform_os: String,
    platform_arch: String,
    release_notes: String,
    request_playground: bool,
}

pub async fn execute(
    args: PublishCiArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<PublishCiResult> {
    let github = load_github_context()?;
    if github.ref_type != "tag" {
        anyhow::bail!(
            "--ci mode requires GITHUB_REF_TYPE=tag (got '{}')",
            github.ref_type
        );
    }

    let oidc_token = acquire_oidc_token().await?;

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let authoritative_input = resolve_producer_authoritative_input(&cwd, reporter.clone(), false)?;
    let manifest_path = authoritative_input.manifest_path.clone();
    let manifest_raw = authoritative_input.manifest_raw.clone();
    let manifest = authoritative_input.manifest.clone();

    let tag = github.r#ref.strip_prefix("refs/tags/").unwrap_or_default();
    let resolved_version = normalize_tag_version(tag)?;
    if !manifest.version.trim().is_empty() && manifest.version.trim() != resolved_version {
        anyhow::bail!(
            "Tag/version mismatch: expected version {} from capsule.toml, got tag {}",
            manifest.version,
            github.r#ref
        );
    }

    let source_repo = find_manifest_repository(&manifest_raw)
        .and_then(|v| normalize_source_repo(&v).ok())
        .unwrap_or_else(|| github.repository.clone());
    if source_repo != github.repository {
        anyhow::bail!(
            "GITHUB_REPOSITORY '{}' does not match capsule repository '{}'",
            github.repository,
            source_repo
        );
    }

    if !args.json_output {
        reporter
            .progress_start(
                "📦 [publish] Building capsule artifact for CI publish...".to_string(),
                None,
            )
            .await?;
    }
    let artifact_path = build_capsule_artifact(
        &manifest_path,
        &manifest.name,
        &resolved_version,
        Some(&authoritative_input),
    );
    if !args.json_output {
        reporter.progress_finish(None).await?;
    }
    let artifact_path = artifact_path?;
    if !args.json_output {
        println!("✅ CI artifact built: {}", artifact_path.display());
    }
    crate::payload_guard::ensure_payload_size(
        &artifact_path,
        args.force_large_payload,
        "--force-large-payload",
    )?;
    let artifact_bytes = fs::read(&artifact_path)
        .with_context(|| format!("Failed to read artifact: {}", artifact_path.display()))?;
    let artifact_sha256 = compute_sha256_hex(&artifact_bytes);
    let artifact_blake3 = compute_blake3_label(&artifact_bytes);

    let did_signature = build_ephemeral_signature(&artifact_blake3);
    let file_name = artifact_path
        .file_name()
        .and_then(|v| v.to_str())
        .map(|v| v.to_string())
        .context("Failed to derive artifact file name")?;

    let request_playground = manifest_store_playground_enabled(&manifest_raw);
    let metadata = CiMetadataPayload {
        capsule_slug: manifest.name.clone(),
        version: resolved_version.clone(),
        source_repo: source_repo.clone(),
        source_commit: github.sha.clone(),
        workflow_ref: github.workflow_ref.clone(),
        workflow_run_id: github.run_id.clone(),
        builder_identity: format!("github-actions:{}", github.workflow_ref),
        idempotency_key: format!("{}:{}:{}", source_repo, tag, github.sha),
        did_signature: did_signature.clone(),
        artifact_sha256,
        artifact_blake3,
        file_name: file_name.clone(),
        platform_os: read_env_trimmed("RUNNER_OS").unwrap_or_else(|| "linux".to_string()),
        platform_arch: read_env_trimmed("RUNNER_ARCH").unwrap_or_else(|| "x64".to_string()),
        release_notes: String::new(),
        request_playground,
    };

    let registry_url = resolve_store_api_base_url();
    let endpoint = format!("{}/v1/publish/ci", registry_url);

    let metadata_json = serde_json::to_string(&metadata).context("Failed to serialize metadata")?;
    let form = reqwest::multipart::Form::new()
        .text("metadata", metadata_json)
        .part(
            "artifact",
            reqwest::multipart::Part::bytes(artifact_bytes)
                .file_name(file_name)
                .mime_str("application/octet-stream")?,
        );

    if !args.json_output {
        reporter
            .progress_start(
                "📤 [publish] Uploading artifact to Store API...".to_string(),
                None,
            )
            .await?;
    }
    let response = reqwest::Client::new()
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", oidc_token))
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("Failed to upload CI artifact to {}", endpoint));
    if !args.json_output {
        reporter.progress_finish(None).await?;
    }
    let response = response?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("{}", classify_ci_publish_http_error(status, &body));
    }

    let result = serde_json::from_str::<PublishCiResult>(&body)
        .context("Invalid /v1/publish/ci response payload")?;

    if !args.json_output {
        println!("CI publish mode: keyless ephemeral Ed25519 signature");
        println!("CI did:key: {}", did_signature.public_key);
    }

    Ok(result)
}

fn load_github_context() -> Result<GitHubContext> {
    Ok(GitHubContext {
        repository: required_env("GITHUB_REPOSITORY")?,
        r#ref: required_env("GITHUB_REF")?,
        ref_type: required_env("GITHUB_REF_TYPE")?,
        sha: required_env("GITHUB_SHA")?,
        workflow_ref: required_env("GITHUB_WORKFLOW_REF")?,
        run_id: read_env_trimmed("GITHUB_RUN_ID"),
    })
}

fn required_env(key: &str) -> Result<String> {
    read_env_trimmed(key).with_context(|| format!("{} is required in --ci mode", key))
}

fn read_env_trimmed(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Deserialize)]
struct OidcTokenResponse {
    value: String,
}

async fn acquire_oidc_token() -> Result<String> {
    if let Some(token) = read_env_trimmed("ATO_OIDC_TOKEN") {
        return Ok(token);
    }

    let request_url = required_env("ACTIONS_ID_TOKEN_REQUEST_URL")
        .context("ACTIONS_ID_TOKEN_REQUEST_URL is required when ATO_OIDC_TOKEN is not set")?;
    let request_token = required_env("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .context("ACTIONS_ID_TOKEN_REQUEST_TOKEN is required when ATO_OIDC_TOKEN is not set")?;

    let separator = if request_url.contains('?') { "&" } else { "?" };
    let url = format!(
        "{request_url}{separator}audience={}",
        urlencoding::encode(OIDC_AUDIENCE)
    );

    let payload = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", request_token))
        .send()
        .await
        .with_context(|| "Failed to request GitHub OIDC token")?
        .error_for_status()
        .with_context(|| "Failed to request GitHub OIDC token")?
        .json::<OidcTokenResponse>()
        .await
        .with_context(|| "Failed to parse GitHub OIDC token response")?;

    let token = payload.value.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("GitHub OIDC token response did not include token value");
    }
    Ok(token)
}

fn resolve_store_api_base_url() -> String {
    read_env_trimmed(ENV_STORE_API_URL)
        .as_deref()
        .map(trim_trailing_slash)
        .unwrap_or_else(|| DEFAULT_STORE_API_URL.to_string())
}

fn trim_trailing_slash(input: &str) -> String {
    input.trim_end_matches('/').to_string()
}

fn normalize_tag_version(tag: &str) -> Result<String> {
    let trimmed = tag.trim();
    let without_prefix = trimmed.strip_prefix('v').unwrap_or(trimmed);
    if without_prefix.is_empty() {
        anyhow::bail!("Git tag version is empty")
    }
    Ok(without_prefix.to_string())
}

fn find_manifest_repository(manifest_raw: &str) -> Option<String> {
    let parsed = toml::from_str::<toml::Value>(manifest_raw).ok()?;
    parsed
        .get("metadata")
        .and_then(|v| v.get("repository"))
        .and_then(|v| v.as_str())
        .or_else(|| parsed.get("repository").and_then(|v| v.as_str()))
        .map(|v| v.to_string())
}

fn manifest_store_playground_enabled(manifest_raw: &str) -> bool {
    let Ok(parsed) = toml::from_str::<toml::Value>(manifest_raw) else {
        return false;
    };
    parsed
        .get("store")
        .and_then(|v| v.get("playground"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn normalize_source_repo(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("repository is empty");
    }

    if raw.contains("://") {
        let parsed = reqwest::Url::parse(raw).with_context(|| "Invalid repository URL")?;
        if !parsed
            .host_str()
            .map(|h| h.eq_ignore_ascii_case("github.com"))
            .unwrap_or(false)
        {
            anyhow::bail!("repository must point to github.com");
        }
        let mut segs = parsed
            .path_segments()
            .context("repository URL has no path segments")?;
        let owner = segs.next().unwrap_or("").trim();
        let repo = segs.next().unwrap_or("").trim_end_matches(".git").trim();
        if owner.is_empty() || repo.is_empty() {
            anyhow::bail!("repository URL must include owner/repo");
        }
        return Ok(format!("{}/{}", owner, repo));
    }

    let mut it = raw.split('/');
    let owner = it.next().unwrap_or("").trim();
    let repo = it.next().unwrap_or("").trim_end_matches(".git").trim();
    if owner.is_empty() || repo.is_empty() || it.next().is_some() {
        anyhow::bail!("repository must be 'owner/repo' or GitHub URL");
    }
    Ok(format!("{}/{}", owner, repo))
}

pub(crate) fn build_capsule_artifact(
    manifest_path: &Path,
    name: &str,
    version: &str,
    authoritative_input: Option<&crate::application::producer_input::ProducerAuthoritativeInput>,
) -> Result<PathBuf> {
    let (decision, manifest_dir) = if let Some(authoritative_input) = authoritative_input {
        authoritative_input.validate_compat_bridge()?;
        (
            capsule_core::router::RuntimeDecision {
                kind: match authoritative_input
                    .descriptor
                    .execution_runtime()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "source" | "native" => capsule_core::router::RuntimeKind::Source,
                    "web" => capsule_core::router::RuntimeKind::Web,
                    "wasm" => capsule_core::router::RuntimeKind::Wasm,
                    "oci" | "docker" | "youki" | "runc" => capsule_core::router::RuntimeKind::Oci,
                    other => anyhow::bail!("Unsupported runtime '{other}'"),
                },
                reason: format!(
                    "lock target {}",
                    authoritative_input.descriptor.selected_target_label()
                ),
                plan: authoritative_input.descriptor.clone(),
            },
            authoritative_input.descriptor.workspace_root.clone(),
        )
    } else {
        let manifest_dir = manifest_path.parent().ok_or_else(|| {
            anyhow::anyhow!("Manifest path has no parent: {}", manifest_path.display())
        })?;
        (
            capsule_core::router::route_manifest(
                manifest_path,
                capsule_core::router::ExecutionProfile::Release,
                None,
            )?,
            manifest_dir.to_path_buf(),
        )
    };
    let artifact_dir = std::env::temp_dir().join("ato-ci-artifacts");
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("Failed to create {}", artifact_dir.display()))?;
    let artifact_path = artifact_dir.join(format!("{}-{}.capsule", name, version));

    if let Some(plan) = crate::build::native_delivery::detect_build_strategy(&manifest_dir)? {
        let result =
            crate::build::native_delivery::build_native_artifact(&plan, Some(&artifact_path))?;
        return Ok(result.artifact_path);
    }

    let reporter = std::sync::Arc::new(capsule_core::reporter::NoOpReporter)
        as std::sync::Arc<dyn capsule_core::reporter::CapsuleReporter + 'static>;

    match decision.kind {
        capsule_core::router::RuntimeKind::Source => {
            let prepared_config =
                capsule_core::packers::source::prepare_source_config_from_descriptor(
                    &decision.plan,
                    "strict".to_string(),
                    false,
                )?;
            capsule_core::packers::source::pack(
                &decision.plan,
                capsule_core::packers::source::SourcePackOptions {
                    compat_manifest: decision.plan.compat_manifest.clone(),
                    workspace_root: decision.plan.workspace_root.clone(),
                    config_json: prepared_config.config_json.clone(),
                    config_path: prepared_config.config_path.clone(),
                    output: Some(artifact_path.clone()),
                    runtime: None,
                    skip_l1: false,
                    skip_validation: false,
                    nacelle_override: None,
                    standalone: false,
                    strict_manifest: false,
                    timings: false,
                },
                reporter,
            )?;
        }
        capsule_core::router::RuntimeKind::Web => {
            let driver = decision
                .plan
                .execution_driver()
                .map(|v| v.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if driver == "static" {
                capsule_core::packers::web::pack(
                    &decision.plan,
                    capsule_core::packers::web::WebPackOptions {
                        compat_manifest: decision.plan.compat_manifest.clone(),
                        workspace_root: decision.plan.workspace_root.clone(),
                        output: Some(artifact_path.clone()),
                    },
                    reporter,
                )?;
            } else {
                let prepared_config =
                    capsule_core::packers::source::prepare_source_config_from_descriptor(
                        &decision.plan,
                        "strict".to_string(),
                        false,
                    )?;
                capsule_core::packers::source::pack(
                    &decision.plan,
                    capsule_core::packers::source::SourcePackOptions {
                        compat_manifest: decision.plan.compat_manifest.clone(),
                        workspace_root: decision.plan.workspace_root.clone(),
                        config_json: prepared_config.config_json.clone(),
                        config_path: prepared_config.config_path.clone(),
                        output: Some(artifact_path.clone()),
                        runtime: None,
                        skip_l1: false,
                        skip_validation: false,
                        nacelle_override: None,
                        standalone: false,
                        strict_manifest: false,
                        timings: false,
                    },
                    reporter,
                )?;
            }
        }
        capsule_core::router::RuntimeKind::Wasm => {
            anyhow::bail!("--ci publish currently supports runtime=source/web only");
        }
        capsule_core::router::RuntimeKind::Oci => {
            anyhow::bail!("--ci publish currently supports runtime=source/web only");
        }
    }

    if !artifact_path.exists() {
        anyhow::bail!(
            "Build did not produce expected artifact: {}",
            artifact_path.display()
        );
    }

    Ok(artifact_path)
}

fn build_ephemeral_signature(content_hash: &str) -> DidSignaturePayload {
    let signing_key = ed25519_dalek::SigningKey::generate(&mut OsRng);
    let verify_key = signing_key.verifying_key();
    let did = capsule_core::types::identity::public_key_to_did(&verify_key.to_bytes());
    let signature = signing_key.sign(content_hash.as_bytes());
    DidSignaturePayload {
        algorithm: "Ed25519".to_string(),
        public_key: did,
        content_hash: content_hash.to_string(),
        signature: BASE64_STANDARD.encode(signature.to_bytes()),
        signed_at: chrono::Utc::now().timestamp(),
    }
}

fn classify_ci_publish_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let normalized = body.to_ascii_lowercase();
    let looks_like_legacy_contract = status == reqwest::StatusCode::BAD_REQUEST
        && normalized.contains("validation_error")
        && normalized.contains("artifact_url")
        && normalized.contains("did_signature")
        && normalized.contains("required");

    if looks_like_legacy_contract {
        return format!(
            "CI publish failed ({}): target Store API appears to be running legacy /v1/publish/ci contract (artifact_url JSON). \
This ato-cli sends OIDC multipart metadata+artifact. \
Deploy latest ato-store (OIDC multipart CI publish), or point ATO_STORE_API_URL to an updated environment.\nraw_response: {}",
            status, body
        );
    }

    format!("CI publish failed ({}): {}", status, body)
}

#[cfg(test)]
mod tests {
    use super::normalize_tag_version;

    #[test]
    fn normalize_tag_version_strips_v_prefix() {
        assert_eq!(normalize_tag_version("v1.2.3").unwrap(), "1.2.3");
    }

    #[test]
    fn normalize_tag_version_rejects_empty_tag() {
        assert!(normalize_tag_version("").is_err());
    }
}
