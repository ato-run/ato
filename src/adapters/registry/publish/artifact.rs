use std::io::{Cursor, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::application::ports::publish::PublishArtifactMetadata;
use crate::application::producer_input::publish_metadata_from_lock;
use crate::artifact_hash::{
    compute_blake3_label as compute_blake3, compute_sha256_label as compute_sha256,
};
use crate::publish::upload_strategy::{
    self, FinalizeUploadRequest, StartUploadRequest, TransferArtifactRequest,
    UploadArtifactDescriptor,
};

#[derive(Debug, Clone)]
pub struct PublishArtifactBytesArgs {
    pub artifact_bytes: Vec<u8>,
    pub scoped_id: String,
    pub registry_url: String,
    pub force_large_payload: bool,
    pub paid_large_payload: bool,
    pub allow_existing: bool,
    pub lock_id: Option<String>,
    pub closure_digest: Option<String>,
    pub publish_metadata: Option<PublishArtifactMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishArtifactResult {
    pub scoped_id: String,
    pub version: String,
    pub artifact_url: String,
    pub file_name: String,
    pub sha256: String,
    pub blake3: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub already_existed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closure_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_metadata: Option<PublishArtifactMetadata>,
}

#[derive(Debug)]
struct ArtifactPayload {
    publisher: String,
    slug: String,
    version: String,
    file_name: String,
    bytes: Vec<u8>,
    sha256: String,
    blake3: String,
    v3_manifest: Option<capsule_core::capsule_v3::CapsuleManifestV3>,
}

#[derive(Debug, Clone)]
pub(crate) struct V3SyncPayload {
    publisher: String,
    slug: String,
    version: String,
    manifest: capsule_core::capsule_v3::CapsuleManifestV3,
}

impl ArtifactPayload {
    fn v3_sync_payload(&self) -> Option<V3SyncPayload> {
        self.v3_manifest.as_ref().map(|manifest| V3SyncPayload {
            publisher: self.publisher.clone(),
            slug: self.slug.clone(),
            version: self.version.clone(),
            manifest: manifest.clone(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactManifestInfo {
    pub name: String,
    pub version: String,
    pub repository_owner: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifiedArtifactInfo {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub blake3: String,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct RegistryErrorPayload {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncChunkDescriptor {
    pub(crate) raw_hash: String,
    pub(crate) raw_size: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncNegotiateRequest {
    pub(crate) artifact_hash: String,
    pub(crate) schema_version: u32,
    pub(crate) chunks: Vec<SyncChunkDescriptor>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncNegotiateResponse {
    pub(crate) missing_chunks: Vec<String>,
    #[serde(default)]
    pub(crate) total_chunks: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ChunkUploadResponse {
    pub(crate) raw_hash: String,
    pub(crate) inserted: bool,
    pub(crate) zstd_size: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncCommitRequest {
    pub(crate) publisher: String,
    pub(crate) slug: String,
    pub(crate) version: String,
    pub(crate) manifest: capsule_core::capsule_v3::CapsuleManifestV3,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncCommitResponse {
    pub(crate) scoped_id: String,
    pub(crate) version: String,
    pub(crate) artifact_hash: String,
    pub(crate) chunk_count: usize,
    pub(crate) total_raw_size: u64,
}

#[derive(Debug, Error)]
pub enum PublishArtifactError {
    #[error("Artifact upload conflict (409 version_exists): {message}")]
    VersionExists { message: String },
    #[error("Managed Store direct publish cannot accept artifacts larger than the current conservative limit for {registry_url}: artifact is {size_bytes} bytes, limit is {limit_bytes} bytes")]
    ManagedStoreDirectPayloadLimitExceeded {
        registry_url: String,
        size_bytes: u64,
        limit_bytes: u64,
    },
    #[error("Managed Store direct publish does not support large payload override flags for {registry_url}: {message}")]
    ManagedStoreLargePayloadOverrideUnsupported {
        registry_url: String,
        message: String,
    },
    #[error("Artifact upload rejected as payload too large ({status}): {message}")]
    PayloadTooLarge { status: u16, message: String },
    #[error("Artifact upload failed ({status}): {message}")]
    UploadFailed { status: u16, message: String },
}

pub fn publish_artifact_bytes(args: PublishArtifactBytesArgs) -> Result<PublishArtifactResult> {
    let base_url = crate::registry::http::normalize_registry_url(&args.registry_url, "--registry")?;
    crate::payload_guard::ensure_payload_bytes_size(
        args.artifact_bytes.len() as u64,
        args.force_large_payload,
        args.paid_large_payload,
        "--force-large-payload",
    )?;
    let payload = load_artifact_payload_from_bytes(&args.artifact_bytes, &args.scoped_id)?;
    let strategy = upload_strategy::select_upload_strategy(&base_url);
    let descriptor = build_upload_artifact_descriptor(
        &payload,
        args.allow_existing,
        args.lock_id,
        args.closure_digest,
        args.publish_metadata,
    );
    let v3_sync_payload = payload.v3_sync_payload();
    let session = strategy.start_upload(&StartUploadRequest {
        registry_url: base_url.clone(),
        artifact: descriptor.clone(),
        force_large_payload: args.force_large_payload,
        paid_large_payload: args.paid_large_payload,
    })?;
    let transfer = strategy.transfer(TransferArtifactRequest {
        registry_url: base_url.clone(),
        session,
        artifact_bytes: payload.bytes,
    })?;

    strategy.finalize_upload(FinalizeUploadRequest {
        registry_url: base_url,
        artifact: descriptor,
        transfer,
        v3_sync_payload,
    })
}

pub fn inspect_artifact_manifest(path: &Path) -> Result<ArtifactManifestInfo> {
    if !path.exists() {
        bail!("Artifact not found: {}", path.display());
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| !ext.eq_ignore_ascii_case("capsule"))
        .unwrap_or(true)
    {
        bail!("--artifact must point to a .capsule file");
    }

    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read artifact: {}", path.display()))?;
    let manifest = extract_manifest_from_capsule(&bytes)?;
    let parsed = capsule_core::types::CapsuleManifest::from_toml(&manifest)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml from artifact: {}", err))?;

    Ok(ArtifactManifestInfo {
        name: parsed.name,
        version: parsed.version,
        repository_owner: extract_repository_owner(&manifest),
    })
}

pub fn verify_artifact(path: &Path) -> Result<VerifiedArtifactInfo> {
    if !path.exists() {
        bail!("Artifact not found: {}", path.display());
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| !ext.eq_ignore_ascii_case("capsule"))
        .unwrap_or(true)
    {
        bail!("--artifact must point to a .capsule file");
    }

    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read artifact: {}", path.display()))?;
    let manifest = extract_manifest_from_capsule(&bytes)?;
    let parsed = capsule_core::types::CapsuleManifest::from_toml(&manifest)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml from artifact: {}", err))?;
    let _ = extract_payload_v3_manifest_from_capsule(&bytes)?;

    Ok(VerifiedArtifactInfo {
        name: parsed.name,
        version: parsed.version,
        sha256: compute_sha256(&bytes),
        blake3: compute_blake3(&bytes),
        size_bytes: bytes.len() as u64,
    })
}

pub fn infer_publish_metadata_from_capsule_bytes(
    bytes: &[u8],
) -> Result<Option<PublishArtifactMetadata>> {
    if let Some(metadata) = infer_publish_metadata_from_finalized_payload(bytes)? {
        return Ok(Some(metadata));
    }

    if let Some(lock) = extract_capsule_lock_from_capsule(bytes)? {
        if let Some(metadata) = publish_metadata_from_lock(&lock) {
            return Ok(Some(metadata));
        }
    }

    if crate::build::native_delivery::detect_install_requires_local_derivation(bytes)?.is_some() {
        return Ok(Some(PublishArtifactMetadata {
            identity_class:
                crate::application::ports::publish::PublishArtifactIdentityClass::ImportedThirdPartyArtifact,
            delivery_mode: Some("artifact-import".to_string()),
            provenance_limited: true,
        }));
    }

    Ok(None)
}

pub(crate) fn build_upload_endpoint(
    base_url: &str,
    publisher: &str,
    slug: &str,
    version: &str,
    file_name: Option<&str>,
    allow_existing: bool,
) -> String {
    let mut endpoint = format!(
        "{}/v1/local/capsules/{}/{}/{}",
        base_url,
        urlencoding::encode(publisher),
        urlencoding::encode(slug),
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
    endpoint
}

fn build_upload_artifact_descriptor(
    payload: &ArtifactPayload,
    allow_existing: bool,
    lock_id: Option<String>,
    closure_digest: Option<String>,
    publish_metadata: Option<PublishArtifactMetadata>,
) -> UploadArtifactDescriptor {
    UploadArtifactDescriptor {
        publisher: payload.publisher.clone(),
        slug: payload.slug.clone(),
        version: payload.version.clone(),
        file_name: payload.file_name.clone(),
        sha256: payload.sha256.clone(),
        blake3: payload.blake3.clone(),
        size_bytes: payload.bytes.len() as u64,
        allow_existing,
        lock_id,
        closure_digest,
        publish_metadata,
    }
}

pub(crate) fn build_direct_upload_headers(
    artifact: &UploadArtifactDescriptor,
) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "content-type".to_string(),
            "application/octet-stream".to_string(),
        ),
        ("x-ato-sha256".to_string(), artifact.sha256.clone()),
        ("x-ato-blake3".to_string(), artifact.blake3.clone()),
    ];
    if let Some(lock_id) = artifact
        .lock_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        headers.push(("x-ato-lock-id".to_string(), lock_id.to_string()));
    }
    if let Some(closure_digest) = artifact
        .closure_digest
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        headers.push((
            "x-ato-closure-digest".to_string(),
            closure_digest.to_string(),
        ));
    }
    if let Some(metadata) = artifact.publish_metadata.as_ref() {
        headers.push((
            "x-ato-publish-identity-class".to_string(),
            metadata.identity_class.as_str().to_string(),
        ));
        if let Some(delivery_mode) = metadata
            .delivery_mode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            headers.push((
                "x-ato-publish-delivery-mode".to_string(),
                delivery_mode.to_string(),
            ));
        }
        headers.push((
            "x-ato-publish-provenance-limited".to_string(),
            if metadata.provenance_limited {
                "true".to_string()
            } else {
                "false".to_string()
            },
        ));
    }
    headers
}

fn load_artifact_payload_from_bytes(bytes: &[u8], scoped_id: &str) -> Result<ArtifactPayload> {
    let scoped = crate::install::parse_capsule_ref(scoped_id)?;
    let manifest = extract_manifest_from_capsule(bytes)?;
    let v3_manifest = extract_payload_v3_manifest_from_capsule(bytes)?;
    let parsed = capsule_core::types::CapsuleManifest::from_toml(&manifest)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml from artifact: {}", err))?;

    if parsed.name != scoped.slug {
        bail!(
            "--scoped-id slug '{}' must match artifact manifest.name '{}'",
            scoped.slug,
            parsed.name
        );
    }

    let version = parsed.version.trim();
    let file_name = if version.is_empty() {
        String::new()
    } else {
        format!("{}-{}.capsule", scoped.slug, version)
    };

    Ok(ArtifactPayload {
        publisher: scoped.publisher,
        slug: scoped.slug,
        version: if version.is_empty() {
            "auto".to_string()
        } else {
            version.to_string()
        },
        file_name,
        sha256: compute_sha256(bytes),
        blake3: compute_blake3(bytes),
        bytes: bytes.to_vec(),
        v3_manifest,
    })
}

fn extract_manifest_from_capsule(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path == "capsule.toml" {
            let mut manifest = String::new();
            entry
                .read_to_string(&mut manifest)
                .context("Failed to read capsule.toml from artifact")?;
            return Ok(manifest);
        }
    }

    bail!("Invalid artifact: capsule.toml not found in .capsule archive")
}

fn extract_capsule_lock_from_capsule(
    bytes: &[u8],
) -> Result<Option<capsule_core::ato_lock::AtoLock>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path != "capsule.lock.json" {
            continue;
        }
        let mut lock_bytes = Vec::new();
        entry
            .read_to_end(&mut lock_bytes)
            .context("Failed to read capsule.lock.json from artifact")?;
        let lock = serde_json::from_slice(&lock_bytes)
            .context("Failed to parse capsule.lock.json from artifact")?;
        return Ok(Some(lock));
    }

    Ok(None)
}

#[derive(Debug, Deserialize)]
struct LocalDerivationSummary {
    #[serde(default)]
    finalized_locally: bool,
}

fn infer_publish_metadata_from_finalized_payload(
    bytes: &[u8],
) -> Result<Option<PublishArtifactMetadata>> {
    let payload_tar = match crate::capsule_archive::extract_payload_tar_from_capsule(bytes) {
        Ok(payload_tar) => payload_tar,
        Err(_) => return Ok(None),
    };
    let mut archive = tar::Archive::new(Cursor::new(payload_tar));
    let entries = archive
        .entries()
        .context("Failed to read payload.tar entries from artifact")?;
    for entry in entries {
        let mut entry = entry.context("Invalid payload.tar entry in artifact")?;
        let entry_path = entry
            .path()
            .context("Failed to read payload entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path != "local-derivation.json" {
            continue;
        }
        let mut payload = Vec::new();
        entry
            .read_to_end(&mut payload)
            .context("Failed to read local-derivation.json from artifact")?;
        let derivation: LocalDerivationSummary = serde_json::from_slice(&payload)
            .context("Failed to parse local-derivation.json from artifact")?;
        if derivation.finalized_locally {
            return Ok(Some(PublishArtifactMetadata {
                identity_class:
                    crate::application::ports::publish::PublishArtifactIdentityClass::LocallyFinalizedSignedBundle,
                delivery_mode: Some("source-derivation".to_string()),
                provenance_limited: false,
            }));
        }
    }
    Ok(None)
}

fn extract_payload_v3_manifest_from_capsule(
    bytes: &[u8],
) -> Result<Option<capsule_core::capsule_v3::CapsuleManifestV3>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path != capsule_core::capsule_v3::V3_PAYLOAD_MANIFEST_PATH {
            continue;
        }

        let mut manifest_bytes = Vec::new();
        entry
            .read_to_end(&mut manifest_bytes)
            .context("Failed to read payload.v3.manifest.json from artifact")?;
        let manifest: capsule_core::capsule_v3::CapsuleManifestV3 =
            serde_json::from_slice(&manifest_bytes)
                .context("Failed to parse payload.v3.manifest.json from artifact")?;
        capsule_core::capsule_v3::verify_artifact_hash(&manifest)
            .context("Invalid payload.v3.manifest.json artifact_hash")?;
        return Ok(Some(manifest));
    }

    Ok(None)
}

pub(crate) fn sync_v3_chunks_if_present(
    base_url: &str,
    payload: Option<&V3SyncPayload>,
) -> Result<()> {
    let Some(payload) = payload else {
        return Ok(());
    };

    let cas = match capsule_core::capsule_v3::CasProvider::from_env() {
        capsule_core::capsule_v3::CasProvider::Enabled(store) => store,
        capsule_core::capsule_v3::CasProvider::Disabled(reason) => {
            capsule_core::capsule_v3::CasProvider::log_disabled_once(
                "publish_v3_chunk_sync",
                &reason,
            );
            return Ok(());
        }
    };

    let client = crate::registry::http::blocking_client_builder(base_url)
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create v3 sync client")?;
    let token = crate::registry::http::current_ato_token();

    let negotiate_request = SyncNegotiateRequest {
        artifact_hash: payload.manifest.artifact_hash.clone(),
        schema_version: payload.manifest.schema_version,
        chunks: payload
            .manifest
            .chunks
            .iter()
            .map(|chunk| SyncChunkDescriptor {
                raw_hash: chunk.raw_hash.clone(),
                raw_size: chunk.raw_size,
            })
            .collect(),
    };

    let negotiate_endpoint = format!("{}/v1/sync/negotiate", base_url);
    let mut negotiate = client.post(&negotiate_endpoint).json(&negotiate_request);
    if let Some(token) = token.as_deref() {
        negotiate = negotiate.bearer_auth(token);
    }
    let negotiate_response = negotiate.send().map_err(|err| {
        anyhow::anyhow!(
            "Failed to negotiate v3 payload sync via {}: {}",
            negotiate_endpoint,
            err
        )
    })?;
    if is_sync_not_supported_status(negotiate_response.status()) {
        return Ok(());
    }
    if !negotiate_response.status().is_success() {
        let status = negotiate_response.status();
        let body = negotiate_response.text().unwrap_or_default();
        bail!(
            "v3 sync negotiate failed ({}): {}",
            status.as_u16(),
            body.trim()
        );
    }
    let negotiate_body = negotiate_response
        .json::<SyncNegotiateResponse>()
        .context("Invalid v3 sync negotiate response")?;

    for chunk in payload.manifest.chunks.iter().filter(|chunk| {
        negotiate_body
            .missing_chunks
            .iter()
            .any(|hash| hash == &chunk.raw_hash)
    }) {
        let chunk_path = cas
            .chunk_path(&chunk.raw_hash)
            .with_context(|| format!("Failed to resolve local CAS chunk {}", chunk.raw_hash))?;
        let bytes = std::fs::read(&chunk_path)
            .with_context(|| format!("Failed to read local CAS chunk {}", chunk.raw_hash))?;
        let chunk_endpoint = format!(
            "{}/v1/chunks/{}",
            base_url,
            urlencoding::encode(&chunk.raw_hash)
        );
        let mut request = client
            .put(&chunk_endpoint)
            .header("content-type", "application/zstd")
            .header("x-raw-size", chunk.raw_size.to_string())
            .body(bytes);
        if let Some(token) = token.as_deref() {
            request = request.bearer_auth(token);
        }
        let response = request.send().map_err(|err| {
            anyhow::anyhow!(
                "Failed to upload v3 chunk {} via {}: {}",
                chunk.raw_hash,
                chunk_endpoint,
                err
            )
        })?;
        if is_sync_not_supported_status(response.status()) {
            return Ok(());
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            bail!(
                "v3 chunk upload failed for {} ({}) : {}",
                chunk.raw_hash,
                status.as_u16(),
                body.trim()
            );
        }
        response
            .json::<ChunkUploadResponse>()
            .context("Invalid v3 chunk upload response")?;
    }

    let commit_endpoint = format!("{}/v1/sync/commit", base_url);
    let mut commit = client.post(&commit_endpoint).json(&SyncCommitRequest {
        publisher: payload.publisher.clone(),
        slug: payload.slug.clone(),
        version: payload.version.clone(),
        manifest: payload.manifest.clone(),
    });
    if let Some(token) = token.as_deref() {
        commit = commit.bearer_auth(token);
    }
    let commit_response = commit.send().map_err(|err| {
        anyhow::anyhow!(
            "Failed to commit v3 payload sync via {}: {}",
            commit_endpoint,
            err
        )
    })?;
    if is_sync_not_supported_status(commit_response.status()) {
        return Ok(());
    }
    if !commit_response.status().is_success() {
        let status = commit_response.status();
        let body = commit_response.text().unwrap_or_default();
        bail!(
            "v3 sync commit failed ({}): {}",
            status.as_u16(),
            body.trim()
        );
    }
    commit_response
        .json::<SyncCommitResponse>()
        .context("Invalid v3 sync commit response")?;

    Ok(())
}

fn is_sync_not_supported_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
    )
}

pub(crate) fn classify_upload_failure(status: StatusCode, body: &str) -> PublishArtifactError {
    let parsed = serde_json::from_str::<RegistryErrorPayload>(body).ok();
    if is_version_exists_conflict(status, parsed.as_ref(), body) {
        let message = parsed
            .as_ref()
            .and_then(|v| v.message.as_deref())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("same version is already published")
            .to_string();
        return PublishArtifactError::VersionExists { message };
    }

    if status == StatusCode::PAYLOAD_TOO_LARGE {
        return PublishArtifactError::PayloadTooLarge {
            status: status.as_u16(),
            message: summarize_payload_too_large_message(parsed.as_ref(), body),
        };
    }

    let message = parsed
        .as_ref()
        .and_then(|v| v.message.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| body.trim())
        .to_string();

    PublishArtifactError::UploadFailed {
        status: status.as_u16(),
        message,
    }
}

// Temporary conservative gate for the current managed Store direct-upload path.
// This is intentionally lower than the observed failure size and is not a
// remote acceptance guarantee. Replace it with capability-based or presigned
// upload negotiation once the managed publish path no longer relies on single
// PUT upload through the edge request-body path.
pub(crate) const MANAGED_STORE_DIRECT_CONSERVATIVE_LIMIT_BYTES: u64 = 95 * 1024 * 1024;

pub(crate) fn enforce_managed_store_direct_upload_policy(
    registry_url: &str,
    size_bytes: u64,
    force_large_payload: bool,
    paid_large_payload: bool,
) -> Result<()> {
    if !is_managed_store_direct_registry(registry_url) {
        return Ok(());
    }

    if size_bytes > MANAGED_STORE_DIRECT_CONSERVATIVE_LIMIT_BYTES {
        return Err(
            PublishArtifactError::ManagedStoreDirectPayloadLimitExceeded {
                registry_url: registry_url.to_string(),
                size_bytes,
                limit_bytes: MANAGED_STORE_DIRECT_CONSERVATIVE_LIMIT_BYTES,
            }
            .into(),
        );
    }

    if !force_large_payload && !paid_large_payload {
        return Ok(());
    }

    let mut disabled_flags = Vec::new();
    if force_large_payload {
        disabled_flags.push("--force-large-payload");
    }
    if paid_large_payload {
        disabled_flags.push("--paid-large-payload");
    }

    Err(PublishArtifactError::ManagedStoreLargePayloadOverrideUnsupported {
        registry_url: registry_url.to_string(),
        message: format!(
            "{} cannot be used with the managed Store direct upload path. This registry still uploads via a single PUT through the edge path; use a private/local registry for large direct uploads until presigned upload is available.",
            disabled_flags.join(" and ")
        ),
    }
    .into())
}

pub(crate) fn is_managed_store_direct_registry(registry_url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(registry_url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("api.ato.run") || host.eq_ignore_ascii_case("staging.api.ato.run")
}

fn summarize_payload_too_large_message(
    parsed: Option<&RegistryErrorPayload>,
    raw_body: &str,
) -> String {
    let parsed_message = parsed
        .and_then(|value| value.message.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(message) = parsed_message {
        return message.to_string();
    }

    let raw = raw_body.trim();
    if raw.is_empty() {
        return "managed Store direct upload rejected the request body as too large before the registry accepted it".to_string();
    }
    if raw.starts_with('<') || raw.to_ascii_lowercase().contains("<html") {
        return "managed Store direct upload rejected the request body as too large at the edge before the registry accepted it".to_string();
    }

    raw.to_string()
}

fn is_version_exists_conflict(
    status: StatusCode,
    parsed: Option<&RegistryErrorPayload>,
    raw_body: &str,
) -> bool {
    if status != StatusCode::CONFLICT {
        return false;
    }

    if parsed
        .and_then(|v| v.error.as_deref())
        .map(|v| v.eq_ignore_ascii_case("version_exists"))
        .unwrap_or(false)
    {
        return true;
    }

    let message = parsed
        .and_then(|v| v.message.as_deref())
        .unwrap_or(raw_body)
        .to_ascii_lowercase();
    message.contains("same version is already published")
        || message.contains("version_exists")
        || message.contains("sha256 mismatch")
}

fn extract_repository_owner(manifest_raw: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    #![allow(dead_code)]

    use super::*;
    use std::collections::HashMap;
    use std::ffi::{OsStr, OsString};
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use axum::extract::{Path as AxumPath, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{post, put};
    use axum::{Json, Router};
    use capsule_core::capsule_v3::{set_artifact_hash, CapsuleManifestV3, ChunkMeta};
    use tar::Builder;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::time::sleep;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&OsStr>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[derive(Clone)]
    struct SyncMockState {
        missing_chunks: Vec<String>,
        fixed_put_status: Option<StatusCode>,
        per_hash_transient_failures: Arc<AsyncMutex<HashMap<String, usize>>>,
        uploaded_hashes: Arc<AsyncMutex<Vec<String>>>,
        put_attempts: Arc<AsyncMutex<HashMap<String, usize>>>,
        negotiate_calls: Arc<AtomicUsize>,
        commit_calls: Arc<AtomicUsize>,
        put_calls: Arc<AtomicUsize>,
        inflight_puts: Arc<AtomicUsize>,
        max_inflight_puts: Arc<AtomicUsize>,
        put_delay: Duration,
    }

    impl SyncMockState {
        fn new(missing_chunks: Vec<String>) -> Self {
            Self {
                missing_chunks,
                fixed_put_status: None,
                per_hash_transient_failures: Arc::new(AsyncMutex::new(HashMap::new())),
                uploaded_hashes: Arc::new(AsyncMutex::new(Vec::new())),
                put_attempts: Arc::new(AsyncMutex::new(HashMap::new())),
                negotiate_calls: Arc::new(AtomicUsize::new(0)),
                commit_calls: Arc::new(AtomicUsize::new(0)),
                put_calls: Arc::new(AtomicUsize::new(0)),
                inflight_puts: Arc::new(AtomicUsize::new(0)),
                max_inflight_puts: Arc::new(AtomicUsize::new(0)),
                put_delay: Duration::from_millis(0),
            }
        }

        fn with_fixed_put_status(mut self, status: StatusCode) -> Self {
            self.fixed_put_status = Some(status);
            self
        }

        fn with_put_delay(mut self, delay: Duration) -> Self {
            self.put_delay = delay;
            self
        }
    }

    struct InflightGuard {
        state: SyncMockState,
    }

    impl Drop for InflightGuard {
        fn drop(&mut self) {
            self.state.inflight_puts.fetch_sub(1, Ordering::SeqCst);
        }
    }

    async fn sync_mock_negotiate(
        State(state): State<SyncMockState>,
        _body: axum::body::Bytes,
    ) -> impl IntoResponse {
        state.negotiate_calls.fetch_add(1, Ordering::SeqCst);
        Json(serde_json::json!({
            "missing_chunks": state.missing_chunks,
        }))
    }

    async fn sync_mock_put_chunk(
        State(state): State<SyncMockState>,
        AxumPath(raw_hash): AxumPath<String>,
        _body: axum::body::Bytes,
    ) -> impl IntoResponse {
        state.put_calls.fetch_add(1, Ordering::SeqCst);
        let current = state.inflight_puts.fetch_add(1, Ordering::SeqCst) + 1;
        let _guard = InflightGuard {
            state: state.clone(),
        };
        update_max(&state.max_inflight_puts, current);

        let attempt = {
            let mut attempts = state.put_attempts.lock().await;
            let entry = attempts.entry(raw_hash.clone()).or_insert(0);
            *entry += 1;
            *entry
        };

        if !state.put_delay.is_zero() {
            sleep(state.put_delay).await;
        }

        if let Some(status) = state.fixed_put_status {
            return (status, Json(serde_json::json!({"error":"forced"}))).into_response();
        }

        let should_fail = {
            let mut failures = state.per_hash_transient_failures.lock().await;
            if let Some(remaining) = failures.get_mut(&raw_hash) {
                if *remaining > 0 {
                    *remaining -= 1;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if should_fail {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"transient","attempt":attempt})),
            )
                .into_response();
        }

        state.uploaded_hashes.lock().await.push(raw_hash);
        Json(serde_json::json!({"ok": true})).into_response()
    }

    async fn sync_mock_commit(
        State(state): State<SyncMockState>,
        _body: axum::body::Bytes,
    ) -> impl IntoResponse {
        state.commit_calls.fetch_add(1, Ordering::SeqCst);
        Json(serde_json::json!({"ok": true}))
    }

    fn update_max(max: &AtomicUsize, candidate: usize) {
        let mut current = max.load(Ordering::SeqCst);
        while candidate > current {
            match max.compare_exchange(current, candidate, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    async fn start_sync_mock_server(state: SyncMockState) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/v1/sync/negotiate", post(sync_mock_negotiate))
            .route("/v1/chunks/:raw_hash", put(sync_mock_put_chunk))
            .route("/v1/sync/commit", post(sync_mock_commit))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock");
        });
        (format!("http://{}", addr), handle)
    }

    fn compress_chunk(data: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
        encoder.write_all(data).expect("write");
        encoder.finish().expect("finish")
    }

    fn build_v3_sync_test_payload(
        cas: &capsule_core::capsule_v3::CasStore,
        chunk_count: usize,
    ) -> (V3SyncPayload, Vec<String>) {
        let mut chunks = Vec::new();
        let mut hashes = Vec::new();

        for i in 0..chunk_count {
            let raw = vec![(i % 251) as u8; 2_048 + (i % 7) * 13];
            let raw_hash = capsule_core::capsule_v3::manifest::blake3_digest(&raw);
            let zstd = compress_chunk(&raw);
            cas.put_chunk_zstd(&raw_hash, &zstd)
                .expect("write local CAS chunk");
            hashes.push(raw_hash.clone());
            chunks.push(ChunkMeta {
                raw_hash,
                raw_size: raw.len() as u32,
                zstd_size_hint: Some(zstd.len() as u32),
            });
        }

        let mut manifest = CapsuleManifestV3::new(chunks);
        set_artifact_hash(&mut manifest).expect("artifact hash");

        (
            V3SyncPayload {
                publisher: "local".to_string(),
                slug: "sync-app".to_string(),
                version: "1.0.0".to_string(),
                manifest,
            },
            hashes,
        )
    }

    fn test_capsule_bytes(name: &str, version: &str) -> Vec<u8> {
        let manifest = format!(
            r#"schema_version = "0.2"
name = "{name}"
version = "{version}"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
entrypoint = "main.ts"
"#
        );
        let mut buf = Vec::<u8>::new();
        {
            let mut builder = Builder::new(&mut buf);
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(manifest.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "capsule.toml", manifest.as_bytes())
                .expect("append manifest");

            let sig = r#"{"signed":false}"#;
            let mut sig_header = tar::Header::new_gnu();
            sig_header.set_mode(0o644);
            sig_header.set_size(sig.len() as u64);
            sig_header.set_cksum();
            builder
                .append_data(&mut sig_header, "signature.json", sig.as_bytes())
                .expect("append signature");
            builder.finish().expect("finish tar");
        }
        buf
    }

    fn test_capsule_bytes_with_v3_manifest(name: &str, version: &str) -> Vec<u8> {
        let bytes = test_capsule_bytes(name, version);
        let mut v3 = CapsuleManifestV3::new(vec![ChunkMeta {
            raw_hash: "blake3:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .to_string(),
            raw_size: 0,
            zstd_size_hint: Some(0),
        }]);
        // Keep valid non-zero chunk size for validation path.
        v3.chunks[0].raw_size = 1;
        v3.total_raw_size = 1;
        set_artifact_hash(&mut v3).expect("set artifact hash");

        let mut rebuilt = Vec::<u8>::new();
        {
            let mut builder = Builder::new(&mut rebuilt);
            let mut archive = tar::Archive::new(Cursor::new(bytes));
            let entries = archive.entries().expect("entries");
            for entry in entries {
                let mut entry = entry.expect("entry");
                let path = entry.path().expect("path").to_string_lossy().to_string();
                let mut content = Vec::new();
                entry.read_to_end(&mut content).expect("read entry");
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_size(content.len() as u64);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, Cursor::new(content))
                    .expect("append existing entry");
            }

            let manifest_bytes = serde_jcs::to_vec(&v3).expect("serialize v3");
            let mut v3_header = tar::Header::new_gnu();
            v3_header.set_mode(0o644);
            v3_header.set_size(manifest_bytes.len() as u64);
            v3_header.set_cksum();
            builder
                .append_data(
                    &mut v3_header,
                    capsule_core::capsule_v3::V3_PAYLOAD_MANIFEST_PATH,
                    Cursor::new(manifest_bytes),
                )
                .expect("append v3 manifest");
            builder.finish().expect("finish tar");
        }
        rebuilt
    }

    fn build_native_source_lock_bytes() -> Vec<u8> {
        let mut lock = capsule_core::ato_lock::AtoLock::default();
        lock.contract.entries.insert(
            "delivery".to_string(),
            serde_json::json!({
                "mode": "source-derivation",
                "artifact": {
                    "kind": "desktop-native",
                    "framework": "tauri",
                    "target": "darwin/arm64",
                    "provenance_limited": false
                },
                "build": {
                    "closure_status": "complete"
                }
            }),
        );
        lock.resolution.entries.insert(
            "closure".to_string(),
            serde_json::json!({
                "kind": "build_closure",
                "status": "complete"
            }),
        );
        serde_json::to_vec(&lock).expect("serialize source lock")
    }

    fn build_native_capsule_with_optional_metadata(
        lock_json: Option<Vec<u8>>,
        local_derivation_json: Option<&str>,
    ) -> Vec<u8> {
        let manifest = r#"schema_version = "0.2"
name = "demo-native"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
entrypoint = "Demo.app"
"#;
        let delivery = r#"schema_version = "0.1"

[artifact]
framework = "tauri"
stage = "unsigned"
target = "darwin/arm64"
input = "Demo.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "Demo.app"]
"#;

        let mut payload_tar = Vec::new();
        {
            let mut builder = Builder::new(&mut payload_tar);
            for (path, bytes) in [
                ("ato.delivery.toml", delivery.as_bytes()),
                ("Demo.app/Contents/MacOS/demo", b"binary".as_slice()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o755);
                header.set_size(bytes.len() as u64);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, bytes)
                    .expect("append payload entry");
            }
            if let Some(local_derivation_json) = local_derivation_json {
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_size(local_derivation_json.len() as u64);
                header.set_cksum();
                builder
                    .append_data(
                        &mut header,
                        "local-derivation.json",
                        local_derivation_json.as_bytes(),
                    )
                    .expect("append local derivation");
            }
            builder.finish().expect("finish payload tar");
        }
        let payload_tar_zst =
            zstd::stream::encode_all(Cursor::new(payload_tar), 3).expect("encode payload");

        let mut artifact = Vec::new();
        {
            let mut builder = Builder::new(&mut artifact);
            let mut manifest_header = tar::Header::new_gnu();
            manifest_header.set_mode(0o644);
            manifest_header.set_size(manifest.len() as u64);
            manifest_header.set_cksum();
            builder
                .append_data(&mut manifest_header, "capsule.toml", manifest.as_bytes())
                .expect("append manifest");
            if let Some(lock_json) = lock_json {
                let mut lock_header = tar::Header::new_gnu();
                lock_header.set_mode(0o644);
                lock_header.set_size(lock_json.len() as u64);
                lock_header.set_cksum();
                builder
                    .append_data(
                        &mut lock_header,
                        "capsule.lock.json",
                        Cursor::new(lock_json),
                    )
                    .expect("append lock");
            }
            let mut payload_header = tar::Header::new_gnu();
            payload_header.set_mode(0o644);
            payload_header.set_size(payload_tar_zst.len() as u64);
            payload_header.set_cksum();
            builder
                .append_data(
                    &mut payload_header,
                    "payload.tar.zst",
                    Cursor::new(payload_tar_zst),
                )
                .expect("append payload");
            builder.finish().expect("finish artifact tar");
        }
        artifact
    }

    #[test]
    fn extract_manifest_from_capsule_succeeds() {
        let bytes = test_capsule_bytes("sample-capsule", "1.0.0");
        let manifest = extract_manifest_from_capsule(&bytes).expect("extract manifest");
        assert!(manifest.contains("name = \"sample-capsule\""));
        assert!(manifest.contains("version = \"1.0.0\""));
    }

    #[test]
    fn infer_publish_metadata_from_source_derived_native_capsule_prefers_embedded_lock() {
        let bytes = build_native_capsule_with_optional_metadata(
            Some(build_native_source_lock_bytes()),
            None,
        );

        let metadata = infer_publish_metadata_from_capsule_bytes(&bytes)
            .expect("infer metadata")
            .expect("metadata");
        assert_eq!(
            metadata.identity_class,
            crate::application::ports::publish::PublishArtifactIdentityClass::SourceDerivedUnsignedBundle
        );
        assert_eq!(metadata.delivery_mode.as_deref(), Some("source-derivation"));
        assert!(!metadata.provenance_limited);
    }

    #[test]
    fn infer_publish_metadata_from_finalized_native_capsule_marks_signed_bundle() {
        let bytes = build_native_capsule_with_optional_metadata(
            Some(build_native_source_lock_bytes()),
            Some(r#"{"finalized_locally":true}"#),
        );

        let metadata = infer_publish_metadata_from_capsule_bytes(&bytes)
            .expect("infer metadata")
            .expect("metadata");
        assert_eq!(
            metadata.identity_class,
            crate::application::ports::publish::PublishArtifactIdentityClass::LocallyFinalizedSignedBundle
        );
        assert_eq!(metadata.delivery_mode.as_deref(), Some("source-derivation"));
        assert!(!metadata.provenance_limited);
    }

    #[test]
    fn infer_publish_metadata_from_native_capsule_without_lock_marks_imported_artifact() {
        let bytes = build_native_capsule_with_optional_metadata(None, None);

        let metadata = infer_publish_metadata_from_capsule_bytes(&bytes)
            .expect("infer metadata")
            .expect("metadata");
        assert_eq!(
            metadata.identity_class,
            crate::application::ports::publish::PublishArtifactIdentityClass::ImportedThirdPartyArtifact
        );
        assert_eq!(metadata.delivery_mode.as_deref(), Some("artifact-import"));
        assert!(metadata.provenance_limited);
    }

    #[test]
    fn slug_mismatch_is_rejected() {
        let bytes = test_capsule_bytes("sample-capsule", "1.0.0");
        let err = load_artifact_payload_from_bytes(&bytes, "koh0920/another-slug")
            .expect_err("must fail");
        assert!(err
            .to_string()
            .contains("must match artifact manifest.name"));
    }

    #[test]
    fn hash_generation_is_stable() {
        let data = b"capsule-bytes";
        let s1 = compute_sha256(data);
        let s2 = compute_sha256(data);
        let b1 = compute_blake3(data);
        let b2 = compute_blake3(data);
        assert_eq!(s1, s2);
        assert_eq!(b1, b2);
        assert!(s1.starts_with("sha256:"));
        assert!(b1.starts_with("blake3:"));
    }

    #[test]
    fn build_upload_endpoint_appends_allow_existing() {
        let endpoint = build_upload_endpoint(
            "http://127.0.0.1:8787",
            "local",
            "demo-app",
            "1.0.0",
            Some("demo-app-1.0.0.capsule"),
            true,
        );
        assert!(endpoint.contains("allow_existing=true"));
        assert!(endpoint.contains("file_name=demo-app-1.0.0.capsule"));
    }

    #[test]
    fn build_upload_endpoint_omits_allow_existing_by_default() {
        let endpoint = build_upload_endpoint(
            "http://127.0.0.1:8787",
            "local",
            "demo-app",
            "1.0.0",
            Some("demo-app-1.0.0.capsule"),
            false,
        );
        assert!(!endpoint.contains("allow_existing="));
    }

    #[test]
    fn build_upload_endpoint_uses_canonical_api_base() {
        let endpoint = build_upload_endpoint(
            "https://api.ato.run",
            "koh0920",
            "demo-app",
            "1.0.0",
            Some("demo-app-1.0.0.capsule"),
            false,
        );

        assert!(
            endpoint.starts_with("https://api.ato.run/v1/local/capsules/koh0920/demo-app/1.0.0")
        );
    }

    #[test]
    fn build_upload_endpoint_omits_file_name_when_version_is_auto() {
        let endpoint = build_upload_endpoint(
            "http://127.0.0.1:8787",
            "local",
            "demo-app",
            "auto",
            None,
            false,
        );

        assert!(!endpoint.contains("file_name="));
        assert!(endpoint.ends_with("/auto"));
    }

    #[test]
    fn classify_upload_failure_detects_version_exists_from_status_and_message() {
        let err = classify_upload_failure(
            StatusCode::CONFLICT,
            r#"{"error":"other","message":"same version is already published"}"#,
        );
        assert!(matches!(err, PublishArtifactError::VersionExists { .. }));
    }

    #[test]
    fn managed_store_large_payload_override_is_disabled() {
        let err = enforce_managed_store_direct_upload_policy(
            "https://api.ato.run",
            MANAGED_STORE_DIRECT_CONSERVATIVE_LIMIT_BYTES,
            true,
            false,
        )
        .expect_err("managed store should reject override flags");

        assert!(err.to_string().contains("--force-large-payload"));
        assert!(err.to_string().contains("managed Store direct upload path"));
    }

    #[test]
    fn custom_registry_still_allows_large_payload_override_flags() {
        enforce_managed_store_direct_upload_policy(
            "http://127.0.0.1:8787",
            MANAGED_STORE_DIRECT_CONSERVATIVE_LIMIT_BYTES + 1,
            true,
            true,
        )
        .expect("custom registry should keep override support");
    }

    #[test]
    fn managed_store_large_payload_is_rejected_before_upload() {
        let err = enforce_managed_store_direct_upload_policy(
            "https://api.ato.run",
            MANAGED_STORE_DIRECT_CONSERVATIVE_LIMIT_BYTES + 1,
            false,
            false,
        )
        .expect_err("managed store should reject over-limit payloads");

        assert!(matches!(
            err.downcast_ref::<PublishArtifactError>(),
            Some(PublishArtifactError::ManagedStoreDirectPayloadLimitExceeded { .. })
        ));
    }

    #[test]
    fn classify_upload_failure_maps_413_html_to_payload_too_large() {
        let err = classify_upload_failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "<html>413 Payload Too Large</html>",
        );

        assert!(matches!(
            err,
            PublishArtifactError::PayloadTooLarge { status: 413, .. }
        ));
        assert!(err.to_string().contains("too large at the edge"));
    }
}
