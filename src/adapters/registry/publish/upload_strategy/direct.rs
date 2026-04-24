use anyhow::{Context, Result};

use super::{
    FinalizeUploadRequest, StartUploadRequest, TransferArtifactRequest, TransferArtifactResponse,
    UploadPreflightRequest, UploadSession, UploadStrategy,
};

pub(crate) struct DirectUploadSession {
    pub(crate) endpoint: String,
    pub(crate) headers: Vec<(String, String)>,
}

pub(crate) struct DirectTransferArtifactResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
}

#[derive(Debug, Default)]
pub(crate) struct DirectUploadStrategy;

impl UploadStrategy for DirectUploadStrategy {
    fn validate_preflight(&self, request: &UploadPreflightRequest) -> Result<()> {
        super::super::artifact::enforce_managed_store_direct_upload_policy(
            &request.registry_url,
            request.artifact_size_bytes,
            request.force_large_payload,
            request.paid_large_payload,
        )
    }

    fn start_upload(&self, request: &StartUploadRequest) -> Result<UploadSession> {
        self.validate_preflight(&UploadPreflightRequest {
            registry_url: request.registry_url.clone(),
            artifact_size_bytes: request.artifact.size_bytes,
            force_large_payload: request.force_large_payload,
            paid_large_payload: request.paid_large_payload,
        })?;

        let endpoint = super::super::artifact::build_upload_endpoint(
            &request.registry_url,
            &request.artifact.publisher,
            &request.artifact.slug,
            &request.artifact.version,
            if request.artifact.file_name.trim().is_empty() {
                None
            } else {
                Some(request.artifact.file_name.as_str())
            },
            request.artifact.allow_existing,
        );

        let headers = super::super::artifact::build_direct_upload_headers(&request.artifact);
        Ok(UploadSession::Direct(DirectUploadSession {
            endpoint,
            headers,
        }))
    }

    fn transfer(&self, request: TransferArtifactRequest) -> Result<TransferArtifactResponse> {
        let UploadSession::Direct(session) = request.session else {
            anyhow::bail!("direct upload strategy requires a direct upload session")
        };

        let mut extra_headers = session.headers.clone();
        if let Some(token) = crate::registry::http::current_ato_token() {
            extra_headers.push(("authorization".to_string(), format!("Bearer {}", token)));
        }

        let response = super::curl_upload::put_bytes(
            &session.endpoint,
            &request.artifact_bytes,
            &extra_headers,
        )
        .with_context(|| format!("Failed to upload artifact to {}", session.endpoint))?;

        Ok(TransferArtifactResponse::Direct(
            DirectTransferArtifactResponse {
                status: response.status,
                body: response.body,
            },
        ))
    }

    fn finalize_upload(
        &self,
        request: FinalizeUploadRequest,
    ) -> Result<super::super::artifact::PublishArtifactResult> {
        let TransferArtifactResponse::Direct(transfer) = request.transfer else {
            anyhow::bail!("direct upload strategy requires a direct transfer response")
        };

        if !(200..300).contains(&transfer.status) {
            let status = reqwest::StatusCode::from_u16(transfer.status)
                .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
            let error = super::super::artifact::classify_upload_failure(status, &transfer.body);
            return Err(error.into());
        }

        let result: super::super::artifact::PublishArtifactResult =
            serde_json::from_str(&transfer.body).with_context(|| {
                let preview: String = transfer.body.chars().take(500).collect();
                format!(
                    "Invalid local registry upload response (status={}): {}",
                    transfer.status, preview
                )
            })?;
        super::super::artifact::sync_v3_chunks_if_present(
            &request.registry_url,
            request.sync_payload.as_ref(),
        )
        .with_context(|| "Failed to finalize payload v3 metadata for uploaded release")?;
        Ok(result)
    }
}
