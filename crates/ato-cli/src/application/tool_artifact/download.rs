//! Streaming HTTP download with sha256 verification.
//!
//! No `curl`/`wget` shell-out. The body streams through
//! [`sha2::Sha256`] as it is written to disk, so a tampered byte at the
//! tail of a 30 MB JAR fails the same way as a tampered byte in the
//! header — no need to re-read the file after writing.

use std::fs::File;
use std::io::{Read, Write};
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
    fn fetch_to(
        &self,
        url: &str,
        dest: &Path,
    ) -> Result<DownloadOutcome, anyhow::Error>;
}

#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub bytes_written: u64,
    pub sha256_hex: String,
}

/// Default production downloader. Uses `reqwest::blocking` with TLS
/// off-by-default for `http://` and rustls for `https://`. No proxy
/// bypass; tool artifacts are typically Maven Central or GitHub
/// releases, which do not need internal-network proxy carve-outs.
pub struct ReqwestDownloader {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestDownloader {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            // Tool artifacts are large (30+ MB for postgres). Generous
            // overall timeout, but keep the connect timeout tight so a
            // misbehaving CDN fails fast.
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(600))
            .https_only(false) // some mirrors ship over http; manifest validates http(s)
            .user_agent(concat!("ato-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("blocking reqwest client");
        Self { client }
    }
}

impl Downloader for ReqwestDownloader {
    fn fetch_to(
        &self,
        url: &str,
        dest: &Path,
    ) -> Result<DownloadOutcome, anyhow::Error> {
        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("HTTP request to {url} failed"))?
            .error_for_status()
            .with_context(|| format!("HTTP {url} returned a non-success status"))?;

        let mut file = File::create(dest)
            .with_context(|| format!("create download dest at {}", dest.display()))?;
        let mut hasher = Sha256::new();
        let mut bytes_written: u64 = 0;
        let mut buf = [0u8; 64 * 1024];
        let mut reader: Box<dyn Read> = Box::new(response);
        loop {
            let n = reader
                .read(&mut buf)
                .with_context(|| format!("read body from {url}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])
                .with_context(|| format!("write body to {}", dest.display()))?;
            hasher.update(&buf[..n]);
            bytes_written += n as u64;
        }
        file.flush().context("flush downloaded file")?;
        Ok(DownloadOutcome {
            bytes_written,
            sha256_hex: hex::encode(hasher.finalize()),
        })
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
        fn fetch_to(
            &self,
            url: &str,
            dest: &Path,
        ) -> Result<DownloadOutcome, anyhow::Error> {
            // url shape: "test-local://<absolute path>"
            let path = url
                .strip_prefix("test-local://")
                .ok_or_else(|| anyhow!("LocalFileDownloader needs test-local:// url, got {url}"))?;
            let bytes = fs::read(path)
                .with_context(|| format!("read source {}", path))?;
            fs::write(dest, &bytes)
                .with_context(|| format!("write dest {}", dest.display()))?;
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
