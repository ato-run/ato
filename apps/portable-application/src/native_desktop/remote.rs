//! Streaming acquisition of a digest-pinned remote native artifact.
//!
//! The route names an upstream URL, the exact byte count and the `sha256:`
//! digest. This module is the only place that turns those three facts into
//! bytes on disk, and it never trusts anything else about the response:
//!
//! * every body is written through a hashing, size-capped writer, so a
//!   substituted or truncated response is detected before it is used;
//! * a missing asset is a distinct failure from a digest mismatch, because the
//!   first is "upstream removed it" and the second is "upstream changed it";
//! * the bytes land in a temporary file beside the destination and are renamed
//!   only after both checks pass, so an interrupted Run leaves no partial
//!   artifact and no file a later stage could mistake for a verified one.
//!
//! Ato is not the distributor of a remote artifact: nothing here copies the
//! bytes into a registry, and the declaration is the only thing that pins them.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ato_formation::native_desktop::RemoteArtifact;
use sha2::{Digest, Sha256};

/// How long one fetch may take end to end.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A named fetch refusal.
///
/// The variants stay distinct on purpose: "404", "the bytes are not the pinned
/// artifact" and "the transport failed" are three different user actions, and
/// collapsing them would hide an upstream change behind a transient error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchFailure {
    #[error("native_fetch_url_invalid: {0}")]
    UrlInvalid(String),
    #[error("native_fetch_not_found: the upstream asset is gone (HTTP {0})")]
    NotFound(u16),
    #[error("native_fetch_unavailable: {0}")]
    Unavailable(String),
    #[error(
        "native_fetch_size_mismatch: the declaration pins {expected} bytes but the response carried {actual}"
    )]
    SizeMismatch { expected: u64, actual: String },
    #[error(
        "native_fetch_digest_mismatch: the declaration pins {expected} but the fetched bytes are {actual}"
    )]
    DigestMismatch { expected: String, actual: String },
}

/// A transport that streams a URL body into a writer and returns the number of
/// bytes it delivered.
pub trait NativeFetcher {
    fn fetch(&self, url: &str, sink: &mut dyn Write) -> Result<u64, FetchFailure>;
}

/// The production transport: one blocking HTTPS GET with a bounded redirect
/// chain.
pub struct ReqwestFetcher {
    timeout: Duration,
}

impl Default for ReqwestFetcher {
    fn default() -> Self {
        Self {
            timeout: FETCH_TIMEOUT,
        }
    }
}

impl NativeFetcher for ReqwestFetcher {
    fn fetch(&self, url: &str, sink: &mut dyn Write) -> Result<u64, FetchFailure> {
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|error| FetchFailure::Unavailable(error.to_string()))?;
        let mut response = client
            .get(url)
            .send()
            .map_err(|error| FetchFailure::Unavailable(error.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
            return Err(FetchFailure::NotFound(status.as_u16()));
        }
        if !status.is_success() {
            return Err(FetchFailure::Unavailable(format!(
                "the upstream asset returned HTTP {}",
                status.as_u16()
            )));
        }
        let mut buffer = [0_u8; 64 * 1024];
        let mut total = 0_u64;
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| FetchFailure::Unavailable(error.to_string()))?;
            if read == 0 {
                break;
            }
            sink.write_all(&buffer[..read])
                .map_err(|error| FetchFailure::Unavailable(error.to_string()))?;
            total += read as u64;
        }
        Ok(total)
    }
}

/// A URL the fetch layer will open.
///
/// A declaration is https-only; the loopback exception exists so the transport
/// can be exercised against a local server, and it cannot reach a third party.
pub fn valid_fetch_url(value: &str) -> bool {
    if ato_formation::native_desktop::valid_native_remote_url(value) {
        return true;
    }
    let Some(rest) = value.strip_prefix("http://") else {
        return false;
    };
    let Some((host, _path)) = ato_formation::native_desktop::split_url_host(rest) else {
        return false;
    };
    let name = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((address, tail)) if tail.is_empty() || tail.starts_with(':') => address,
            _ => return false,
        },
        None => match host.split_once(':') {
            Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                name
            }
            Some(_) => return false,
            None => host,
        },
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// The writer every fetch body goes through.
///
/// It hashes, counts and refuses to store more than the declaration pins, so a
/// response that is larger than the artifact cannot fill the disk before the
/// mismatch is noticed.
struct PinnedWriter<'a> {
    file: &'a mut File,
    hasher: Sha256,
    written: u64,
    limit: u64,
    exceeded: bool,
}

impl Write for PinnedWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.written);
        if buffer.len() as u64 > remaining {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "native_fetch_size_mismatch: the response is larger than the declaration pins",
            ));
        }
        self.hasher.update(buffer);
        self.file.write_all(buffer)?;
        self.written += buffer.len() as u64;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// A verified fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedArtifact {
    /// The file the verified bytes were renamed into.
    pub path: PathBuf,
    pub bytes: u64,
    /// The digest recomputed from the bytes that landed on disk.
    pub sha256: String,
}

/// Largest source tarball this build will download. A source tree is smaller
/// than a packaged desktop application, so the artifact bound is reused.
#[cfg(target_os = "macos")]
pub const MAX_NATIVE_SOURCE_BYTES: u64 = ato_formation::native_desktop::MAX_NATIVE_REMOTE_BYTES;

/// Fetch one pinned remote artifact to `destination`.
///
/// The destination is created atomically: a failed fetch removes its temporary
/// file and never leaves a short or mismatched artifact behind.
#[cfg(test)]
pub fn fetch_to_file(
    remote: &RemoteArtifact,
    destination: &Path,
    fetcher: &dyn NativeFetcher,
) -> Result<FetchedArtifact, FetchFailure> {
    fetch_checked(
        &remote.url,
        &remote.sha256,
        remote.bytes,
        Some(remote.bytes),
        destination,
        fetcher,
    )
}

/// Reuse only rehashed bytes, and publish downloads atomically from a unique
/// directory so concurrent Instances cannot truncate each other's transfers.
#[allow(clippy::too_many_arguments)]
pub fn fetch_cached(
    url: &str,
    sha256: &str,
    limit: u64,
    expected_bytes: Option<u64>,
    cache: &Path,
    fetcher: &dyn NativeFetcher,
) -> Result<FetchedArtifact, FetchFailure> {
    let unavailable = |error: std::io::Error| FetchFailure::Unavailable(error.to_string());
    fs::create_dir_all(cache).map_err(unavailable)?;
    let destination = cache.join(sha256.trim_start_matches("sha256:"));
    if let Ok(metadata) = fs::symlink_metadata(&destination)
        && metadata.is_file()
        && metadata.len() <= limit
        && expected_bytes.is_none_or(|expected| metadata.len() == expected)
    {
        let mut input = File::open(&destination).map_err(unavailable)?;
        let mut hash = Sha256::new();
        let size = std::io::copy(&mut input, &mut hash).map_err(unavailable)?;
        let digest = format!("sha256:{:x}", hash.finalize());
        if digest == sha256 && size == metadata.len() {
            return Ok(FetchedArtifact {
                path: destination,
                bytes: size,
                sha256: digest,
            });
        }
    }
    let scratch = tempfile::Builder::new()
        .prefix("download-")
        .tempdir_in(cache)
        .map_err(unavailable)?;
    let mut fetched = fetch_checked(
        url,
        sha256,
        limit,
        expected_bytes,
        &scratch.path().join("artifact"),
        fetcher,
    )?;
    fs::rename(&fetched.path, &destination).map_err(unavailable)?;
    fetched.path = destination;
    Ok(fetched)
}

/// Fetch a body, verify it against one pinned digest, and place it atomically.
///
/// `limit` bounds how much is read; `expected_bytes` additionally requires the
/// exact byte count the contract pins when the declaration names one.
pub fn fetch_checked(
    url: &str,
    sha256: &str,
    limit: u64,
    expected_bytes: Option<u64>,
    destination: &Path,
    fetcher: &dyn NativeFetcher,
) -> Result<FetchedArtifact, FetchFailure> {
    let remote = RemoteArtifact {
        url: url.to_owned(),
        sha256: sha256.to_owned(),
        bytes: limit,
    };
    if !valid_fetch_url(&remote.url) {
        return Err(FetchFailure::UrlInvalid(format!(
            "{} is not an https URL",
            remote.url
        )));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            FetchFailure::Unavailable(format!("could not create {}: {error}", parent.display()))
        })?;
    }
    let temporary = destination.with_extension("part");
    let _ = fs::remove_file(&temporary);
    let mut file = File::create(&temporary).map_err(|error| {
        FetchFailure::Unavailable(format!("could not create {}: {error}", temporary.display()))
    })?;
    let outcome = {
        let mut writer = PinnedWriter {
            file: &mut file,
            hasher: Sha256::new(),
            written: 0,
            limit: remote.bytes,
            exceeded: false,
        };
        let result = fetcher.fetch(&remote.url, &mut writer);
        let exceeded = writer.exceeded;
        let written = writer.written;
        let digest = format!("sha256:{:x}", writer.hasher.finalize());
        if exceeded || written > remote.bytes {
            Err(FetchFailure::SizeMismatch {
                expected: remote.bytes,
                actual: format!("more than {} bytes", remote.bytes),
            })
        } else {
            match result {
                Err(error) => Err(error),
                Ok(_) if expected_bytes.is_some_and(|expected| written != expected) => {
                    Err(FetchFailure::SizeMismatch {
                        expected: expected_bytes.unwrap_or(0),
                        actual: format!("{written} bytes"),
                    })
                }
                Ok(_) if digest != remote.sha256 => Err(FetchFailure::DigestMismatch {
                    expected: remote.sha256.clone(),
                    actual: digest.clone(),
                }),
                Ok(total) => Ok((digest.clone(), total)),
            }
        }
    };
    match outcome {
        Ok((digest, bytes)) => {
            drop(file);
            fs::rename(&temporary, destination).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                FetchFailure::Unavailable(format!(
                    "could not place {}: {error}",
                    destination.display()
                ))
            })?;
            Ok(FetchedArtifact {
                path: destination.to_path_buf(),
                bytes,
                sha256: digest,
            })
        }
        Err(error) => {
            drop(file);
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    struct BytesFetcher<'a>(&'a [u8]);
    impl NativeFetcher for BytesFetcher<'_> {
        fn fetch(&self, _: &str, sink: &mut dyn Write) -> Result<u64, FetchFailure> {
            sink.write_all(self.0).unwrap();
            Ok(self.0.len() as u64)
        }
    }
    struct Offline;
    impl NativeFetcher for Offline {
        fn fetch(&self, _: &str, _: &mut dyn Write) -> Result<u64, FetchFailure> {
            Err(FetchFailure::Unavailable("offline".into()))
        }
    }
    #[test]
    fn cache_is_rehashed_and_corrupt_bytes_never_run_offline() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"trusted artifact";
        let digest = format!("sha256:{:x}", Sha256::digest(bytes));
        let get = |fetcher: &dyn NativeFetcher| {
            fetch_cached(
                "https://example.com/app",
                &digest,
                bytes.len() as u64,
                Some(bytes.len() as u64),
                root.path(),
                fetcher,
            )
        };
        let first = get(&BytesFetcher(bytes)).unwrap();
        assert_eq!(get(&Offline).unwrap().sha256, digest);
        fs::write(&first.path, b"changed artifact").unwrap();
        assert!(get(&Offline).is_err());
        assert_eq!(
            fs::read(get(&BytesFetcher(bytes)).unwrap().path).unwrap(),
            bytes
        );
    }

    /// Serve exactly one request with a canned response and close.
    fn serve(status: &str, body: Vec<u8>) -> String {
        let status = status.to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{address}/artifact.tar.gz")
    }

    fn digest(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    fn remote(url: &str, bytes: &[u8]) -> RemoteArtifact {
        RemoteArtifact {
            url: url.to_owned(),
            sha256: digest(bytes),
            bytes: bytes.len() as u64,
        }
    }

    fn destination() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fetch/artifact.tar.gz");
        (root, path)
    }

    #[test]
    fn a_matching_body_lands_atomically_and_reports_its_own_digest() {
        let body = b"verified artifact bytes".to_vec();
        let url = serve("200 OK", body.clone());
        let (_root, path) = destination();
        let fetched =
            fetch_to_file(&remote(&url, &body), &path, &ReqwestFetcher::default()).unwrap();
        assert_eq!(fetched.bytes, body.len() as u64);
        assert_eq!(fetched.sha256, digest(&body));
        assert_eq!(fs::read(&path).unwrap(), body);
        assert!(!path.with_extension("part").exists());
    }

    #[test]
    fn a_missing_asset_is_reported_as_not_found_not_as_a_digest_mismatch() {
        let url = serve("404 Not Found", b"no such release".to_vec());
        let (_root, path) = destination();
        let pinned = RemoteArtifact {
            url,
            sha256: digest(b"expected"),
            bytes: 8,
        };
        let error = fetch_to_file(&pinned, &path, &ReqwestFetcher::default()).unwrap_err();
        assert_eq!(error, FetchFailure::NotFound(404), "{error}");
        assert!(!path.exists());
        assert!(!path.with_extension("part").exists());
    }

    #[test]
    fn a_changed_asset_is_a_digest_mismatch_and_leaves_nothing_behind() {
        let body = b"the upstream replaced this".to_vec();
        let url = serve("200 OK", body.clone());
        let (_root, path) = destination();
        // The declaration pins the same length but a different digest, which is
        // exactly "upstream changed the bytes of the asset".
        let pinned = RemoteArtifact {
            url,
            sha256: format!("sha256:{}", "0".repeat(64)),
            bytes: body.len() as u64,
        };
        let error = fetch_to_file(&pinned, &path, &ReqwestFetcher::default()).unwrap_err();
        assert!(
            matches!(error, FetchFailure::DigestMismatch { .. }),
            "{error}"
        );
        assert!(!path.exists());
        assert!(!path.with_extension("part").exists());
    }

    #[test]
    fn a_short_body_is_a_size_mismatch() {
        let body = b"short".to_vec();
        let url = serve("200 OK", body.clone());
        let (_root, path) = destination();
        let pinned = RemoteArtifact {
            url,
            sha256: digest(&body),
            bytes: body.len() as u64 + 10,
        };
        let error = fetch_to_file(&pinned, &path, &ReqwestFetcher::default()).unwrap_err();
        assert!(
            matches!(error, FetchFailure::SizeMismatch { .. }),
            "{error}"
        );
        assert!(!path.exists());
    }

    #[test]
    fn an_oversized_body_stops_at_the_declared_bound() {
        let body = vec![b'x'; 4096];
        let url = serve("200 OK", body.clone());
        let (_root, path) = destination();
        let pinned = RemoteArtifact {
            url,
            sha256: digest(&body[..8]),
            bytes: 8,
        };
        let error = fetch_to_file(&pinned, &path, &ReqwestFetcher::default()).unwrap_err();
        assert!(
            matches!(error, FetchFailure::SizeMismatch { .. }),
            "{error}"
        );
        assert!(!path.exists());
        assert!(!path.with_extension("part").exists());
    }

    #[test]
    fn a_server_error_is_unavailable() {
        let url = serve("500 Internal Server Error", Vec::new());
        let (_root, path) = destination();
        let pinned = RemoteArtifact {
            url,
            sha256: digest(b"x"),
            bytes: 1,
        };
        let error = fetch_to_file(&pinned, &path, &ReqwestFetcher::default()).unwrap_err();
        assert!(matches!(error, FetchFailure::Unavailable(_)), "{error}");
        assert!(!path.exists());
    }

    #[test]
    fn only_https_and_loopback_http_urls_are_opened() {
        for accepted in [
            "https://github.com/example/app.tar.gz",
            "http://127.0.0.1:8080/artifact",
            "http://127.0.0.1/artifact",
            "http://localhost:9/x",
            "http://[::1]:80/x",
        ] {
            assert!(valid_fetch_url(accepted), "{accepted}");
        }
        for refused in [
            "http://example.com/artifact",
            "ftp://127.0.0.1/artifact",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!valid_fetch_url(refused), "{refused}");
        }
        let (_root, path) = destination();
        let pinned = RemoteArtifact {
            url: "http://example.com/artifact".to_owned(),
            sha256: digest(b"x"),
            bytes: 1,
        };
        assert!(matches!(
            fetch_to_file(&pinned, &path, &ReqwestFetcher::default()).unwrap_err(),
            FetchFailure::UrlInvalid(_)
        ));
    }
}
