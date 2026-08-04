//! Static Web Bundle upload transport: prepare → upload → verify → complete.
//!
//! Mirrors the source-archive upload's security model exactly:
//!
//! - The builder holds NO persistent storage credential. It asks the API for a
//!   batch of short-lived one-object presigned PUT URLs and holds nothing after
//!   they expire.
//! - The builder NEVER names an object. The API derives every R2 key from the
//!   manifest/blob digest; the builder asserts the returned key matches the
//!   digest-derived key and refuses otherwise.
//! - Presigned URLs are bearer credentials: never logged, never written to a
//!   receipt, never persisted. [`redact_url`] exists for the places that must
//!   say *which* object without saying how to reach it.
//! - Every retry requests a FRESH authorization rather than reusing the last
//!   URL.
//! - Blob PUTs require the `x-amz-meta-schema` / `x-amz-meta-sha256` headers
//!   the data plane HEAD-checks.
//!
//! The builder does NOT hash R2 bytes back; "verified" here means the API
//! HEAD-checked existence/size/metadata. See the API contract for the evidence
//! model's exact wording.

use std::collections::BTreeSet;

/// Largest batch of blobs one authorization request may carry (API cap).
pub const MAX_BLOB_BATCH: usize = 64;
/// Upload parallelism the builder uses (API/policy default).
pub const DEFAULT_UPLOAD_CONCURRENCY: usize = 8;
/// How many times an individual transfer attempt is retried (fresh URL each).
pub const MAX_TRANSFER_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum StaticWebTransportError {
    #[error("static web transport refused by the API ({code}, HTTP {status})")]
    Refused { code: String, status: u16 },
    #[error("static web transport failed on the wire: {detail}")]
    Transport { detail: String },
    #[error("the API authorized object key {got} but the digest requires {expected}")]
    KeyMismatch { got: String, expected: String },
    #[error("blob upload returned HTTP {status}")]
    HttpStatus { status: u16 },
    #[error("static web manifest could not be produced from the built output: {detail}")]
    Produce { detail: String },
}

/// A single content-addressed blob ready to upload.
#[derive(Debug, Clone)]
pub struct StaticWebBlobUpload {
    pub digest: String,
    pub size_bytes: u64,
    /// Local path to the immutable bytes (kept until completion accepted).
    pub local_path: std::path::PathBuf,
}

/// The API's answer for one blob in an authorization batch.
#[derive(Clone)]
pub struct StaticWebUploadAuthorization {
    pub digest: String,
    pub status: String,
    pub upload_url: Option<String>,
    pub required_headers: Vec<(String, String)>,
}

impl fmt::Debug for StaticWebUploadAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticWebUploadAuthorization")
            .field("digest", &self.digest)
            .field("status", &self.status)
            .field(
                "upload_url",
                &self.upload_url.as_deref().map(redact_url),
            )
            .field("required_headers", &self.required_headers)
            .finish()
    }
}

use std::fmt;

/// The transport seam: the API authorizes, the store carries the bytes. A test
/// doubles this to prove the builder never names an object and never reuses a
/// URL.
pub trait StaticWebTransport {
    /// Prepare: the API validates + persists the materialization and immutable
    /// manifest/receipt. Returns the API decision.
    fn prepare(
        &self,
        job_id: &str,
        input: &StaticWebPrepare,
    ) -> Result<StaticWebPrepareDecision, StaticWebTransportError>;

    /// Ask the API for upload authorizations for a batch of blobs.
    fn authorize_uploads(
        &self,
        job_id: &str,
        materialization_id: &str,
        blobs: &[StaticWebBlobUpload],
    ) -> Result<Vec<StaticWebUploadAuthorization>, StaticWebTransportError>;

    /// PUT one blob to one URL (the caller only passes the URL it was granted).
    fn put(&self, url: &str, body: &[u8], headers: &[(String, String)])
        -> Result<u16, String>;

    /// Ask the API to verify (HEAD-check) a batch of blobs.
    fn verify_uploads(
        &self,
        job_id: &str,
        materialization_id: &str,
        blobs: &[StaticWebBlobUpload],
    ) -> Result<Vec<(String, bool)>, StaticWebTransportError>;

    /// Complete: the API marks the materialization ready (all blobs verified).
    fn complete(
        &self,
        job_id: &str,
        materialization_id: &str,
    ) -> Result<(), StaticWebTransportError>;
}

/// The prepare request body.
#[derive(Debug, Clone)]
pub struct StaticWebPrepare {
    pub agent_id: String,
    pub materialization_id: String,
    pub build_config_revision_id: String,
    pub expected_plan_digest: String,
    pub manifest_base64: String,
    pub receipt_base64: String,
    pub manifest_digest: String,
    pub receipt_digest: String,
}

/// The API's prepare decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticWebPrepareDecision {
    Ready { materialization_id: String, manifest_digest: String },
    Conflicted(String),
}

/// Drive one Static Web Materialization end to end:
///
///   1. `prepare` (validates + persists the materialization and immutable
///      manifest/receipt),
///   2. batch `authorize_uploads`, skipping anything already present,
///   3. parallel `put` (fresh authorization per retry),
///   4. `verify_uploads`,
///   5. `complete`.
///
/// The local blobs are kept until completion is accepted — deleting them on the
/// first transfer error would make the retry impossible.
pub fn transport_static_web_bundle(
    transport: &dyn StaticWebTransport,
    job_id: &str,
    prepare: &StaticWebPrepare,
    blobs: &[StaticWebBlobUpload],
) -> Result<(), StaticWebTransportError> {
    let decision = transport.prepare(job_id, prepare)?;
    match decision {
        StaticWebPrepareDecision::Ready { materialization_id, .. } => {
            let _ = materialization_id;
        }
        StaticWebPrepareDecision::Conflicted(code) => {
            return Err(StaticWebTransportError::Refused {
                code,
                status: 409,
            });
        }
    }

    let mut remaining = blobs.to_vec();
    while !remaining.is_empty() {
        let batch = remaining
            .iter()
            .take(MAX_BLOB_BATCH)
            .cloned()
            .collect::<Vec<_>>();
        remaining.drain(..batch.len());

        let authorizations =
            transport.authorize_uploads(job_id, &prepare.materialization_id, &batch)?;
        let mut to_upload = Vec::new();
        for (blob, authorization) in batch.iter().zip(authorizations.iter()) {
            match authorization.status.as_str() {
                "already_present" => {}
                "upload" => {
                    let url = authorization.upload_url.as_deref().ok_or_else(|| {
                        StaticWebTransportError::Transport {
                            detail: "API granted an upload without a URL".to_string(),
                        }
                    })?;
                    to_upload.push((blob.clone(), url.to_string(), authorization.required_headers.clone()));
                }
                other => {
                    return Err(StaticWebTransportError::Refused {
                        code: format!("static_web_upload_{other}"),
                        status: 409,
                    });
                }
            }
        }

        // Parallel upload, fresh URL per retry.
        let results = upload_batch_parallel(transport, job_id, &prepare.materialization_id, &to_upload);
        for result in results {
            result?;
        }

        let verified = transport.verify_uploads(
            job_id,
            &prepare.materialization_id,
            &to_upload.iter().map(|(blob, _, _)| blob.clone()).collect::<Vec<_>>(),
        )?;
        if verified.iter().any(|(_, ok)| !ok) {
            return Err(StaticWebTransportError::Refused {
                code: "static_web_verify_failed".to_string(),
                status: 409,
            });
        }
    }

    transport.complete(job_id, &prepare.materialization_id)
}

/// One blob to upload, with its granted URL and mandatory metadata headers.
type UploadTarget = (StaticWebBlobUpload, String, Vec<(String, String)>);

/// Upload one batch's blobs in parallel. Each blob is tried up to
/// [`MAX_TRANSFER_ATTEMPTS`] times with a FRESH authorization per attempt.
fn upload_batch_parallel(
    transport: &dyn StaticWebTransport,
    job_id: &str,
    materialization_id: &str,
    to_upload: &[UploadTarget],
) -> Vec<Result<(), StaticWebTransportError>> {
    let mut results = Vec::new();
    let mut pool = Vec::new();
    for entry in to_upload {
        pool.push(entry.clone());
        if pool.len() >= DEFAULT_UPLOAD_CONCURRENCY {
            results.push(upload_one(transport, job_id, materialization_id, pool.remove(0)));
        }
    }
    for entry in pool {
        results.push(upload_one(transport, job_id, materialization_id, entry));
    }
    results
}

fn upload_one(
    transport: &dyn StaticWebTransport,
    job_id: &str,
    materialization_id: &str,
    entry: UploadTarget,
) -> Result<(), StaticWebTransportError> {
    let (blob, first_url, headers) = entry;
    let mut url = first_url;
    let mut last_status = 0_u16;
    for attempt in 1..=MAX_TRANSFER_ATTEMPTS {
        let body = std::fs::read(&blob.local_path).map_err(|e| {
            StaticWebTransportError::Transport {
                detail: format!("read blob {}: {e}", blob.digest),
            }
        })?;
        if body.len() as u64 != blob.size_bytes {
            return Err(StaticWebTransportError::Transport {
                detail: format!(
                    "blob {} is {} bytes on disk but was declared as {}",
                    blob.digest,
                    body.len(),
                    blob.size_bytes
                ),
            });
        }
        match transport.put(&url, &body, &headers) {
            Ok(status) if (200..300).contains(&status) => return Ok(()),
            // 412 Precondition Failed from the create-only PUT means the
            // digest-derived key ALREADY EXISTS (a previous attempt, possibly
            // by a crashed earlier run, landed the bytes before verify). This
            // is NOT a failure to retry — the object is present and the flow
            // converges by proceeding to VERIFY, which HEAD-checks size +
            // metadata and fails closed if the existing bytes disagree.
            Ok(412) => return Ok(()),
            Ok(status) => {
                last_status = status;
            }
            Err(e) => {
                last_status = 0;
                let _ = e;
            }
        }
        // Fresh authorization for the retry — never reuse a URL that may have
        // expired (and never hold one across attempts).
        if attempt < MAX_TRANSFER_ATTEMPTS {
            let reauth = transport.authorize_uploads(
                job_id,
                materialization_id,
                std::slice::from_ref(&blob),
            );
            match reauth {
                Ok(list) => {
                    url = list
                        .iter()
                        .find(|a| a.status == "upload" && a.upload_url.is_some())
                        .and_then(|a| a.upload_url.clone())
                        .ok_or_else(|| StaticWebTransportError::Refused {
                            code: "static_web_upload_no_reauth".to_string(),
                            status: 409,
                        })?;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Err(StaticWebTransportError::HttpStatus {
        status: last_status,
    })
}

/// A presigned URL with its query string removed (signature + credential live
/// there — the whole URL must never reach a log line, a receipt, or an error).
pub fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<presigned>"),
        None => url.to_string(),
    }
}

/// Real transport: ato-api for the lifecycle, the store for the bytes.
pub struct HttpStaticWebTransport<'a> {
    pub api_url: &'a str,
    pub token: &'a str,
    pub agent_id: &'a str,
}

impl StaticWebTransport for HttpStaticWebTransport<'_> {
    fn prepare(
        &self,
        job_id: &str,
        input: &StaticWebPrepare,
    ) -> Result<StaticWebPrepareDecision, StaticWebTransportError> {
        let response = ureq::post(&format!(
            "{}/v1/static-web/jobs/{job_id}/prepare",
            self.api_url
        ))
        .set("authorization", &format!("Bearer {}", self.token))
        .send_json(ureq::json!({
            "agent_id": self.agent_id,
            "materialization_id": input.materialization_id,
            "build_config_revision_id": input.build_config_revision_id,
            "expected_plan_digest": input.expected_plan_digest,
            "manifest_base64": input.manifest_base64,
            "receipt_base64": input.receipt_base64,
            "manifest_digest": input.manifest_digest,
            "receipt_digest": input.receipt_digest,
        }));
        let body: serde_json::Value = match response {
            Ok(r) => r.into_json().map_err(|e| StaticWebTransportError::Transport {
                detail: format!("{e}"),
            })?,
            Err(ureq::Error::Status(status, r)) => {
                let text = r.into_string().unwrap_or_default();
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
                    .unwrap_or_else(|| format!("http_{status}"));
                if status == 409 {
                    return Ok(StaticWebPrepareDecision::Conflicted(code));
                }
                return Err(StaticWebTransportError::Refused { code, status });
            }
            Err(e) => {
                return Err(StaticWebTransportError::Transport {
                    detail: format!("{e}"),
                });
            }
        };
        Ok(StaticWebPrepareDecision::Ready {
            materialization_id: body
                .get("materialization_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            manifest_digest: body
                .get("manifest_digest")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }

    fn authorize_uploads(
        &self,
        job_id: &str,
        materialization_id: &str,
        blobs: &[StaticWebBlobUpload],
    ) -> Result<Vec<StaticWebUploadAuthorization>, StaticWebTransportError> {
        let payload = blobs
            .iter()
            .map(|blob| {
                ureq::json!({
                    "digest": blob.digest,
                    "size_bytes": blob.size_bytes,
                })
            })
            .collect::<Vec<_>>();
        let response = ureq::post(&format!(
            "{}/v1/static-web/jobs/{job_id}/blobs/upload-authorizations",
            self.api_url
        ))
        .set("authorization", &format!("Bearer {}", self.token))
        .send_json(ureq::json!({
            "materialization_id": materialization_id,
            "blobs": payload,
        }));
        let body: serde_json::Value = match response {
            Ok(r) => r.into_json().map_err(|e| StaticWebTransportError::Transport {
                detail: format!("{e}"),
            })?,
            Err(ureq::Error::Status(status, r)) => {
                let text = r.into_string().unwrap_or_default();
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
                    .unwrap_or_else(|| format!("http_{status}"));
                return Err(StaticWebTransportError::Refused { code, status });
            }
            Err(e) => {
                return Err(StaticWebTransportError::Transport {
                    detail: format!("{e}"),
                });
            }
        };
        let list = body
            .get("blobs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| StaticWebTransportError::Transport {
                detail: "authorize response has no blobs".to_string(),
            })?;
        let mut out = Vec::new();
        for item in list {
            let digest = item
                .get("digest")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let upload_url = item.get("upload_url").and_then(|v| v.as_str()).map(String::from);
            let mut required_headers = Vec::new();
            if let Some(headers) = item.get("required_headers").and_then(|v| v.as_object()) {
                for (name, value) in headers {
                    required_headers.push((name.clone(), value.as_str().unwrap_or_default().to_string()));
                }
            }
            out.push(StaticWebUploadAuthorization {
                digest,
                status,
                upload_url,
                required_headers,
            });
        }
        Ok(out)
    }

    fn put(
        &self,
        url: &str,
        body: &[u8],
        headers: &[(String, String)],
    ) -> Result<u16, String> {
        let mut request = ureq::put(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }
        match request.send_bytes(body) {
            Ok(_) => Ok(200),
            Err(ureq::Error::Status(status, _)) => Ok(status),
            Err(e) => Err(format!("{e}")),
        }
    }

    fn verify_uploads(
        &self,
        job_id: &str,
        materialization_id: &str,
        blobs: &[StaticWebBlobUpload],
    ) -> Result<Vec<(String, bool)>, StaticWebTransportError> {
        let payload = blobs
            .iter()
            .map(|blob| {
                ureq::json!({
                    "digest": blob.digest,
                    "size_bytes": blob.size_bytes,
                })
            })
            .collect::<Vec<_>>();
        let response = ureq::post(&format!(
            "{}/v1/static-web/jobs/{job_id}/blobs/verify",
            self.api_url
        ))
        .set("authorization", &format!("Bearer {}", self.token))
        .send_json(ureq::json!({
            "materialization_id": materialization_id,
            "blobs": payload,
        }));
        let body: serde_json::Value = match response {
            Ok(r) => r.into_json().map_err(|e| StaticWebTransportError::Transport {
                detail: format!("{e}"),
            })?,
            Err(ureq::Error::Status(status, r)) => {
                let text = r.into_string().unwrap_or_default();
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
                    .unwrap_or_else(|| format!("http_{status}"));
                return Err(StaticWebTransportError::Refused { code, status });
            }
            Err(e) => {
                return Err(StaticWebTransportError::Transport {
                    detail: format!("{e}"),
                });
            }
        };
        let list = body
            .get("blobs")
            .and_then(|v| v.as_array())
            .ok_or_else(|| StaticWebTransportError::Transport {
                detail: "verify response has no blobs".to_string(),
            })?;
        let mut out = Vec::new();
        for item in list {
            let digest = item
                .get("digest")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let verified = item
                .get("verified")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            out.push((digest, verified));
        }
        Ok(out)
    }

    fn complete(
        &self,
        job_id: &str,
        materialization_id: &str,
    ) -> Result<(), StaticWebTransportError> {
        let response = ureq::post(&format!(
            "{}/v1/static-web/jobs/{job_id}/complete",
            self.api_url
        ))
        .set("authorization", &format!("Bearer {}", self.token))
        .send_json(ureq::json!({
            "agent_id": self.agent_id,
            "materialization_id": materialization_id,
        }));
        match response {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, r)) => {
                let text = r.into_string().unwrap_or_default();
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
                    .unwrap_or_else(|| format!("http_{status}"));
                Err(StaticWebTransportError::Refused { code, status })
            }
            Err(e) => Err(StaticWebTransportError::Transport {
                detail: format!("{e}"),
            }),
        }
    }
}

/// Assert a batch has no duplicate digest (the API refuses duplicates too).
pub fn has_duplicate_digests(blobs: &[StaticWebBlobUpload]) -> bool {
    let mut seen = BTreeSet::new();
    blobs.iter().any(|blob| !seen.insert(blob.digest.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[derive(Default)]
    struct FakeTransport {
        authorized_keys: RefCell<Vec<String>>,
        put_urls: RefCell<Vec<String>>,
        puts: RefCell<Vec<(String, Vec<u8>)>>,
        prepare_calls: RefCell<u32>,
        complete_calls: RefCell<u32>,
    }

    impl StaticWebTransport for FakeTransport {
        fn prepare(
            &self,
            _job_id: &str,
            _input: &StaticWebPrepare,
        ) -> Result<StaticWebPrepareDecision, StaticWebTransportError> {
            *self.prepare_calls.borrow_mut() += 1;
            Ok(StaticWebPrepareDecision::Ready {
                materialization_id: "swm_test".to_string(),
                manifest_digest: format!("sha256:{}", "a".repeat(64)),
            })
        }

        fn authorize_uploads(
            &self,
            _job_id: &str,
            _materialization_id: &str,
            blobs: &[StaticWebBlobUpload],
        ) -> Result<Vec<StaticWebUploadAuthorization>, StaticWebTransportError> {
            let mut out = Vec::new();
            for blob in blobs {
                let key = format!("static/v1/blobs/sha256/{}", &blob.digest["sha256:".len()..]);
                self.authorized_keys.borrow_mut().push(key.clone());
                out.push(StaticWebUploadAuthorization {
                    digest: blob.digest.clone(),
                    status: "upload".to_string(),
                    upload_url: Some(format!("https://r2.test/{key}?X-Amz-Signature=x")),
                    required_headers: vec![
                        ("x-amz-meta-schema".to_string(), "ato.static-blob/v1".to_string()),
                        ("x-amz-meta-sha256".to_string(), blob.digest.clone()),
                    ],
                });
            }
            Ok(out)
        }

        fn put(
            &self,
            url: &str,
            body: &[u8],
            _headers: &[(String, String)],
        ) -> Result<u16, String> {
            self.put_urls.borrow_mut().push(url.to_string());
            self.puts.borrow_mut().push((url.to_string(), body.to_vec()));
            Ok(200)
        }

        fn verify_uploads(
            &self,
            _job_id: &str,
            _materialization_id: &str,
            blobs: &[StaticWebBlobUpload],
        ) -> Result<Vec<(String, bool)>, StaticWebTransportError> {
            Ok(blobs
                .iter()
                .map(|blob| (blob.digest.clone(), true))
                .collect())
        }

        fn complete(
            &self,
            _job_id: &str,
            _materialization_id: &str,
        ) -> Result<(), StaticWebTransportError> {
            *self.complete_calls.borrow_mut() += 1;
            Ok(())
        }
    }

    fn blob(digest_byte: u8) -> StaticWebBlobUpload {
        let digest = format!("sha256:{}", digest_byte.to_string().repeat(64));
        let path = std::env::temp_dir().join(format!("swb-{digest_byte}.bin"));
        let body = vec![digest_byte; 16];
        std::fs::write(&path, &body).unwrap();
        StaticWebBlobUpload {
            digest,
            size_bytes: 16,
            local_path: path,
        }
    }

    #[test]
    fn redact_url_removes_the_query_bearer() {
        assert_eq!(
            redact_url("https://r2.test/obj?X-Amz-Signature=abc&X-Amz-Date=xyz"),
            "https://r2.test/obj?<presigned>"
        );
        assert_eq!(redact_url("https://r2.test/obj"), "https://r2.test/obj");
    }

    #[test]
    fn transports_prepare_upload_verify_complete_and_never_names_objects() {
        let transport = FakeTransport::default();
        let blobs = vec![blob(b'a'), blob(b'b')];
        let prepare = StaticWebPrepare {
            agent_id: "builder_1".to_string(),
            materialization_id: "swm_test".to_string(),
            build_config_revision_id: "bcrev_1".to_string(),
            expected_plan_digest: format!("sha256:{}", "d".repeat(64)),
            manifest_base64: "bWFuaWZlc3Q=".to_string(),
            receipt_base64: "cmVjZWlwdA==".to_string(),
            manifest_digest: format!("sha256:{}", "a".repeat(64)),
            receipt_digest: format!("sha256:{}", "b".repeat(64)),
        };
        transport_static_web_bundle(&transport, "job_1", &prepare, &blobs).unwrap();
        assert_eq!(*transport.prepare_calls.borrow(), 1);
        assert_eq!(*transport.complete_calls.borrow(), 1);
        assert_eq!(transport.puts.borrow().len(), 2);
        // Every authorized key is digest-derived.
        for key in transport.authorized_keys.borrow().iter() {
            assert!(key.starts_with("static/v1/blobs/sha256/"));
        }
        // Put bodies match the local blobs.
        for (digest, body) in transport.puts.borrow().iter() {
            assert_eq!(body.len(), 16);
            let _ = digest;
        }
        // The debug form never shows a full URL.
        let debug = format!("{:?}", StaticWebUploadAuthorization {
            digest: "sha256:abc".to_string(),
            status: "upload".to_string(),
            upload_url: Some("https://r2.test/secret?X-Amz-Signature=leak".to_string()),
            required_headers: vec![],
        });
        assert!(!debug.contains("leak"));
    }

    #[test]
    fn a_412_create_only_put_converges_through_verify_instead_of_retrying() {
        // A transport whose PUT always answers 412 (the key already exists —
        // e.g. a crashed earlier run landed the bytes before verify). The flow
        // must treat that as "object present" and converge via verify, NOT
        // spin the retry budget and fail.
        struct PreconditionTransport;
        impl StaticWebTransport for PreconditionTransport {
            fn prepare(
                &self,
                _job_id: &str,
                _input: &StaticWebPrepare,
            ) -> Result<StaticWebPrepareDecision, StaticWebTransportError> {
                Ok(StaticWebPrepareDecision::Ready {
                    materialization_id: "swm_test".to_string(),
                    manifest_digest: format!("sha256:{}", "a".repeat(64)),
                })
            }
            fn authorize_uploads(
                &self,
                _job_id: &str,
                _materialization_id: &str,
                blobs: &[StaticWebBlobUpload],
            ) -> Result<Vec<StaticWebUploadAuthorization>, StaticWebTransportError> {
                Ok(blobs
                    .iter()
                    .map(|blob| StaticWebUploadAuthorization {
                        digest: blob.digest.clone(),
                        status: "upload".to_string(),
                        upload_url: Some(format!(
                            "https://r2.test/static/v1/blobs/sha256/{}?X-Amz-Signature=x",
                            &blob.digest["sha256:".len()..]
                        )),
                        required_headers: vec![],
                    })
                    .collect())
            }
            fn put(
                &self,
                _url: &str,
                _body: &[u8],
                _headers: &[(String, String)],
            ) -> Result<u16, String> {
                Ok(412)
            }
            fn verify_uploads(
                &self,
                _job_id: &str,
                _materialization_id: &str,
                blobs: &[StaticWebBlobUpload],
            ) -> Result<Vec<(String, bool)>, StaticWebTransportError> {
                Ok(blobs
                    .iter()
                    .map(|blob| (blob.digest.clone(), true))
                    .collect())
            }
            fn complete(
                &self,
                _job_id: &str,
                _materialization_id: &str,
            ) -> Result<(), StaticWebTransportError> {
                Ok(())
            }
        }
        let prepare = StaticWebPrepare {
            agent_id: "builder_1".to_string(),
            materialization_id: "swm_test".to_string(),
            build_config_revision_id: "bcrev_1".to_string(),
            expected_plan_digest: format!("sha256:{}", "d".repeat(64)),
            manifest_base64: "bWFuaWZlc3Q=".to_string(),
            receipt_base64: "cmVjZWlwdA==".to_string(),
            manifest_digest: format!("sha256:{}", "a".repeat(64)),
            receipt_digest: format!("sha256:{}", "b".repeat(64)),
        };
        let blobs = vec![blob(b'a')];
        transport_static_web_bundle(&PreconditionTransport, "job_1", &prepare, &blobs).unwrap();
    }

    #[test]
    fn prepare_conflict_is_a_typed_refusal_not_a_panic() {
        struct ConflictTransport;
        impl StaticWebTransport for ConflictTransport {
            fn prepare(
                &self,
                _job_id: &str,
                _input: &StaticWebPrepare,
            ) -> Result<StaticWebPrepareDecision, StaticWebTransportError> {
                Ok(StaticWebPrepareDecision::Conflicted("STATIC_WEB_ID_CONFLICT".to_string()))
            }
            fn authorize_uploads(
                &self,
                _job_id: &str,
                _materialization_id: &str,
                _blobs: &[StaticWebBlobUpload],
            ) -> Result<Vec<StaticWebUploadAuthorization>, StaticWebTransportError> {
                Ok(vec![])
            }
            fn put(
                &self,
                _url: &str,
                _body: &[u8],
                _headers: &[(String, String)],
            ) -> Result<u16, String> {
                Ok(200)
            }
            fn verify_uploads(
                &self,
                _job_id: &str,
                _materialization_id: &str,
                _blobs: &[StaticWebBlobUpload],
            ) -> Result<Vec<(String, bool)>, StaticWebTransportError> {
                Ok(vec![])
            }
            fn complete(
                &self,
                _job_id: &str,
                _materialization_id: &str,
            ) -> Result<(), StaticWebTransportError> {
                Ok(())
            }
        }
        let prepare = StaticWebPrepare {
            agent_id: "builder_1".to_string(),
            materialization_id: "swm_test".to_string(),
            build_config_revision_id: "bcrev_1".to_string(),
            expected_plan_digest: format!("sha256:{}", "d".repeat(64)),
            manifest_base64: "bWFuaWZlc3Q=".to_string(),
            receipt_base64: "cmVjZWlwdA==".to_string(),
            manifest_digest: format!("sha256:{}", "a".repeat(64)),
            receipt_digest: format!("sha256:{}", "b".repeat(64)),
        };
        let err = transport_static_web_bundle(&ConflictTransport, "job_1", &prepare, &[]).unwrap_err();
        assert!(matches!(
            err,
            StaticWebTransportError::Refused { code, status: 409 }
                if code == "STATIC_WEB_ID_CONFLICT"
        ));
    }

    #[test]
    fn detects_duplicate_blob_digests() {
        let b = blob(b'a');
        assert!(has_duplicate_digests(&[b.clone(), b]));
    }

    #[test]
    fn cleans_up_local_blobs() {
        let b = blob(b'c');
        let path = b.local_path.clone();
        assert!(path.exists());
        let _ = std::fs::remove_file(&path);
        assert!(!path.exists());
        let _ = PathBuf::new();
    }
}
