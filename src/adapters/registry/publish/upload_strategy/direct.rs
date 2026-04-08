use std::error::Error as _;

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
    pub(crate) response: reqwest::blocking::Response,
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

        let mut client_builder =
            crate::registry::http::blocking_client_builder(&request.registry_url);
        if super::super::artifact::is_managed_store_direct_registry(&request.registry_url) {
            client_builder = client_builder.http1_only();
        }
        let client = client_builder
            .build()
            .context("Failed to create registry upload client")?;
        let mut builder = client.put(&session.endpoint);
        for (name, value) in &session.headers {
            builder = builder.header(name, value);
        }
        builder = crate::registry::http::with_blocking_ato_token(builder);

        let response = builder.body(request.artifact_bytes).send().map_err(|err| {
            anyhow::anyhow!(
                "Failed to upload artifact to {}: {}",
                session.endpoint,
                format_reqwest_transport_error(&err),
            )
        })?;

        Ok(TransferArtifactResponse::Direct(
            DirectTransferArtifactResponse { response },
        ))
    }

    fn finalize_upload(
        &self,
        request: FinalizeUploadRequest,
    ) -> Result<super::super::artifact::PublishArtifactResult> {
        let TransferArtifactResponse::Direct(transfer) = request.transfer else {
            anyhow::bail!("direct upload strategy requires a direct transfer response")
        };

        if !transfer.response.status().is_success() {
            let status = transfer.response.status();
            let body = transfer.response.text().unwrap_or_default();
            let error = super::super::artifact::classify_upload_failure(status, &body);
            return Err(error.into());
        }

        let result = transfer
            .response
            .json::<super::super::artifact::PublishArtifactResult>()
            .context("Invalid local registry upload response")?;
        super::super::artifact::sync_v3_chunks_if_present(
            &request.registry_url,
            request.v3_sync_payload.as_ref(),
        )
        .with_context(|| "Failed to finalize payload v3 metadata for uploaded release")?;
        Ok(result)
    }
}

fn format_reqwest_transport_error(err: &reqwest::Error) -> String {
    let mut rendered = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let detail = cause.to_string();
        if !detail.is_empty() && !rendered.contains(&detail) {
            rendered.push_str(": ");
            rendered.push_str(&detail);
        }
        source = cause.source();
    }
    rendered
}
