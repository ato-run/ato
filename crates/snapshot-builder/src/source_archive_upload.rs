//! Getting the frozen source archive off this builder's disk.
//!
//! Until this existed, `process_source_materialize_job` froze an archive into
//! the job directory and reported the local path. That path is meaningless to
//! anyone else: the moment the process exits or another builder claims the
//! follow-up build, the archive is gone. A submission cannot reach a third party
//! that way.
//!
//! # The builder holds no storage credential
//!
//! It asks the API to authorize one upload and receives a URL that permits one
//! method, on one object, for a few minutes. It cannot choose the object — the
//! API derives the key from the archive digest — and it holds nothing once the
//! URL expires.
//!
//! # The URL is a secret
//!
//! A presigned URL carries its own signature in the query string, so it is a
//! bearer credential. It is never logged, never written to a receipt, and never
//! persisted. [`redact_url`] exists for the places that want to say *which*
//! object without saying how to reach it.
//!
//! # Retrying
//!
//! An expired URL is not an error to work around; it is a signal to ask again.
//! Every retry requests a FRESH authorization rather than reusing the last URL,
//! because the alternative — holding a URL and hoping — is how a short TTL turns
//! into a long one. The object key is unchanged across retries, because it is
//! derived from the bytes and the bytes have not changed.
//!
//! The local archive is kept until the report is accepted or the job fails
//! terminally. Deleting it on the first upload error would make the retry
//! impossible.

use std::fmt;
use std::path::{Path, PathBuf};

/// How many times to attempt the transfer before giving up.
///
/// Bounded, because an unbounded retry against a failing store is how one job
/// consumes a builder indefinitely — the same shape as the unbounded capture
/// retry tracked in ato#1160.
pub const MAX_UPLOAD_ATTEMPTS: u32 = 3;

/// What the API said about an authorized upload.
///
/// The URL is deliberately not `Debug`-derived on the struct that holds it —
/// see the manual impl below, which redacts it.
#[derive(Clone)]
pub struct UploadAuthorization {
    pub url: String,
    pub object_key: String,
    pub expires_in_seconds: u64,
}

impl fmt::Debug for UploadAuthorization {
    /// Redacted, because `{:?}` in a log line is exactly how a bearer credential
    /// escapes. The object key is safe and is the part anyone debugging wants.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadAuthorization")
            .field("url", &redact_url(&self.url))
            .field("object_key", &self.object_key)
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish()
    }
}

/// Why the archive did not reach the store.
#[derive(Debug)]
pub enum UploadFailure {
    /// The API refused to authorize. Carries its code, never a URL.
    AuthorizationRefused { code: String, detail: String },
    /// Every attempt failed. Carries the last status, never a URL.
    TransferFailed { attempts: u32, detail: String },
    /// The object is not there, or not the size we sent, after a successful PUT.
    NotStored { detail: String },
    /// The archive could not be read from local disk.
    ArchiveUnreadable { detail: String },
}

impl UploadFailure {
    /// A stable machine code for the failure ack.
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthorizationRefused { .. } => "upload_authorization_refused",
            Self::TransferFailed { .. } => "upload_transfer_failed",
            Self::NotStored { .. } => "upload_not_stored",
            Self::ArchiveUnreadable { .. } => "archive_unreadable",
        }
    }
}

impl fmt::Display for UploadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorizationRefused { code, detail } => {
                write!(
                    f,
                    "the API refused to authorize the upload ({code}): {detail}"
                )
            }
            Self::TransferFailed { attempts, detail } => write!(
                f,
                "the archive did not transfer after {attempts} attempts: {detail}"
            ),
            Self::NotStored { detail } => {
                write!(f, "the archive is not stored as reported: {detail}")
            }
            Self::ArchiveUnreadable { detail } => {
                write!(f, "the local archive could not be read: {detail}")
            }
        }
    }
}

/// The two calls this needs to make, behind a seam.
///
/// A trait rather than direct `ureq` calls so the retry and refusal behaviour is
/// testable without a network or a store — the parts most likely to be wrong are
/// exactly the parts a live test would exercise least.
pub trait ArchiveUploadTransport {
    /// Ask the API to authorize one upload of `digest`, `size_bytes` long.
    fn authorize(
        &self,
        job_id: &str,
        digest: &str,
        size_bytes: u64,
    ) -> Result<UploadAuthorization, UploadFailure>;

    /// PUT `body` to `url`. Returns the HTTP status.
    ///
    /// Takes the bytes rather than a path so an implementation cannot decide to
    /// upload something other than what was verified.
    fn put(&self, url: &str, body: &[u8]) -> Result<u16, String>;
}

/// Put the archive in the store, retrying with a fresh authorization each time.
///
/// Returns the object key the API derived. The caller reports that key; it does
/// not invent one, and it never reports a local path.
pub fn upload_source_archive(
    transport: &dyn ArchiveUploadTransport,
    job_id: &str,
    archive: &LocalArchive,
) -> Result<String, UploadFailure> {
    let digest = archive.digest();
    let body = std::fs::read(archive.path()).map_err(|e| UploadFailure::ArchiveUnreadable {
        detail: format!("{e}"),
    })?;
    let size_bytes = body.len() as u64;

    // What the archive step measured, against what is on disk now. A mismatch
    // means the file changed after it was frozen, so the digest that authorizes
    // the upload no longer describes the bytes being sent — and the API would
    // store them under a key that lies about them.
    if size_bytes != archive.size_bytes() {
        return Err(UploadFailure::NotStored {
            detail: format!(
                "the local archive is {size_bytes} bytes but was frozen at {}",
                archive.size_bytes()
            ),
        });
    }
    let actual_digest = capsule::blob::source_archive_hash(&body);
    if actual_digest != digest {
        return Err(UploadFailure::NotStored {
            detail: format!(
                "the local archive digest changed after freeze: expected {digest}, measured {actual_digest}"
            ),
        });
    }
    let expected_object_key = snapshot::source_materialization::object_key_for_archive(digest)
        .map_err(|e| UploadFailure::NotStored {
            detail: format!("derive the content-addressed object key: {e}"),
        })?;

    let mut last = String::new();
    for attempt in 1..=MAX_UPLOAD_ATTEMPTS {
        // A FRESH authorization every time. Reusing the previous URL would mean
        // retrying against a grant that may already have expired, and holding a
        // URL across retries is how a short TTL becomes a long one.
        let authorization = transport.authorize(job_id, digest, size_bytes)?;
        if authorization.object_key != expected_object_key {
            return Err(UploadFailure::NotStored {
                detail: format!(
                    "the API authorized object key {} but the archive digest requires {expected_object_key}",
                    authorization.object_key
                ),
            });
        }

        match transport.put(&authorization.url, &body) {
            Ok(status) if (200..300).contains(&status) => {
                return Ok(authorization.object_key);
            }
            Ok(status) => {
                // No URL in the message: it would be a bearer credential in a
                // failure ack that ato-api stores.
                last = format!("attempt {attempt} returned HTTP {status}");
            }
            Err(e) => {
                last = format!("attempt {attempt} failed: {e}");
            }
        }
    }

    Err(UploadFailure::TransferFailed {
        attempts: MAX_UPLOAD_ATTEMPTS,
        detail: last,
    })
}

/// A presigned URL with its query string removed.
///
/// The signature and credential live in the query, so the whole URL must not
/// reach a log line, an ack body, or an error message.
pub fn redact_url(url: &str) -> String {
    match url.split_once('?') {
        Some((base, _)) => format!("{base}?<presigned>"),
        None => url.to_string(),
    }
}

/// The archive as it exists on this builder, before it goes anywhere.
///
/// The path is deliberately not `pub`: it is a builder-internal temporary, and
/// the whole point of this module is that a local path is not something the API
/// or the database should ever be told about. Callers get the digest and the
/// size, which are facts about the bytes rather than about this host.
pub struct LocalArchive {
    path: PathBuf,
    digest: String,
    size_bytes: u64,
}

impl LocalArchive {
    pub fn new(path: PathBuf, digest: String, size_bytes: u64) -> Self {
        Self {
            path,
            digest,
            size_bytes,
        }
    }

    /// For the upload only. Not for reporting.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// Remove the local copy.
    ///
    /// Called only after the report is accepted, or on a terminal failure.
    /// Removing it earlier — on the first transfer error, say — would make the
    /// retry impossible, which is the opposite of what a cleanup is for.
    pub fn discard(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl fmt::Debug for LocalArchive {
    /// No path. A builder-local path in a log line invites someone to put it in
    /// an ack, and an ack is a contract.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalArchive")
            .field("digest", &self.digest)
            .field("size_bytes", &self.size_bytes)
            .finish()
    }
}

/// The real transport: ato-api for authorization, the store for the bytes.
///
/// Nothing here logs a URL. `ureq` errors are stringified through
/// [`redact_url`]-safe paths only — the URL is never interpolated into a message
/// that leaves this process.
pub struct HttpArchiveUploadTransport<'a> {
    pub api_url: &'a str,
    pub token: &'a str,
    pub agent_id: &'a str,
}

impl ArchiveUploadTransport for HttpArchiveUploadTransport<'_> {
    fn authorize(
        &self,
        job_id: &str,
        digest: &str,
        size_bytes: u64,
    ) -> Result<UploadAuthorization, UploadFailure> {
        let response = ureq::post(&format!(
            "{}/v1/capsule-snapshots/jobs/{job_id}/source-archive/upload-authorization",
            self.api_url
        ))
        .set("authorization", &format!("Bearer {}", self.token))
        .send_json(ureq::json!({
            "agent_id": self.agent_id,
            "source_archive_digest": digest,
            "size_bytes": size_bytes,
        }));

        let body: serde_json::Value = match response {
            Ok(r) => r
                .into_json()
                .map_err(|e| UploadFailure::AuthorizationRefused {
                    code: "unreadable_response".to_string(),
                    detail: format!("{e}"),
                })?,
            Err(ureq::Error::Status(status, r)) => {
                // The API's own code, so the builder's failure ack says what the
                // API decided rather than restating it as a generic error.
                let text = r.into_string().unwrap_or_default();
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
                    .unwrap_or_else(|| format!("http_{status}"));
                return Err(UploadFailure::AuthorizationRefused {
                    code,
                    detail: format!("authorization returned HTTP {status}"),
                });
            }
            Err(e) => {
                return Err(UploadFailure::AuthorizationRefused {
                    code: "transport".to_string(),
                    detail: format!("{e}"),
                });
            }
        };

        let field = |name: &str| -> Result<String, UploadFailure> {
            body.get(name)
                .and_then(|v| v.as_str())
                .map(String::from)
                .ok_or_else(|| UploadFailure::AuthorizationRefused {
                    code: "malformed_authorization".to_string(),
                    detail: format!("response has no {name}"),
                })
        };

        Ok(UploadAuthorization {
            url: field("upload_url")?,
            object_key: field("object_key")?,
            expires_in_seconds: body
                .get("expires_in_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        })
    }

    fn put(&self, url: &str, body: &[u8]) -> Result<u16, String> {
        match ureq::put(url)
            .set("content-type", "application/octet-stream")
            .send_bytes(body)
        {
            Ok(r) => Ok(r.status()),
            Err(ureq::Error::Status(status, _)) => Ok(status),
            // `ureq`'s Display for a transport error can include the URL, which
            // is a bearer credential. Only the kind is reported.
            Err(ureq::Error::Transport(t)) => Err(format!("transport error: {}", t.kind())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;
    use tempfile::TempDir;

    const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const KEY: &str =
        "source-archives/sha256/1111111111111111111111111111111111111111111111111111111111111111";

    struct FakeTransport {
        /// One entry per PUT attempt: the status to return, or an error.
        put_results: RefCell<Vec<Result<u16, String>>>,
        authorize_calls: RefCell<u32>,
        urls_issued: RefCell<Vec<String>>,
        bodies: RefCell<Vec<usize>>,
        refuse_authorization: bool,
    }

    impl FakeTransport {
        fn with(put_results: Vec<Result<u16, String>>) -> Self {
            Self {
                put_results: RefCell::new(put_results),
                authorize_calls: RefCell::new(0),
                urls_issued: RefCell::new(Vec::new()),
                bodies: RefCell::new(Vec::new()),
                refuse_authorization: false,
            }
        }
        fn refusing() -> Self {
            Self {
                put_results: RefCell::new(Vec::new()),
                authorize_calls: RefCell::new(0),
                urls_issued: RefCell::new(Vec::new()),
                bodies: RefCell::new(Vec::new()),
                refuse_authorization: true,
            }
        }
    }

    impl ArchiveUploadTransport for FakeTransport {
        fn authorize(
            &self,
            _job_id: &str,
            digest: &str,
            _size_bytes: u64,
        ) -> Result<UploadAuthorization, UploadFailure> {
            if self.refuse_authorization {
                return Err(UploadFailure::AuthorizationRefused {
                    code: "SOURCE_ARCHIVE_TRANSPORT_UNCONFIGURED".to_string(),
                    detail: "not configured".to_string(),
                });
            }
            let n = {
                let mut calls = self.authorize_calls.borrow_mut();
                *calls += 1;
                *calls
            };
            // A different URL each time, as a real API issues.
            let object_key =
                snapshot::source_materialization::object_key_for_archive(digest).unwrap();
            let url = format!("https://store.example/{object_key}?X-Amz-Signature=sig{n}");
            self.urls_issued.borrow_mut().push(url.clone());
            Ok(UploadAuthorization {
                url,
                object_key,
                expires_in_seconds: 300,
            })
        }

        fn put(&self, _url: &str, body: &[u8]) -> Result<u16, String> {
            self.bodies.borrow_mut().push(body.len());
            let mut results = self.put_results.borrow_mut();
            if results.is_empty() {
                Ok(200)
            } else {
                results.remove(0)
            }
        }
    }

    /// A `LocalArchive` whose recorded size matches the file on disk.
    fn local(path: &Path) -> LocalArchive {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let bytes = std::fs::read(path).unwrap();
        LocalArchive::new(
            path.to_path_buf(),
            capsule::blob::source_archive_hash(&bytes),
            size,
        )
    }

    fn key_for(bytes: &[u8]) -> String {
        snapshot::source_materialization::object_key_for_archive(
            &capsule::blob::source_archive_hash(bytes),
        )
        .unwrap()
    }

    fn archive(dir: &TempDir, bytes: &[u8]) -> PathBuf {
        let p = dir.path().join("source.tar.zst");
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(bytes).expect("write");
        p
    }

    #[test]
    fn uploads_and_returns_the_key_the_api_derived() {
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"archive bytes");
        let transport = FakeTransport::with(vec![Ok(200)]);

        let key = upload_source_archive(&transport, "job_1", &local(&path)).expect("upload");

        // The key comes from the API, not from anything the builder computed.
        assert_eq!(key, key_for(b"archive bytes"));
        assert_eq!(*transport.authorize_calls.borrow(), 1);
        assert_eq!(transport.bodies.borrow().as_slice(), &[13]);
    }

    #[test]
    fn asks_for_a_fresh_url_on_every_retry() {
        // Reusing the previous URL would mean retrying against a grant that may
        // already have expired. Holding one across retries is how a short TTL
        // quietly becomes a long one.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let transport = FakeTransport::with(vec![Ok(503), Err("reset".into()), Ok(200)]);

        let key = upload_source_archive(&transport, "job_1", &local(&path)).expect("upload");

        assert_eq!(key, key_for(b"bytes"));
        assert_eq!(*transport.authorize_calls.borrow(), 3);
        let urls = transport.urls_issued.borrow();
        assert_eq!(urls.len(), 3);
        assert_ne!(urls[0], urls[1]);
        assert_ne!(urls[1], urls[2]);
    }

    #[test]
    fn the_object_key_does_not_change_across_retries() {
        // The key is derived from the bytes, and the bytes did not change.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let transport = FakeTransport::with(vec![Ok(500), Ok(200)]);
        let key = upload_source_archive(&transport, "job_1", &local(&path)).expect("upload");
        assert_eq!(key, key_for(b"bytes"));
    }

    #[test]
    fn gives_up_after_a_bounded_number_of_attempts() {
        // Unbounded retry against a failing store is how one job consumes a
        // builder indefinitely.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let transport = FakeTransport::with(vec![Ok(500), Ok(500), Ok(500), Ok(200)]);

        let failure =
            upload_source_archive(&transport, "job_1", &local(&path)).expect_err("must give up");

        assert_eq!(failure.code(), "upload_transfer_failed");
        assert_eq!(
            *transport.authorize_calls.borrow(),
            MAX_UPLOAD_ATTEMPTS,
            "one authorization per attempt, and no more attempts than the bound"
        );
    }

    #[test]
    fn stops_immediately_when_authorization_is_refused() {
        // A refusal is not a transient transfer error: retrying it just asks the
        // same question again.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let transport = FakeTransport::refusing();

        let failure =
            upload_source_archive(&transport, "job_1", &local(&path)).expect_err("must refuse");

        assert_eq!(failure.code(), "upload_authorization_refused");
        assert!(transport.bodies.borrow().is_empty(), "nothing was sent");
    }

    #[test]
    fn a_missing_local_archive_is_not_a_transfer_problem() {
        let dir = TempDir::new().expect("tempdir");
        let transport = FakeTransport::with(vec![]);
        let failure = upload_source_archive(
            &transport,
            "job_1",
            &LocalArchive::new(dir.path().join("absent.tar.zst"), DIGEST.to_string(), 5),
        )
        .expect_err("must fail");
        assert_eq!(failure.code(), "archive_unreadable");
        assert_eq!(*transport.authorize_calls.borrow(), 0);
    }

    #[test]
    fn no_failure_message_carries_a_url() {
        // A failure detail is acked to the API and stored. A presigned URL in one
        // is a bearer credential in a database.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let transport = FakeTransport::with(vec![Ok(500), Ok(500), Ok(500)]);
        let failure = upload_source_archive(&transport, "job_1", &local(&path)).expect_err("fail");
        let rendered = failure.to_string();
        assert!(!rendered.contains("X-Amz-Signature"));
        assert!(!rendered.contains("https://store.example"));
    }

    #[test]
    fn debug_output_redacts_the_url() {
        let authorization = UploadAuthorization {
            url: "https://store.example/key?X-Amz-Signature=abc&X-Amz-Credential=AKIA".to_string(),
            object_key: KEY.to_string(),
            expires_in_seconds: 300,
        };
        let rendered = format!("{authorization:?}");
        assert!(!rendered.contains("X-Amz-Signature"));
        assert!(!rendered.contains("AKIA"));
        assert!(rendered.contains("<presigned>"));
        // Still says which object, which is the whole reason to log anything.
        assert!(rendered.contains(KEY));
    }

    #[test]
    fn the_local_path_never_appears_in_debug_output() {
        // A path in a log invites someone to put it in an ack, and an ack is a
        // contract. `LocalArchive` reports facts about the BYTES instead.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let local = LocalArchive::new(path.clone(), DIGEST.to_string(), 5);
        let rendered = format!("{local:?}");
        assert!(!rendered.contains(&path.display().to_string()));
        assert!(rendered.contains(DIGEST));
    }

    #[test]
    fn the_archive_survives_a_failed_upload() {
        // Cleaning up on the first transfer error would make the retry
        // impossible, which is the opposite of what a cleanup is for.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let transport = FakeTransport::with(vec![Ok(500), Ok(500), Ok(500)]);
        let _ = upload_source_archive(&transport, "job_1", &local(&path));
        assert!(path.exists(), "the local archive must remain for a retry");
    }

    #[test]
    fn refuses_to_upload_bytes_that_are_not_the_ones_that_were_frozen() {
        // The digest authorizing the upload describes the bytes as frozen. If the
        // file changed since, sending it would have the API store bytes under a
        // key that lies about them.
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        let transport = FakeTransport::with(vec![Ok(200)]);
        let stale = LocalArchive::new(path, DIGEST.to_string(), 99);

        let failure = upload_source_archive(&transport, "job_1", &stale).expect_err("must refuse");

        assert_eq!(failure.code(), "upload_not_stored");
        assert!(transport.bodies.borrow().is_empty(), "nothing was sent");
    }

    #[test]
    fn refuses_same_size_bytes_with_a_different_digest() {
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"first");
        let frozen = local(&path);
        std::fs::write(&path, b"other").expect("replace with same-size bytes");
        let transport = FakeTransport::with(vec![Ok(200)]);

        let failure = upload_source_archive(&transport, "job_1", &frozen).expect_err("must refuse");

        assert_eq!(failure.code(), "upload_not_stored");
        assert_eq!(*transport.authorize_calls.borrow(), 0);
        assert!(transport.bodies.borrow().is_empty(), "nothing was sent");
    }

    #[test]
    fn discard_removes_it() {
        let dir = TempDir::new().expect("tempdir");
        let path = archive(&dir, b"bytes");
        LocalArchive::new(path.clone(), DIGEST.to_string(), 5).discard();
        assert!(!path.exists());
    }

    #[test]
    fn redacting_keeps_the_object_and_drops_the_signature() {
        assert_eq!(
            redact_url("https://s.example/a/b?X-Amz-Signature=deadbeef"),
            "https://s.example/a/b?<presigned>"
        );
        assert_eq!(redact_url("https://s.example/a/b"), "https://s.example/a/b");
    }
}
