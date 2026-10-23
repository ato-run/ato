use super::{Binding, ClaimedSourceArchive, SourceArchiveOutput};
use crate::job::source_fetch;
use anyhow::{Result, ensure};
use ato_formation::source::{FileVerifiedArchive, SourceLimits};
use reqwest::{
    Method, Url,
    blocking::{Client, RequestBuilder},
};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_CONTROL_BODY: u64 = 64 * 1024;

/// API authorization stays on the existing Runner/Formation principal.
/// Signed object URLs are used without that bearer credential.
pub struct SourceArchiveApi {
    client: Client,
    base: String,
    token: String,
    local: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadTicket {
    upload_id: String,
    binding_digest: String,
    upload_url: String,
    download_url: String,
    expires_at_ms: u64,
}
impl std::fmt::Debug for UploadTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadTicket")
            .field("upload_id", &self.upload_id)
            .field("binding_digest", &self.binding_digest)
            .field("signed_urls", &"[redacted]")
            .finish()
    }
}

#[derive(Serialize)]
struct TicketMutation<'a> {
    #[serde(flatten)]
    binding: &'a Binding,
    upload_id: &'a str,
    binding_digest: String,
}
impl SourceArchiveApi {
    pub fn new(base: String, token: String) -> Result<Self> {
        let parsed =
            Url::parse(&base).map_err(|_| anyhow::anyhow!("source_archive_api_origin_invalid"))?;
        let local = loopback(&parsed);
        ensure!(
            parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none()
                && (parsed.scheme() == "https" || (local && parsed.scheme() == "http")),
            "source_archive_api_origin_invalid"
        );
        Ok(Self {
            client: source_fetch::client()?,
            base: base.trim_end_matches('/').into(),
            token,
            local,
        })
    }
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(
                method,
                format!("{}/v1/internal/formation/{path}", self.base),
            )
            .bearer_auth(&self.token)
    }
    fn control<T: serde::de::DeserializeOwned>(
        &self,
        request: RequestBuilder,
        deadline: Instant,
    ) -> Result<T> {
        let response = request
            .timeout(source_fetch::remaining(deadline)?)
            .send()
            .map_err(|_| anyhow::anyhow!("source_archive_control_request_failed"))?;
        ensure!(
            response.status().is_success(),
            "source_archive_control_rejected_http_{}",
            response.status().as_u16()
        );
        let mut bytes = Vec::new();
        response
            .take(MAX_CONTROL_BODY + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow::anyhow!("source_archive_control_response_failed"))?;
        ensure!(
            bytes.len() as u64 <= MAX_CONTROL_BODY,
            "source_archive_control_response_too_large"
        );
        source_fetch::remaining(deadline)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("source_archive_control_response_invalid"))
    }
    pub fn claim_next(&self, worker_id: &str) -> Result<Option<ClaimedSourceArchive>> {
        ensure!(
            !worker_id.is_empty() && worker_id.len() <= 160,
            "source_archive_worker_id_invalid"
        );
        let value: serde_json::Value = self.control(
            self.request(Method::POST, "source-archives/claim")
                .query(&[("worker_id", worker_id)])
                .json(&serde_json::json!({})),
            Instant::now() + Duration::from_secs(30),
        )?;
        if value["job_id"].is_null() {
            return Ok(None);
        }
        let claim: ClaimedSourceArchive = serde_json::from_value(value)
            .map_err(|_| anyhow::anyhow!("source_archive_claim_invalid"))?;
        claim.validate()?;
        Ok(Some(claim))
    }
    pub(super) fn authorize(&self, binding: &Binding, deadline: Instant) -> Result<UploadTicket> {
        let ticket: UploadTicket = self.control(
            self.request(
                Method::POST,
                &format!("attempts/{}/source-archive/uploads", binding.attempt_id),
            )
            .json(binding),
            deadline,
        )?;
        ensure!(
            !ticket.upload_id.is_empty()
                && ticket.upload_id.len() <= 160
                && ticket
                    .upload_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "source_archive_upload_id_invalid"
        );
        let validation = (|| -> Result<()> {
            ensure!(
                ticket.binding_digest == binding.digest()?,
                "source_archive_upload_binding_mismatch"
            );
            self.object_url(&ticket.upload_url)?;
            self.object_url(&ticket.download_url)?;
            ticket.deadline(deadline)?;
            Ok(())
        })();
        if let Err(error) = validation {
            self.abort(&ticket, binding, deadline)
                .map_err(|_| anyhow::anyhow!("source_only_storage_cleanup_unconfirmed"))?;
            return Err(error);
        }
        Ok(ticket)
    }
    fn object_url(&self, raw: &str) -> Result<Url> {
        let url =
            Url::parse(raw).map_err(|_| anyhow::anyhow!("source_archive_object_url_invalid"))?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
                && (url.scheme() == "https"
                    || (self.local && loopback(&url) && url.scheme() == "http")),
            "source_archive_object_url_invalid"
        );
        Ok(url)
    }
    pub(super) fn transfer_and_verify(
        &self,
        ticket: &UploadTicket,
        binding: &Binding,
        verified: &FileVerifiedArchive,
        work_root: &Path,
        deadline: Instant,
    ) -> Result<()> {
        let deadline = ticket.deadline(deadline)?;
        let descriptor = &binding.archive_descriptor;
        let body = reqwest::blocking::Body::sized(
            DeadlineReader {
                inner: verified.archive_reader()?,
                deadline,
            },
            descriptor.size_bytes,
        );
        let uploaded = self
            .client
            .put(self.object_url(&ticket.upload_url)?)
            .timeout(source_fetch::remaining(deadline)?)
            .header("content-type", "application/octet-stream")
            .body(body)
            .send()
            .map_err(|_| anyhow::anyhow!("source_archive_upload_failed"))?;
        ensure!(
            [200, 201, 204].contains(&uploaded.status().as_u16()),
            "source_archive_upload_rejected_http_{}",
            uploaded.status().as_u16()
        );
        drop(uploaded);
        let (file, _) = source_fetch::download_file(
            self.client.get(self.object_url(&ticket.download_url)?),
            work_root,
            descriptor.size_bytes,
            deadline,
        )?;
        let stored = FileVerifiedArchive::verify_with_guard(
            file,
            &descriptor.source_archive_digest,
            descriptor.size_bytes,
            SourceLimits::default(),
            &|| Instant::now() < deadline,
        )?;
        ensure!(
            stored.tree_digest() == descriptor.source_tree_digest
                && stored.resolver_contract() == descriptor.resolver_contract_version
                && stored.archive_format() == descriptor.archive_format_version
                && stored.expanded_file_count() as u64 == descriptor.observed_file_count
                && stored.expanded_bytes() == descriptor.observed_uncompressed_bytes
                && stored.closure_ref(&descriptor.subdirectory)?.as_str()
                    == descriptor.source_closure_ref,
            "source_archive_readback_measurement_mismatch"
        );
        Ok(())
    }
    fn mutation<'a>(
        &self,
        ticket: &'a UploadTicket,
        binding: &'a Binding,
    ) -> Result<TicketMutation<'a>> {
        Ok(TicketMutation {
            binding,
            upload_id: &ticket.upload_id,
            binding_digest: binding.digest()?,
        })
    }
    pub(super) fn abort(
        &self,
        ticket: &UploadTicket,
        binding: &Binding,
        _deadline: Instant,
    ) -> Result<()> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Cleanup {
            cleanup_confirmed: bool,
        }
        // Cleanup has its own bounded time allowance. An expired transfer URL
        // cannot prevent authenticated cleanup of this exact owned staging key.
        let cleanup: Cleanup = self.control(
            self.request(
                Method::DELETE,
                &format!(
                    "attempts/{}/source-archive/uploads/{}",
                    binding.attempt_id, ticket.upload_id
                ),
            )
            .json(&self.mutation(ticket, binding)?),
            Instant::now() + Duration::from_secs(10),
        )?;
        ensure!(
            cleanup.cleanup_confirmed,
            "source_archive_owned_cleanup_unconfirmed"
        );
        Ok(())
    }
    pub(super) fn complete(
        &self,
        ticket: &UploadTicket,
        binding: &Binding,
        deadline: Instant,
    ) -> Result<SourceArchiveOutput> {
        self.control(
            self.request(
                Method::POST,
                &format!("attempts/{}/source-archive", binding.attempt_id),
            )
            .json(&self.mutation(ticket, binding)?),
            deadline,
        )
    }
    pub fn report_failure(&self, claim: &ClaimedSourceArchive) -> Result<()> {
        let _: serde_json::Value = self.control(self.request(Method::POST, &format!("attempts/{}/failure", claim.attempt_id))
            .json(&serde_json::json!({"reason":"Source archive materialization failed.","code":"source_archive_failed","stage":"source"})), Instant::now() + Duration::from_secs(10))?;
        Ok(())
    }
}
impl UploadTicket {
    fn deadline(&self, maximum: Instant) -> Result<Instant> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        ensure!(
            u128::from(self.expires_at_ms) > now,
            "source_archive_upload_expired"
        );
        let left = (u128::from(self.expires_at_ms) - now).min(15 * 60 * 1000) as u64;
        Ok(maximum.min(Instant::now() + Duration::from_millis(left)))
    }
}
fn loopback(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    )
}
struct DeadlineReader<R> {
    inner: R,
    deadline: Instant,
}
impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "source upload deadline",
            ));
        }
        self.inner.read(bytes)
    }
}
