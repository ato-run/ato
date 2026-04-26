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
    pub paid_large_payload: bool,
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
    if authoritative_input
        .desktop_source_publish_contract()
        .is_some()
    {
        anyhow::bail!(
            "--ci publish does not yet support Tauri/Electron/Wails source publish. Use private/local registry publish first."
        );
    }
    let (manifest_name, manifest_version) =
        semantic_publish_identity(&authoritative_input.descriptor)?;

    let tag = github.r#ref.strip_prefix("refs/tags/").unwrap_or_default();
    let resolved_version = normalize_tag_version(tag)?;
    if !manifest_version.trim().is_empty() && manifest_version.trim() != resolved_version {
        anyhow::bail!(
            "Tag/version mismatch: expected version {} from capsule.toml, got tag {}",
            manifest_version,
            github.r#ref
        );
    }

    let source_repo = authoritative_input
        .compatibility_input_repository()
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
        &manifest_name,
        &resolved_version,
        Some(&authoritative_input),
        None,
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
        args.paid_large_payload,
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

    let request_playground = authoritative_input.compatibility_store_playground_enabled();
    let metadata = CiMetadataPayload {
        capsule_slug: manifest_name.clone(),
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

fn semantic_publish_identity(
    descriptor: &capsule_core::router::ExecutionDescriptor,
) -> Result<(String, String)> {
    let name = descriptor
        .runtime_model
        .metadata
        .name
        .clone()
        .filter(|value| !value.trim().is_empty())
        .context("authoritative lock metadata is missing package name")?;
    let version = descriptor
        .runtime_model
        .metadata
        .version
        .clone()
        .unwrap_or_default();
    Ok((name, version))
}

pub(crate) fn build_capsule_artifact(
    name: &str,
    version: &str,
    authoritative_input: Option<&crate::application::producer_input::ProducerAuthoritativeInput>,
    manifest_path: Option<&Path>,
) -> Result<PathBuf> {
    build_capsule_artifact_with_output(name, version, authoritative_input, manifest_path, false)
}

pub(crate) fn build_capsule_artifact_with_output(
    name: &str,
    version: &str,
    authoritative_input: Option<&crate::application::producer_input::ProducerAuthoritativeInput>,
    manifest_path: Option<&Path>,
    stream_output: bool,
) -> Result<PathBuf> {
    let (decision, manifest_dir) = if let Some(authoritative_input) = authoritative_input {
        authoritative_input.validate_legacy_producer_bridge()?;
        (
            capsule_core::router::RuntimeDecision {
                kind: match authoritative_input
                    .descriptor
                    .execution_runtime()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .split('/')
                    .next()
                    .unwrap_or_default()
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
        let manifest_path = manifest_path.context(
            "manifest path is required when building a publish artifact without authoritative input",
        )?;
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
    let artifact_dir =
        capsule_core::common::paths::workspace_tmp_dir(&manifest_dir).join("ato-ci-artifacts");
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("Failed to create {}", artifact_dir.display()))?;
    let artifact_path = artifact_dir.join(format!("{}-{}.capsule", name, version));

    let native_plan =
        crate::build::native_delivery::detect_build_strategy_with_legacy_fallback(&decision.plan)?;

    if let Some(plan) = native_plan {
        let lock_json = authoritative_input
            .map(crate::application::producer_input::ProducerAuthoritativeInput::serialized_lock_json)
            .transpose()?;
        let result =
            crate::build::native_delivery::build_native_artifact_with_distribution_lock_output(
                &plan,
                Some(&artifact_path),
                lock_json.as_deref(),
                stream_output,
            )?;
        return Ok(result.artifact_path);
    }

    let reporter = std::sync::Arc::new(capsule_core::reporter::NoOpReporter)
        as std::sync::Arc<dyn capsule_core::reporter::CapsuleReporter + 'static>;
    let compat_input = if let Some(authoritative_input) = authoritative_input {
        authoritative_input.packaging_compat_project_input()?
    } else {
        decision.plan.compat_project_input()?
    };

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
                    compat_input: compat_input.clone(),
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
                        compat_input: compat_input.clone(),
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
                        compat_input: compat_input.clone(),
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
    use std::fs;
    use std::io::Read;
    use std::sync::Arc;

    #[cfg(target_os = "macos")]
    use super::build_capsule_artifact_with_output;
    use super::{build_capsule_artifact, normalize_tag_version, semantic_publish_identity};
    use crate::application::producer_input::resolve_producer_authoritative_input;
    use crate::reporters::CliReporter;

    fn write_gpui_wry_native_command_fixture(root: &std::path::Path) {
        fs::create_dir_all(root).expect("fixture dir");
        fs::write(
            root.join("capsule.toml"),
            r#"schema_version = "0.3"
name = "desktop-demo"
version = "0.1.0"
type = "app"

runtime = "source/native"
working_dir = "."
build = "sh build-app.sh"
run = "echo noop"
[artifact]
framework = "gpui-wry"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/Desktop Demo.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "dist/Desktop Demo.app"]
"#,
        )
        .expect("capsule.toml");
        fs::write(
            root.join("build-app.sh"),
            "#!/bin/sh\nset -eu\nmkdir -p 'dist/Desktop Demo.app/Contents/MacOS'\nprintf '#!/bin/sh\necho native\n' > 'dist/Desktop Demo.app/Contents/MacOS/Desktop Demo'\nchmod 755 'dist/Desktop Demo.app/Contents/MacOS/Desktop Demo'\n",
        )
        .expect("build-app.sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(root.join("build-app.sh"))
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(root.join("build-app.sh"), permissions).expect("chmod");
        }
    }

    fn read_artifact_manifest(path: &std::path::Path) -> String {
        let bytes = std::fs::read(path).expect("read artifact");
        let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let entry_path = entry.path().expect("path").to_string_lossy().to_string();
            if entry_path == "capsule.toml" {
                let mut manifest = String::new();
                entry.read_to_string(&mut manifest).expect("read manifest");
                return manifest;
            }
        }
        panic!("capsule.toml missing from artifact");
    }

    #[test]
    fn normalize_tag_version_strips_v_prefix() {
        assert_eq!(normalize_tag_version("v1.2.3").unwrap(), "1.2.3");
    }

    #[test]
    fn normalize_tag_version_rejects_empty_tag() {
        assert!(normalize_tag_version("").is_err());
    }

    #[test]
    fn authoritative_ci_build_does_not_materialize_project_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","version":"0.1.0","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        std::fs::write(
            tmp.path().join("package-lock.json"),
            r#"{"name":"demo","version":"0.1.0","lockfileVersion":3,"packages":{}}"#,
        )
        .expect("package-lock.json");
        std::fs::write(tmp.path().join("index.js"), "console.log('demo');\n").expect("index.js");

        let authoritative_input = resolve_producer_authoritative_input(
            tmp.path(),
            Arc::new(CliReporter::new(false)),
            false,
        )
        .expect("authoritative input");
        let (name, version) =
            semantic_publish_identity(&authoritative_input.descriptor).expect("identity");

        let _outcome = build_capsule_artifact(&name, &version, Some(&authoritative_input), None);
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn authoritative_ci_build_ignores_transitional_manifest_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"demo","version":"0.1.0","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        std::fs::write(
            tmp.path().join("package-lock.json"),
            r#"{"name":"demo","version":"0.1.0","lockfileVersion":3,"packages":{}}"#,
        )
        .expect("package-lock.json");
        std::fs::write(tmp.path().join("index.js"), "console.log('demo');\n").expect("index.js");

        let mut authoritative_input = resolve_producer_authoritative_input(
            tmp.path(),
            Arc::new(CliReporter::new(false)),
            false,
        )
        .expect("authoritative input");
        authoritative_input.descriptor.manifest_path = tmp.path().join("missing-capsule.toml");
        authoritative_input.descriptor.manifest_dir = tmp.path().join("missing-manifest-dir");

        let (name, version) =
            semantic_publish_identity(&authoritative_input.descriptor).expect("identity");
        let outcome = build_capsule_artifact(&name, &version, Some(&authoritative_input), None);
        if let Err(err) = &outcome {
            let message = err.to_string();
            assert!(
                !message.contains("missing-capsule.toml"),
                "build must not consult transitional manifest_path: {message}"
            );
            assert!(
                !message.contains("missing-manifest-dir"),
                "build must not consult transitional manifest_dir: {message}"
            );
        }
        assert!(!tmp.path().join("capsule.toml").exists());
    }

    #[test]
    fn authoritative_ci_build_preserves_exports_in_packaged_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("capsule.toml"),
            r#"
schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
runtime_version = "3.12"
run_command = "python3 default.py"

[targets.export]
runtime = "source"
driver = "python"
runtime_version = "3.12"
run_command = "python3 tool.py"
[exports.cli.tool]
kind = "python-tool"
target = "export"
args = ["--from-export"]
"#,
        )
        .expect("capsule.toml");
        std::fs::write(tmp.path().join("default.py"), "print('default')\n").expect("default.py");
        std::fs::write(tmp.path().join("tool.py"), "print('tool')\n").expect("tool.py");
        std::fs::write(
            tmp.path().join("pyproject.toml"),
            "[project]\nname='tool'\nversion='1.0.0'\n",
        )
        .expect("pyproject.toml");
        std::fs::write(tmp.path().join("uv.lock"), "version = 1\n").expect("uv.lock");

        let authoritative_input = resolve_producer_authoritative_input(
            tmp.path(),
            Arc::new(CliReporter::new(false)),
            false,
        )
        .expect("authoritative input");
        let (name, version) =
            semantic_publish_identity(&authoritative_input.descriptor).expect("identity");

        let artifact_path =
            build_capsule_artifact(&name, &version, Some(&authoritative_input), None)
                .expect("artifact");
        let manifest = read_artifact_manifest(&artifact_path);

        assert!(
            manifest.contains("[exports.cli.tool]"),
            "manifest was: {manifest}"
        );
        assert!(
            manifest.contains("target = \"export\""),
            "manifest was: {manifest}"
        );
        assert!(
            manifest.contains("args = [\"--from-export\"]"),
            "manifest was: {manifest}"
        );
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "darwin native command publish build requires a macOS host"
    )]
    fn authoritative_ci_build_supports_gpui_wry_native_command_projects() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_gpui_wry_native_command_fixture(tmp.path());

        let authoritative_input = resolve_producer_authoritative_input(
            tmp.path(),
            Arc::new(CliReporter::new(false)),
            false,
        )
        .expect("authoritative input");
        let (name, version) =
            semantic_publish_identity(&authoritative_input.descriptor).expect("identity");

        let artifact_path =
            build_capsule_artifact(&name, &version, Some(&authoritative_input), None)
                .expect("artifact");
        assert!(artifact_path.exists(), "artifact must exist");
        let manifest = read_artifact_manifest(&artifact_path);
        assert!(
            manifest.contains("name = \"desktop-demo\""),
            "manifest was: {manifest}"
        );
    }

    #[cfg(target_os = "macos")]
    fn ato_desktop_bundle_executable_present() -> Option<std::path::PathBuf> {
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = crate_root.parent().and_then(std::path::Path::parent)?;
        let macos_dir = repo_root
            .join("crates")
            .join("ato-desktop")
            .join("dist")
            .join("darwin-arm64")
            .join("Ato Desktop.app")
            .join("Contents")
            .join("MacOS");
        let entries = std::fs::read_dir(&macos_dir).ok()?;
        let has_executable = entries
            .filter_map(Result::ok)
            .any(|entry| entry.file_type().map(|ty| ty.is_file()).unwrap_or(false));
        has_executable.then(|| repo_root.join("crates").join("ato-desktop"))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn repository_ato_desktop_authoritative_ci_build_succeeds() {
        let Some(desktop_root) = ato_desktop_bundle_executable_present() else {
            eprintln!(
                "skipping repository_ato_desktop_authoritative_ci_build_succeeds: \
                 dist/darwin-arm64/Ato Desktop.app/Contents/MacOS/ has no executable; \
                 build the desktop bundle first to exercise this test"
            );
            return;
        };
        assert!(
            desktop_root.join("capsule.toml").is_file(),
            "capsule.toml missing"
        );

        let authoritative_input = resolve_producer_authoritative_input(
            &desktop_root,
            Arc::new(CliReporter::new(false)),
            false,
        )
        .expect("authoritative input");
        authoritative_input
            .ensure_desktop_source_publish_ready()
            .expect("desktop source publish readiness");
        let (name, version) =
            semantic_publish_identity(&authoritative_input.descriptor).expect("identity");

        let artifact_path =
            build_capsule_artifact(&name, &version, Some(&authoritative_input), None)
                .expect("artifact");
        assert!(artifact_path.exists(), "artifact must exist");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn repository_ato_desktop_authoritative_ci_build_succeeds_with_stream_output() {
        let Some(desktop_root) = ato_desktop_bundle_executable_present() else {
            eprintln!(
                "skipping repository_ato_desktop_authoritative_ci_build_succeeds_with_stream_output: \
                 dist/darwin-arm64/Ato Desktop.app/Contents/MacOS/ has no executable; \
                 build the desktop bundle first to exercise this test"
            );
            return;
        };

        let authoritative_input = resolve_producer_authoritative_input(
            &desktop_root,
            Arc::new(CliReporter::new(false)),
            false,
        )
        .expect("authoritative input");
        let (name, version) =
            semantic_publish_identity(&authoritative_input.descriptor).expect("identity");

        let artifact_path = build_capsule_artifact_with_output(
            &name,
            &version,
            Some(&authoritative_input),
            None,
            true,
        )
        .expect("artifact");
        assert!(artifact_path.exists(), "artifact must exist");
    }
}
