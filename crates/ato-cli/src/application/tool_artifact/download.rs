//! Streaming HTTP download with sha256 verification.
//!
//! No `curl`/`wget` shell-out. The body streams through
//! [`sha2::Sha256`] as it is written to disk, so a tampered byte at the
//! tail of a 30 MB JAR fails the same way as a tampered byte in the
//! header — no need to re-read the file after writing.
//!
//! The production [`ReqwestDownloader`] uses async `reqwest::Client`
//! and bridges to the sync [`Downloader`] trait through the same
//! pattern as `block_on_runtime_fetch` (`crates/ato-cli/src/adapters/
//! runtime/manager.rs`). This avoids `reqwest::blocking::Client` whose
//! constructor spawns + drops an inner tokio runtime — fatal when the
//! caller is already inside a tokio runtime, which is exactly where
//! the orchestrator's `start_one` runs us from.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};

use super::error::ToolArtifactError;
use super::manifest::ToolArtifactManifest;

/// Streaming HTTP download with checksum verification.
///
/// The downloader is a trait so tests can substitute a local-file
/// transport without spinning up a real HTTP server. The production
/// impl is [`ReqwestDownloader`].
pub trait Downloader {
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<DownloadOutcome, anyhow::Error>;
}

#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub bytes_written: u64,
    pub sha256_hex: String,
}

/// Default production downloader. Uses async `reqwest::Client`; the
/// sync [`Downloader::fetch_to`] bridges to it via a fresh
/// `current_thread` runtime on a dedicated OS thread. This is the
/// same pattern as `block_on_runtime_fetch`. Avoiding
/// `reqwest::blocking::Client` is deliberate — that constructor
/// internally creates and drops a tokio runtime, which panics with
/// "Cannot drop a runtime in a context where blocking is not allowed"
/// when called from inside the orchestrator's outer runtime.
///
/// No proxy bypass; tool artifacts are typically Maven Central or
/// GitHub releases, which do not need internal-network proxy
/// carve-outs.
#[derive(Clone)]
pub struct ReqwestDownloader {
    client: reqwest::Client,
}

impl Default for ReqwestDownloader {
    fn default() -> Self {
        // reqwest::Client::builder() does NOT spawn an inner runtime,
        // so this is safe from any context (sync or async).
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .https_only(false)
            .user_agent(concat!("ato-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("async reqwest client");
        Self { client }
    }
}

impl ReqwestDownloader {
    /// Async hot loop that streams the body to disk, hashing as it
    /// goes. Public so tests can drive it from an existing async
    /// runtime; production code reaches it through the sync
    /// [`Downloader::fetch_to`] bridge.
    pub async fn fetch_to_async(
        &self,
        url: String,
        dest: std::path::PathBuf,
    ) -> Result<DownloadOutcome, anyhow::Error> {
        use tokio::io::AsyncWriteExt;
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("HTTP request to {url} failed"))?
            .error_for_status()
            .with_context(|| format!("HTTP {url} returned a non-success status"))?;
        let mut file = tokio::fs::File::create(&dest)
            .await
            .with_context(|| format!("create download dest at {}", dest.display()))?;
        let mut hasher = Sha256::new();
        let mut bytes_written: u64 = 0;
        let mut stream = response.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("read body chunk from {url}"))?;
            file.write_all(&chunk)
                .await
                .with_context(|| format!("write body to {}", dest.display()))?;
            hasher.update(&chunk);
            bytes_written += chunk.len() as u64;
        }
        file.flush().await.context("flush downloaded file")?;
        Ok(DownloadOutcome {
            bytes_written,
            sha256_hex: hex::encode(hasher.finalize()),
        })
    }
}

impl Downloader for ReqwestDownloader {
    fn fetch_to(&self, url: &str, dest: &Path) -> Result<DownloadOutcome, anyhow::Error> {
        let url = url.to_string();
        let dest = dest.to_path_buf();
        let client = self.clone();
        // We may be inside a tokio runtime (orchestrator path) or not
        // (CLI sub-command path). In either case we need a fresh
        // `current_thread` runtime so the body stream is driven to
        // completion without re-entering the outer runtime, and we
        // need that runtime to live on a dedicated OS thread so its
        // Drop happens outside any async context. This mirrors
        // `block_on_runtime_fetch`.
        let do_fetch = move || -> Result<DownloadOutcome, anyhow::Error> {
            std::thread::spawn(move || -> Result<DownloadOutcome, anyhow::Error> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("build tool-artifact downloader runtime")?;
                runtime.block_on(client.fetch_to_async(url, dest))
            })
            .join()
            .map_err(|_| anyhow!("tool-artifact downloader thread panicked"))?
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => match handle.runtime_flavor() {
                // Wrap in block_in_place on multi-thread so other
                // tasks on the outer runtime can keep making progress
                // on sibling worker threads while this caller blocks
                // on .join().
                tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(do_fetch),
                // current_thread: don't wrap. block_in_place panics
                // on that flavor; running join() directly stalls the
                // single worker for the duration of the download —
                // acceptable because the caller is already
                // synchronously awaiting the artifact.
                _ => do_fetch(),
            },
            Err(_) => do_fetch(),
        }
    }
}

/// Download `manifest.url` to `dest`, streaming through sha256, and
/// verify the digest against `manifest.sha256` before returning. On
/// mismatch the partial file is left in place for diagnosis; the
/// caller is responsible for removing it (the resolver does this by
/// downloading into a temp dir that is unconditionally cleaned).
pub fn fetch_and_verify(
    downloader: &dyn Downloader,
    manifest: &ToolArtifactManifest,
    dest: &Path,
) -> Result<(), ToolArtifactError> {
    let outcome = downloader.fetch_to(&manifest.url, dest).map_err(|err| {
        ToolArtifactError::ArtifactDownloadFailed {
            name: manifest.name.clone(),
            url: manifest.url.clone(),
            source: err,
        }
    })?;
    if outcome.sha256_hex != manifest.sha256 {
        return Err(ToolArtifactError::ArtifactChecksumMismatch {
            name: manifest.name.clone(),
            url: manifest.url.clone(),
            expected: manifest.sha256.clone(),
            got: outcome.sha256_hex,
        });
    }
    if outcome.bytes_written == 0 {
        return Err(ToolArtifactError::ArtifactDownloadFailed {
            name: manifest.name.clone(),
            url: manifest.url.clone(),
            source: anyhow!("empty body"),
        });
    }
    Ok(())
}

/// Compute sha256 of an existing file. Used to verify the inner member
/// of a `jar+txz` archive after extracting it from the wrapper.
pub fn sha256_of_file(path: &Path) -> Result<String, anyhow::Error> {
    let mut file =
        File::open(path).with_context(|| format!("open {} for hashing", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("read {} for hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Test-only file:// downloader. Lets the resolve integration test
    //! exercise the full pipeline without a live HTTP listener.

    use super::*;
    use std::fs;

    pub struct LocalFileDownloader;

    impl Downloader for LocalFileDownloader {
        fn fetch_to(&self, url: &str, dest: &Path) -> Result<DownloadOutcome, anyhow::Error> {
            // url shape: "test-local://<absolute path>"
            let path = url
                .strip_prefix("test-local://")
                .ok_or_else(|| anyhow!("LocalFileDownloader needs test-local:// url, got {url}"))?;
            let bytes = fs::read(path).with_context(|| format!("read source {}", path))?;
            fs::write(dest, &bytes).with_context(|| format!("write dest {}", dest.display()))?;
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            Ok(DownloadOutcome {
                bytes_written: bytes.len() as u64,
                sha256_hex: hex::encode(hasher.finalize()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::LocalFileDownloader;
    use super::*;
    use crate::application::tool_artifact::manifest::{
        ArchiveFormat, ArtifactLayout, ToolArtifactManifest,
    };

    fn fixture_manifest(url: String, sha256: String) -> ToolArtifactManifest {
        ToolArtifactManifest {
            schema_version: "1".into(),
            name: "demo".into(),
            version: "1.0.0".into(),
            platform: "linux-x86_64".into(),
            url,
            sha256,
            archive_format: ArchiveFormat::TarGz,
            inner_member: None,
            inner_sha256: None,
            strip_prefix: None,
            layout: ArtifactLayout {
                bin_dir: "bin".into(),
                lib_dir: "lib".into(),
                share_dir: "share".into(),
            },
            provides: vec!["demo".into()],
        }
    }

    #[test]
    fn local_downloader_reports_correct_sha256() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        std::fs::write(&src, b"hello world").unwrap();
        let dest = tmp.path().join("dest.bin");

        let url = format!("test-local://{}", src.display());
        let outcome = LocalFileDownloader
            .fetch_to(&url, &dest)
            .expect("local fetch");
        assert_eq!(outcome.bytes_written, 11);
        assert_eq!(
            outcome.sha256_hex,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello world");
    }

    #[test]
    fn fetch_and_verify_rejects_checksum_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        std::fs::write(&src, b"hello world").unwrap();
        let dest = tmp.path().join("dest.bin");

        let manifest = fixture_manifest(
            format!("test-local://{}", src.display()),
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
        );
        let err = fetch_and_verify(&LocalFileDownloader, &manifest, &dest).unwrap_err();
        match err {
            ToolArtifactError::ArtifactChecksumMismatch { expected, got, .. } => {
                assert!(expected.starts_with("deadbeef"));
                assert_eq!(
                    got,
                    "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
                );
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn fetch_and_verify_accepts_matching_checksum() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.bin");
        std::fs::write(&src, b"hello world").unwrap();
        let dest = tmp.path().join("dest.bin");

        let manifest = fixture_manifest(
            format!("test-local://{}", src.display()),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9".into(),
        );
        fetch_and_verify(&LocalFileDownloader, &manifest, &dest).expect("must accept matching");
    }

    #[test]
    fn sha256_of_file_matches_known_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f");
        std::fs::write(&path, b"hello world").unwrap();
        assert_eq!(
            sha256_of_file(&path).unwrap(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
