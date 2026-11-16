//! Streaming transport for the Capsule Hub large-bundle lane.
//!
//! Contract: docs/rfcs/draft/capsule-hub-m2-native.md sections 3 and 4. A bundle
//! larger than the Hub's 8 MiB Base64 JSON bound is moved with presigned URLs,
//! and neither transfer buffers the bundle:
//!
//! - **Upload** streams the file from disk through HashingReader while SHA-256
//!   is computed over exactly the bytes the body carries, so the bytes that
//!   arrive at R2 are the bytes the declared digest names. R2 additionally signs
//!   content-type and x-amz-checksum-sha256 into the URL.
//! - **Download** streams the response body into a 0600 temporary file in
//!   $ATO_HOME/hub, hashing and counting as it writes, fsyncs it, verifies size
//!   *and* digest, renames it onto "<hex>.capsule" (atomic within the
//!   directory), and then re-hashes the stored file from disk. No partial or
//!   mismatching download ever takes the cache name.
//!
//! The sibling Base64 lane (hub_public) keeps its own behaviour; this module is
//! only reached for the large path or for a presigned URL.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::{Body, Client, Response};
use reqwest::header::{ACCEPT, HeaderMap, HeaderName, HeaderValue};
use sha2::{Digest, Sha256};

/// Read/write chunk size: large enough to amortise syscalls, small enough that
/// a multi-gigabyte transfer never needs one allocation.
pub(crate) const STREAM_CHUNK_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A presigned transfer carries a whole bundle; the per-request timeout that
/// bounds the JSON calls would abort a legitimate 1 GB upload.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
/// How much of a refusal body is read for its error code.
const ERROR_BODY_BYTES: u64 = 8 * 1024;

/// An HTTP refusal kept structured, so the caller can choose between falling
/// back to the Base64 lane, retrying and failing.
#[derive(Debug)]
pub(crate) enum TransferError {
    /// A non-success HTTP answer, with the API's error code when it has one.
    Refused { status: u16, code: String },
    /// Anything else: local IO, DNS/TLS, a malformed body, a size or digest
    /// mismatch. Never recoverable by a different lane.
    Failed(anyhow::Error),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { status, code } if code.is_empty() => {
                write!(
                    formatter,
                    "the endpoint refused the transfer (HTTP {status})"
                )
            }
            Self::Refused { status, code } => {
                write!(
                    formatter,
                    "the endpoint refused the transfer (HTTP {status}, {code})"
                )
            }
            Self::Failed(error) => write!(formatter, "{error:#}"),
        }
    }
}

impl std::error::Error for TransferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Failed(error) => Some(error.as_ref()),
            Self::Refused { .. } => None,
        }
    }
}

impl From<anyhow::Error> for TransferError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

fn transfer_failed(code: &str, message: impl std::fmt::Display) -> TransferError {
    TransferError::Failed(anyhow::anyhow!("{code}: {message}"))
}

type SharedSha256 = Arc<Mutex<Sha256>>;

fn shared_sha256() -> SharedSha256 {
    Arc::new(Mutex::new(Sha256::new()))
}

/// Hashes every byte that is actually read out of the file into the request
/// body, so the digest describes the transmitted bytes rather than a second read.
struct HashingReader<R> {
    inner: R,
    hasher: SharedSha256,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read > 0 {
            self.hasher
                .lock()
                .expect("the stream digest lock is never poisoned")
                .update(&buffer[..read]);
        }
        Ok(read)
    }
}

/// Hashes every byte written to the download file, so the digest is computed
/// over the stored bytes as they arrive.
struct HashingWriter<W> {
    inner: W,
    hasher: SharedSha256,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        if written > 0 {
            self.hasher
                .lock()
                .expect("the stream digest lock is never poisoned")
                .update(&buffer[..written]);
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn hex_digest(hasher: &SharedSha256) -> String {
    let finalized = hasher
        .lock()
        .expect("the stream digest lock is never poisoned")
        .clone()
        .finalize();
    format!("sha256:{finalized:x}")
}

/// Stream a file and return its exact byte length and SHA-256 without buffering
/// it. This is the digest declared to .../uploads before the body is sent.
pub(crate) fn stream_file_sha256(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; STREAM_CHUNK_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((total, format!("sha256:{:x}", hasher.finalize())))
}

/// A client for presigned transfers: no redirects (a presigned URL is single
/// key and single method), long overall timeout, short connect timeout.
pub(crate) fn transfer_client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TRANSFER_TIMEOUT)
        .user_agent(concat!("ato/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build the Ato transfer HTTP client")
}

/// A presigned URL must stay HTTPS, or plain HTTP on a literal loopback host so
/// the loopback test servers work exactly like the API client.
fn presigned_url(raw: &str) -> std::result::Result<reqwest::Url, TransferError> {
    let url = reqwest::Url::parse(raw).map_err(|error| {
        transfer_failed(
            crate::hub_public::ERR_API_FAILED,
            format!("the API returned an unusable transfer URL: {error}"),
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(transfer_failed(
            crate::hub_public::ERR_API_FAILED,
            "the API returned a transfer URL with userinfo",
        ));
    }
    if url.fragment().is_some() {
        return Err(transfer_failed(
            crate::hub_public::ERR_API_FAILED,
            "the API returned a transfer URL with a fragment",
        ));
    }
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(transfer_failed(
            crate::hub_public::ERR_API_FAILED,
            format!(
                "the transfer URL {raw} must use HTTPS (plain HTTP is allowed only for a loopback host)"
            ),
        ));
    }
    Ok(url)
}

/// The signed headers, applied verbatim except for names the HTTP client owns
/// and credentials that must never reach a presigned host.
fn signed_headers(
    headers: &BTreeMap<String, String>,
) -> std::result::Result<HeaderMap, TransferError> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "host" | "content-length" | "authorization" | "cookie"
        ) {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            transfer_failed(
                crate::hub_public::ERR_API_FAILED,
                format!("the API returned an unusable upload header name: {error}"),
            )
        })?;
        let header_value = HeaderValue::from_str(value).map_err(|error| {
            transfer_failed(
                crate::hub_public::ERR_API_FAILED,
                format!("the API returned an unusable upload header value: {error}"),
            )
        })?;
        map.insert(header_name, header_value);
    }
    Ok(map)
}

fn error_code(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    value
        .get("error")
        .and_then(|error| error.as_str())
        .map(|error| {
            error
                .chars()
                .filter(|character| !character.is_control())
                .take(120)
                .collect()
        })
        .unwrap_or_default()
}

fn read_error_body(response: Response) -> String {
    let mut text = String::new();
    let _ = response.take(ERROR_BODY_BYTES).read_to_string(&mut text);
    text
}

fn refused(status: StatusCode, body: &str) -> TransferError {
    TransferError::Refused {
        status: status.as_u16(),
        code: error_code(body),
    }
}

/// Stream the bundle to the presigned URL with the signed headers, verifying
/// both the declared size and the digest of the bytes that were actually sent.
pub(crate) fn stream_put(
    client: &Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    bundle: &Path,
    declared_size: u64,
    declared_digest: &str,
) -> std::result::Result<(), TransferError> {
    let url = presigned_url(url)?;
    let file = File::open(bundle).with_context(|| format!("open {}", bundle.display()))?;
    let on_disk = file
        .metadata()
        .with_context(|| format!("inspect {}", bundle.display()))?
        .len();
    if on_disk != declared_size {
        return Err(transfer_failed(
            crate::hub_public::ERR_SIZE_MISMATCH,
            format!(
                "{} is {on_disk} bytes; the upload declares {declared_size} bytes",
                bundle.display()
            ),
        ));
    }
    let hasher = shared_sha256();
    let reader = HashingReader {
        inner: file,
        hasher: Arc::clone(&hasher),
    };
    let response = client
        .put(url)
        .headers(signed_headers(headers)?)
        .header(ACCEPT, "*/*")
        .body(Body::sized(reader, declared_size))
        .send()
        .map_err(|error| {
            transfer_failed(
                crate::hub_public::ERR_API_FAILED,
                format!("failed while uploading the bundle: {error}"),
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = read_error_body(response);
        return Err(refused(status, &body));
    }
    // The body was streamed; the digest covers exactly those bytes.
    let observed = hex_digest(&hasher);
    if observed != declared_digest {
        return Err(transfer_failed(
            crate::hub_public::ERR_DIGEST_MISMATCH,
            format!(
                "the uploaded bytes hash to {observed}; the declared digest is {declared_digest}"
            ),
        ));
    }
    Ok(())
}

/// The verified cache path for a digest, when the stored file still hashes to
/// it. A corrupt entry is not trusted and is replaced by the next download.
pub(crate) fn verified_cache_path(
    ato_home: &Path,
    digest: &str,
    size: u64,
) -> Result<Option<PathBuf>> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Ok(None);
    };
    let path = ato_home.join("hub").join(format!("{hex}.capsule"));
    if !path.is_file() {
        return Ok(None);
    }
    let Ok((stored, stored_digest)) = stream_file_sha256(&path) else {
        return Ok(None);
    };
    Ok((stored == size && stored_digest == digest).then_some(path))
}

/// Stream the URL into $ATO_HOME/hub/<digest>.capsule, verifying size and digest
/// before the atomic rename and re-verifying the stored file afterwards.
pub(crate) fn stream_download(
    client: &Client,
    url: &str,
    expected_digest: &str,
    expected_size: u64,
    ato_home: &Path,
) -> std::result::Result<PathBuf, TransferError> {
    let Some(hex) = expected_digest.strip_prefix("sha256:") else {
        return Err(transfer_failed(
            crate::hub_public::ERR_INVALID_DIGEST,
            format!("{expected_digest} is not a sha256 digest"),
        ));
    };
    let directory = ato_home.join("hub");
    fs::create_dir_all(&directory)
        .with_context(|| format!("create the Hub cache directory {}", directory.display()))?;
    let path = directory.join(format!("{hex}.capsule"));
    if let Some(cached) = verified_cache_path(ato_home, expected_digest, expected_size)? {
        return Ok(cached);
    }

    let url = presigned_url(url)?;
    let response = client
        .get(url)
        .header(ACCEPT, "application/octet-stream")
        .send()
        .map_err(|error| {
            transfer_failed(
                crate::hub_public::ERR_API_FAILED,
                format!("failed while downloading the capsule: {error}"),
            )
        })?;
    let status = response.status();
    if !status.is_success() {
        let body = read_error_body(response);
        return Err(refused(status, &body));
    }
    if let Some(length) = response.content_length()
        && length != expected_size
    {
        return Err(transfer_failed(
            crate::hub_public::ERR_SIZE_MISMATCH,
            format!(
                "the artifact announces {length} bytes; the resolved version is {expected_size} bytes"
            ),
        ));
    }

    let temporary = directory.join(format!(
        ".{hex}.{}.{}.part",
        std::process::id(),
        unix_now_nanos()
    ));
    if let Err(error) = receive_body(&temporary, response, expected_size, expected_digest) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = rename_over(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(TransferError::Failed(error));
    }
    crate::hub::sync_directory_best_effort(&directory);

    // "streamed, then verified from disk": the stored bytes are hashed again so
    // a short write cannot pass as the resolved version.
    let (stored, stored_digest) = match stream_file_sha256(&path) {
        Ok(result) => result,
        Err(error) => {
            let _ = fs::remove_file(&path);
            return Err(TransferError::Failed(error));
        }
    };
    if stored != expected_size {
        let _ = fs::remove_file(&path);
        return Err(transfer_failed(
            crate::hub_public::ERR_SIZE_MISMATCH,
            format!(
                "the stored capsule is {stored} bytes; the resolved version is {expected_size} bytes"
            ),
        ));
    }
    if stored_digest != expected_digest {
        let _ = fs::remove_file(&path);
        return Err(transfer_failed(
            crate::hub_public::ERR_DIGEST_MISMATCH,
            format!(
                "the stored capsule hashes to {stored_digest}; the resolved version is {expected_digest}"
            ),
        ));
    }
    Ok(path)
}

/// Write the response body to the path (0600, create-new), fsync it, and check
/// the byte count and digest before returning.
fn receive_body(
    path: &Path,
    response: Response,
    expected_size: u64,
    expected_digest: &str,
) -> std::result::Result<(), TransferError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .with_context(|| format!("create the Hub capsule {}", path.display()))?;
    let hasher = shared_sha256();
    let mut writer = HashingWriter {
        inner: file,
        hasher: Arc::clone(&hasher),
    };
    let mut total = 0_u64;
    let mut body = response.take(expected_size.saturating_add(1));
    let mut buffer = vec![0_u8; STREAM_CHUNK_BYTES];
    loop {
        let read = body.read(&mut buffer).map_err(|error| {
            transfer_failed(
                crate::hub_public::ERR_API_FAILED,
                format!("failed while downloading the capsule: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read]).map_err(|error| {
            transfer_failed(
                crate::hub_public::ERR_API_FAILED,
                format!("failed to write {}: {error}", path.display()),
            )
        })?;
        total += read as u64;
    }
    writer.flush().map_err(|error| {
        transfer_failed(
            crate::hub_public::ERR_API_FAILED,
            format!("failed to flush {}: {error}", path.display()),
        )
    })?;
    writer.inner.sync_all().map_err(|error| {
        transfer_failed(
            crate::hub_public::ERR_API_FAILED,
            format!("failed to flush {}: {error}", path.display()),
        )
    })?;

    if total != expected_size {
        return Err(transfer_failed(
            crate::hub_public::ERR_SIZE_MISMATCH,
            format!("the artifact is {total} bytes; the resolved version is {expected_size} bytes"),
        ));
    }
    let observed = hex_digest(&hasher);
    if observed != expected_digest {
        return Err(transfer_failed(
            crate::hub_public::ERR_DIGEST_MISMATCH,
            format!("the artifact hashes to {observed}; the resolved version is {expected_digest}"),
        ));
    }
    Ok(())
}

/// Rename within the cache directory. Windows refuses to replace an existing
/// file, so a verified-but-corrupt cache entry is removed first and the rename
/// retried once.
fn rename_over(temporary: &Path, path: &Path) -> Result<()> {
    match fs::rename(temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if path.exists() => {
            fs::remove_file(path)
                .with_context(|| format!("replace the Hub capsule {}", path.display()))?;
            fs::rename(temporary, path).with_context(|| {
                format!("move the Hub capsule into {} after {error}", path.display())
            })
        }
        Err(error) => {
            Err(error).with_context(|| format!("move the Hub capsule into {}", path.display()))
        }
    }
}

fn unix_now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
