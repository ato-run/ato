use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};
use serde_json::json;

use capsule_core::types::signing::StoredKey;

use super::{
    FinalizeUploadRequest, StartUploadRequest, TransferArtifactRequest, TransferArtifactResponse,
    UploadPreflightRequest, UploadSession, UploadStrategy,
};

fn describe_curl_response(
    status_code: u16,
    response: &super::curl_upload::CurlUploadResponse,
) -> String {
    let status = reqwest::StatusCode::from_u16(status_code)
        .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let cf_ray = response.headers.get("cf-ray").cloned();
    let request_id = response.headers.get("x-request-id").cloned();
    let body = &response.body;

    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let (error_code, message) = match &parsed {
        Some(v) => (
            v.get("error").and_then(|x| x.as_str()).map(String::from),
            v.get("message").and_then(|x| x.as_str()).map(String::from),
        ),
        None => (None, None),
    };

    let mut parts = vec![format!("HTTP {}", status)];
    match (error_code, message) {
        (Some(e), Some(m)) if !m.is_empty() => parts.push(format!("{}: {}", e, m)),
        (Some(e), _) => parts.push(e),
        (_, Some(m)) if !m.is_empty() => parts.push(m),
        _ if !body.trim().is_empty() => parts.push(body.trim().to_string()),
        _ => {}
    }
    if let Some(id) = request_id {
        parts.push(format!("request_id={}", id));
    }
    if let Some(ray) = cf_ray {
        parts.push(format!("cf-ray={}", ray));
    }
    if status.is_server_error() {
        parts.push(
            "この障害はサーバー側の問題です。cf-ray / request_id を添えてサポートに連絡してください。"
                .to_string(),
        );
    }
    parts.join(" | ")
}

fn curl_auth_headers() -> Vec<(String, String)> {
    match crate::registry::http::current_ato_token() {
        Some(token) => vec![("authorization".to_string(), format!("Bearer {}", token))],
        None => Vec::new(),
    }
}

pub(crate) struct PresignedUploadSession {
    pub(crate) capsule_id: String,
    pub(crate) version: String,
    pub(crate) upload_url: String,
    pub(crate) already_existed: bool,
}

pub(crate) struct PresignedTransferArtifactResponse {
    pub(crate) capsule_id: String,
    pub(crate) version: String,
    pub(crate) already_existed: bool,
}

#[derive(Debug, Default)]
pub(crate) struct PresignedUploadStrategy;

impl UploadStrategy for PresignedUploadStrategy {
    fn validate_preflight(&self, _request: &UploadPreflightRequest) -> Result<()> {
        Ok(())
    }

    fn start_upload(&self, request: &StartUploadRequest) -> Result<UploadSession> {
        let publisher = fetch_publisher_identity(&request.registry_url)?;
        if publisher.handle != request.artifact.publisher {
            bail!(
                "publisher identity mismatch: current session belongs to '{}' but publish target is '{}'",
                publisher.handle,
                request.artifact.publisher
            );
        }

        let signing_key = load_publisher_signing_key()?;
        let did = signing_key.did()?;
        if did != publisher.author_did {
            bail!(
                "publisher signing key DID '{}' does not match store publisher DID '{}'; rerun `ato login` / publisher onboarding to refresh local publisher keys",
                did,
                publisher.author_did
            );
        }

        let capsule_id = resolve_or_create_capsule_id(&request.registry_url, &request.artifact)?;
        let release = create_release(
            &request.registry_url,
            &capsule_id,
            &request.artifact,
            &signing_key,
            &publisher.author_did,
        )?;

        Ok(UploadSession::Presigned(PresignedUploadSession {
            capsule_id,
            version: release.version,
            upload_url: release.upload_url,
            already_existed: release.already_existed,
        }))
    }

    fn transfer(&self, _request: TransferArtifactRequest) -> Result<TransferArtifactResponse> {
        let TransferArtifactRequest {
            session,
            artifact_bytes,
            ..
        } = _request;
        let UploadSession::Presigned(session) = session else {
            bail!("presigned upload strategy requires a presigned upload session")
        };

        let response = super::curl_upload::put_bytes(&session.upload_url, &artifact_bytes, &[])
            .with_context(|| {
                format!(
                    "Failed to upload artifact to presigned URL (size={} bytes)",
                    artifact_bytes.len()
                )
            })?;

        if !response.is_success() {
            let status = reqwest::StatusCode::from_u16(response.status)
                .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
            let error = super::super::artifact::classify_upload_failure(status, &response.body);
            return Err(error.into());
        }

        Ok(TransferArtifactResponse::Presigned(
            PresignedTransferArtifactResponse {
                capsule_id: session.capsule_id,
                version: session.version,
                already_existed: session.already_existed,
            },
        ))
    }

    fn finalize_upload(
        &self,
        request: FinalizeUploadRequest,
    ) -> Result<super::super::artifact::PublishArtifactResult> {
        let TransferArtifactResponse::Presigned(transfer) = request.transfer else {
            bail!("presigned upload strategy requires a presigned transfer response")
        };

        let response = super::curl_upload::post_json(
            &format!(
                "{}/v1/capsules/{}/releases/{}/finalize",
                request.registry_url, transfer.capsule_id, transfer.version
            ),
            b"{}",
            &curl_auth_headers(),
        )
        .context("Failed to finalize presigned artifact upload")?;

        if !response.is_success() {
            let status = reqwest::StatusCode::from_u16(response.status)
                .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
            let error = super::super::artifact::classify_upload_failure(status, &response.body);
            return Err(error.into());
        }

        Ok(super::super::artifact::PublishArtifactResult {
            scoped_id: format!("{}/{}", request.artifact.publisher, request.artifact.slug),
            version: transfer.version.clone(),
            artifact_url: build_download_url(
                &request.registry_url,
                &request.artifact.publisher,
                &request.artifact.slug,
                &transfer.version,
            ),
            file_name: request.artifact.file_name.clone(),
            sha256: request.artifact.sha256.clone(),
            blake3: request.artifact.blake3.clone(),
            size_bytes: request.artifact.size_bytes,
            already_existed: transfer.already_existed,
            lock_id: request.artifact.lock_id.clone(),
            closure_digest: request.artifact.closure_digest.clone(),
            publish_metadata: request.artifact.publish_metadata.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct PublisherIdentityResponse {
    handle: String,
    author_did: String,
}

#[derive(Debug, Deserialize)]
struct CapsuleLookupResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CreateCapsuleResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct CreateReleaseResponse {
    version: String,
    upload_url: String,
    #[serde(default)]
    already_existed: bool,
}

#[derive(Debug, Serialize)]
struct CreateCapsuleRequest {
    slug: String,
    name: String,
    description: String,
    category: String,
    #[serde(rename = "type")]
    capsule_type: String,
}

#[derive(Debug, Serialize)]
struct CreateReleaseRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    allow_existing: bool,
    content_hash: String,
    size_bytes: u64,
    manifest: serde_json::Value,
    capabilities: Vec<String>,
    release_notes: String,
    builder_identity: String,
    signature: CreateReleaseSignature,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateReleaseSignature {
    algorithm: String,
    public_key: String,
    content_hash: String,
    signature: String,
    signed_at: i64,
}

fn fetch_publisher_identity(registry_url: &str) -> Result<PublisherIdentityResponse> {
    let _ = crate::auth::require_session_token()?;
    let url = format!("{}/v1/publishers/me", registry_url);
    let response = super::curl_upload::get(&url, &curl_auth_headers())
        .context("Failed to fetch publisher identity")?;

    if !response.is_success() {
        bail!(
            "Failed to resolve publisher identity: {}",
            describe_curl_response(response.status, &response)
        );
    }

    serde_json::from_str::<PublisherIdentityResponse>(&response.body)
        .context("Failed to parse publisher identity response")
}

fn load_publisher_signing_key() -> Result<StoredKey> {
    let key_path = dirs::home_dir()
        .context("Cannot determine home directory for publisher signing key")?
        .join(".ato")
        .join("keys")
        .join("publisher-signing-key.json");

    if !key_path.exists() {
        bail!(
            "Publisher signing key not found at {}. Run `ato login` / publisher onboarding before using the presigned publish strategy.",
            key_path.display()
        );
    }

    StoredKey::read(&key_path).with_context(|| {
        format!(
            "Failed to read publisher signing key at {}",
            key_path.display()
        )
    })
}

fn resolve_or_create_capsule_id(
    registry_url: &str,
    artifact: &super::UploadArtifactDescriptor,
) -> Result<String> {
    let lookup_url = format!(
        "{}/v1/capsules/by/{}/{}?format=id",
        registry_url,
        urlencoding::encode(&artifact.publisher),
        urlencoding::encode(&artifact.slug)
    );
    let response = super::curl_upload::get(&lookup_url, &curl_auth_headers())
        .context("Failed to resolve scoped capsule id for presigned publish")?;

    if response.is_success() {
        return serde_json::from_str::<CapsuleLookupResponse>(&response.body)
            .map(|payload| payload.id)
            .context("Failed to parse scoped capsule lookup response");
    }

    if response.status != 404 {
        bail!(
            "Scoped capsule lookup failed: {}",
            describe_curl_response(response.status, &response)
        );
    }

    let create_body = CreateCapsuleRequest {
        slug: artifact.slug.clone(),
        name: artifact.slug.clone(),
        description: String::new(),
        category: "other".to_string(),
        capsule_type: "app".to_string(),
    };
    let body_bytes =
        serde_json::to_vec(&create_body).context("Failed to serialize create capsule body")?;
    let create_response = super::curl_upload::post_json(
        &format!("{}/v1/capsules", registry_url),
        &body_bytes,
        &curl_auth_headers(),
    )
    .context("Failed to create capsule for presigned publish")?;

    if create_response.is_success() {
        return serde_json::from_str::<CreateCapsuleResponse>(&create_response.body)
            .map(|payload| payload.id)
            .context("Failed to parse create capsule response");
    }

    if create_response.status == 409 {
        return resolve_or_create_capsule_id_retry_lookup(registry_url, artifact);
    }

    bail!(
        "Create capsule failed: {}",
        describe_curl_response(create_response.status, &create_response)
    )
}

fn resolve_or_create_capsule_id_retry_lookup(
    registry_url: &str,
    artifact: &super::UploadArtifactDescriptor,
) -> Result<String> {
    let lookup_url = format!(
        "{}/v1/capsules/by/{}/{}?format=id",
        registry_url,
        urlencoding::encode(&artifact.publisher),
        urlencoding::encode(&artifact.slug)
    );
    let response = super::curl_upload::get(&lookup_url, &curl_auth_headers())
        .context("Failed to re-fetch capsule after slug conflict")?;
    if !response.is_success() {
        bail!(
            "Scoped capsule lookup after slug conflict failed: {}",
            describe_curl_response(response.status, &response)
        );
    }
    serde_json::from_str::<CapsuleLookupResponse>(&response.body)
        .map(|payload| payload.id)
        .context("Failed to parse scoped capsule lookup response")
}

fn create_release(
    registry_url: &str,
    capsule_id: &str,
    artifact: &super::UploadArtifactDescriptor,
    signing_key: &StoredKey,
    author_did: &str,
) -> Result<CreateReleaseResponse> {
    let signing_key = signing_key.to_signing_key()?;
    let signature = signing_key.sign(artifact.blake3.as_bytes()).to_bytes();
    let body = CreateReleaseRequest {
        version: normalize_release_version(&artifact.version),
        allow_existing: artifact.allow_existing,
        content_hash: artifact.blake3.clone(),
        size_bytes: artifact.size_bytes,
        manifest: build_release_manifest(artifact),
        capabilities: Vec::new(),
        release_notes: String::new(),
        builder_identity: "ato-cli:presigned-upload".to_string(),
        signature: CreateReleaseSignature {
            algorithm: "Ed25519".to_string(),
            public_key: author_did.to_string(),
            content_hash: artifact.blake3.clone(),
            signature: BASE64_STANDARD.encode(signature),
            signed_at: chrono::Utc::now().timestamp(),
        },
    };
    let body_bytes =
        serde_json::to_vec(&body).context("Failed to serialize create release body")?;
    let response = super::curl_upload::post_json(
        &format!("{}/v1/capsules/{}/releases", registry_url, capsule_id),
        &body_bytes,
        &curl_auth_headers(),
    )
    .context("Failed to create release for presigned publish")?;

    if response.status == 409 {
        return Err(
            super::super::artifact::PublishArtifactError::VersionExists {
                message: "same version is already published".to_string(),
            }
            .into(),
        );
    }

    if !response.is_success() {
        bail!(
            "Create release failed: {}",
            describe_curl_response(response.status, &response)
        );
    }

    serde_json::from_str::<CreateReleaseResponse>(&response.body)
        .context("Failed to parse create release response")
}

fn build_release_manifest(artifact: &super::UploadArtifactDescriptor) -> serde_json::Value {
    let mut manifest = json!({
        "source": "presigned_upload",
        "publisher": artifact.publisher,
        "name": artifact.slug,
        "version": artifact.version,
    });

    if let Some(lock_id) = artifact
        .lock_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        manifest["lock_id"] = json!(lock_id);
    }
    if let Some(closure_digest) = artifact
        .closure_digest
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        manifest["closure_digest"] = json!(closure_digest);
    }
    if let Some(metadata) = artifact.publish_metadata.as_ref() {
        manifest["distribution"] = json!({
            "identity_class": metadata.identity_class.as_str(),
            "delivery_mode": metadata.delivery_mode,
            "provenance_limited": metadata.provenance_limited,
        });
    }

    manifest
}

fn normalize_release_version(version: &str) -> Option<String> {
    let trimmed = version.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn build_download_url(registry_url: &str, publisher: &str, slug: &str, version: &str) -> String {
    format!(
        "{}/v1/capsules/by/{}/{}/download?version={}",
        registry_url,
        urlencoding::encode(publisher),
        urlencoding::encode(slug),
        urlencoding::encode(version)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::http::HeaderMap;
    use axum::routing::{get, post, put};
    use axum::{Json, Router};
    use serial_test::serial;
    use tempfile::tempdir;

    #[derive(Debug, Default)]
    struct MockServerData {
        capsule_id: Option<String>,
        uploaded_bytes: Option<Vec<u8>>,
        upload_auth_header: Option<String>,
        finalize_auth_header: Option<String>,
        last_allow_existing: Option<bool>,
        create_release_already_existed: bool,
    }

    #[derive(Clone, Default)]
    struct MockServerState {
        did: String,
        inner: Arc<Mutex<MockServerData>>,
    }

    #[derive(Debug, Deserialize)]
    struct MockCreateCapsuleRequest {
        slug: String,
        name: String,
    }

    #[derive(Debug, Deserialize)]
    struct MockCreateReleaseRequest {
        version: Option<String>,
        allow_existing: bool,
        content_hash: String,
        size_bytes: u64,
        signature: CreateReleaseSignature,
    }

    async fn publisher_me(State(state): State<MockServerState>) -> Json<serde_json::Value> {
        Json(json!({
            "handle": "pub-test",
            "author_did": state.did,
        }))
    }

    async fn lookup_capsule(
        State(state): State<MockServerState>,
    ) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
        let guard = state.inner.lock().expect("lock state");
        let Some(capsule_id) = guard.capsule_id.clone() else {
            return Err(axum::http::StatusCode::NOT_FOUND);
        };
        Ok(Json(json!({ "id": capsule_id })))
    }

    async fn create_capsule(
        State(state): State<MockServerState>,
        Json(body): Json<MockCreateCapsuleRequest>,
    ) -> Json<serde_json::Value> {
        assert_eq!(body.slug, "demo-app");
        assert_eq!(body.name, "demo-app");
        let mut guard = state.inner.lock().expect("lock state");
        let capsule_id = guard
            .capsule_id
            .get_or_insert_with(|| "capsule_ulid_demo".to_string())
            .clone();
        Json(json!({ "id": capsule_id }))
    }

    async fn create_release(
        Path(id): Path<String>,
        headers: HeaderMap,
        State(state): State<MockServerState>,
        Json(body): Json<MockCreateReleaseRequest>,
    ) -> Json<serde_json::Value> {
        assert_eq!(id, "capsule_ulid_demo");
        assert_eq!(body.version.as_deref(), Some("1.0.0"));
        {
            let mut guard = state.inner.lock().expect("lock state");
            guard.last_allow_existing = Some(body.allow_existing);
        }
        assert_eq!(body.content_hash, "blake3:demo");
        assert_eq!(body.size_bytes, 4);
        assert_eq!(body.signature.algorithm, "Ed25519");
        let host = headers
            .get("host")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("127.0.0.1");
        let guard = state.inner.lock().expect("lock state");
        Json(json!({
            "version": "1.0.0",
            "upload_url": format!("http://{}/upload/{}/1.0.0", host, guard.capsule_id.clone().unwrap_or_default()),
            "already_existed": guard.create_release_already_existed,
        }))
    }

    async fn upload_bytes(
        headers: HeaderMap,
        State(state): State<MockServerState>,
        body: axum::body::Bytes,
    ) -> axum::http::StatusCode {
        let mut guard = state.inner.lock().expect("lock state");
        guard.uploaded_bytes = Some(body.to_vec());
        guard.upload_auth_header = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        axum::http::StatusCode::OK
    }

    async fn finalize_release(
        headers: HeaderMap,
        State(state): State<MockServerState>,
    ) -> Json<serde_json::Value> {
        let mut guard = state.inner.lock().expect("lock state");
        guard.finalize_auth_header = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        Json(json!({ "status": "published" }))
    }

    async fn start_mock_server(state: MockServerState) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/v1/publishers/me", get(publisher_me))
            .route("/v1/capsules/by/:publisher/:slug", get(lookup_capsule))
            .route("/v1/capsules", post(create_capsule))
            .route("/v1/capsules/:id/releases", post(create_release))
            .route(
                "/v1/capsules/:id/releases/:version/finalize",
                post(finalize_release),
            )
            .route("/upload/:id/:version", put(upload_bytes))
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

    fn write_test_signing_key(home: &std::path::Path) -> StoredKey {
        let key = StoredKey::generate();
        let key_path = home
            .join(".ato")
            .join("keys")
            .join("publisher-signing-key.json");
        std::fs::create_dir_all(key_path.parent().expect("parent")).expect("mkdir");
        key.write(&key_path).expect("write signing key");
        key
    }

    #[test]
    #[serial]
    fn presigned_strategy_round_trip_uses_unsigned_put_and_finalize() {
        let home = tempdir().expect("tempdir");
        let previous_home = std::env::var("HOME").ok();
        let previous_token = std::env::var("ATO_TOKEN").ok();
        std::env::set_var("HOME", home.path());
        std::env::set_var("ATO_TOKEN", "test-session-token");

        let key = write_test_signing_key(home.path());
        let did = key.did().expect("did");
        let state = MockServerState {
            did,
            inner: Arc::new(Mutex::new(MockServerData::default())),
        };
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (base_url, handle) = runtime.block_on(start_mock_server(state.clone()));
        let strategy = PresignedUploadStrategy;
        let artifact = super::super::UploadArtifactDescriptor {
            publisher: "pub-test".to_string(),
            slug: "demo-app".to_string(),
            version: "1.0.0".to_string(),
            file_name: "demo-app-1.0.0.capsule".to_string(),
            sha256: "sha256:demo".to_string(),
            blake3: "blake3:demo".to_string(),
            size_bytes: 4,
            allow_existing: false,
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
        };

        let session = strategy
            .start_upload(&StartUploadRequest {
                registry_url: base_url.clone(),
                artifact: artifact.clone(),
                force_large_payload: false,
                paid_large_payload: false,
            })
            .expect("start upload");
        let transfer = strategy
            .transfer(TransferArtifactRequest {
                registry_url: base_url.clone(),
                session,
                artifact_bytes: vec![1, 2, 3, 4],
            })
            .expect("transfer");
        let result = strategy
            .finalize_upload(FinalizeUploadRequest {
                registry_url: base_url,
                artifact,
                transfer,
                sync_payload: None,
            })
            .expect("finalize");

        let guard = state.inner.lock().expect("lock state");
        assert_eq!(guard.uploaded_bytes.as_deref(), Some(&[1, 2, 3, 4][..]));
        assert!(guard.upload_auth_header.is_none());
        assert_eq!(
            guard.finalize_auth_header.as_deref(),
            Some("Bearer test-session-token")
        );
        assert_eq!(result.scoped_id, "pub-test/demo-app");
        assert_eq!(result.version, "1.0.0");
        assert_eq!(guard.last_allow_existing, Some(false));

        handle.abort();
        let _ = runtime.block_on(handle);
        drop(runtime);

        if let Some(value) = previous_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(value) = previous_token {
            std::env::set_var("ATO_TOKEN", value);
        } else {
            std::env::remove_var("ATO_TOKEN");
        }
    }

    #[test]
    #[serial]
    fn presigned_strategy_preserves_allow_existing_and_already_existed() {
        let home = tempdir().expect("tempdir");
        let previous_home = std::env::var("HOME").ok();
        let previous_token = std::env::var("ATO_TOKEN").ok();
        std::env::set_var("HOME", home.path());
        std::env::set_var("ATO_TOKEN", "test-session-token");

        let key = write_test_signing_key(home.path());
        let did = key.did().expect("did");
        let state = MockServerState {
            did,
            inner: Arc::new(Mutex::new(MockServerData {
                create_release_already_existed: true,
                ..MockServerData::default()
            })),
        };
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let (base_url, handle) = runtime.block_on(start_mock_server(state.clone()));
        let strategy = PresignedUploadStrategy;
        let artifact = super::super::UploadArtifactDescriptor {
            publisher: "pub-test".to_string(),
            slug: "demo-app".to_string(),
            version: "1.0.0".to_string(),
            file_name: "demo-app-1.0.0.capsule".to_string(),
            sha256: "sha256:demo".to_string(),
            blake3: "blake3:demo".to_string(),
            size_bytes: 4,
            allow_existing: true,
            lock_id: None,
            closure_digest: None,
            publish_metadata: None,
        };

        let session = strategy
            .start_upload(&StartUploadRequest {
                registry_url: base_url.clone(),
                artifact: artifact.clone(),
                force_large_payload: false,
                paid_large_payload: false,
            })
            .expect("start upload");
        let transfer = strategy
            .transfer(TransferArtifactRequest {
                registry_url: base_url.clone(),
                session,
                artifact_bytes: vec![1, 2, 3, 4],
            })
            .expect("transfer");
        let result = strategy
            .finalize_upload(FinalizeUploadRequest {
                registry_url: base_url,
                artifact,
                transfer,
                sync_payload: None,
            })
            .expect("finalize");

        let guard = state.inner.lock().expect("lock state");
        assert_eq!(guard.last_allow_existing, Some(true));
        assert!(result.already_existed);

        handle.abort();
        let _ = runtime.block_on(handle);
        drop(runtime);

        if let Some(value) = previous_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(value) = previous_token {
            std::env::set_var("ATO_TOKEN", value);
        } else {
            std::env::remove_var("ATO_TOKEN");
        }
    }
}
