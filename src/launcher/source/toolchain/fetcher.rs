use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info};

use super::RuntimeFetcher;

/// A checksum manifest URL and (optional) detached signature URL.
///
/// Phase 1 reads the unsigned checksum text to extract the expected sha256.
/// Phase 2 will verify `signature_url` before trusting the checksum contents.
#[derive(Debug, Clone)]
struct ChecksumManifest {
    unsigned_url: String,
    #[allow(dead_code)]
    signature_url: Option<String>,
}

impl ChecksumManifest {
    fn new(unsigned_url: String, signature_url: Option<String>) -> Self {
        Self {
            unsigned_url,
            signature_url,
        }
    }
}

#[async_trait]
pub(crate) trait ToolchainFetcher: Send + Sync {
    fn language(&self) -> &'static str;

    async fn download_runtime(
        &self,
        provider: &RuntimeFetcher,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf>;
}

pub(crate) fn default_fetchers() -> HashMap<&'static str, Box<dyn ToolchainFetcher>> {
    let mut fetchers: HashMap<&'static str, Box<dyn ToolchainFetcher>> = HashMap::new();
    fetchers.insert("python", Box::new(PythonFetcher));
    fetchers.insert("node", Box::new(NodeFetcher));
    fetchers.insert("deno", Box::new(DenoFetcher));
    fetchers.insert("bun", Box::new(BunFetcher));
    fetchers
}

pub(crate) struct PythonFetcher;

#[async_trait]
impl ToolchainFetcher for PythonFetcher {
    fn language(&self) -> &'static str {
        "python"
    }

    async fn download_runtime(
        &self,
        provider: &RuntimeFetcher,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let runtime_dir = provider.get_runtime_path("python", version);

        if runtime_dir.exists() {
            info!("✓ Python {} already cached", version);
            return Ok(runtime_dir);
        }

        toolchain_out!("⬇️  Downloading Python {} runtime...", version);

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let download_url = RuntimeFetcher::get_python_download_url(version, &os, &arch)?;

        debug!("Fetching from: {}", download_url);

        let expected_sha256 = provider
            .fetch_expected_sha256(&(download_url.clone() + ".sha256"), None)
            .await
            .context("Failed to fetch expected sha256")?;

        let archive_path = provider
            .cache_dir()
            .join(format!("python-{}.tar.gz", version));
        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        provider
            .verify_sha256_of_file(&archive_path, &expected_sha256)
            .context("Downloaded Python runtime failed sha256 verification")?;

        let temp_dir = provider.cache_dir().join(format!("tmp-python-{}", version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        toolchain_out!("📦 Extracting Python {} runtime...", version);
        RuntimeFetcher::extract_archive_from_file(&archive_path, &temp_dir)?;

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir)
            .context("Failed to move extracted runtime to cache")?;

        let _ = std::fs::remove_file(&archive_path);

        toolchain_out!("✓ Python {} installed at {:?}", version, runtime_dir);
        Ok(runtime_dir)
    }
}

pub(crate) struct NodeFetcher;

#[async_trait]
impl ToolchainFetcher for NodeFetcher {
    fn language(&self) -> &'static str {
        "node"
    }

    async fn download_runtime(
        &self,
        provider: &RuntimeFetcher,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let runtime_dir = provider.get_runtime_path("node", version);
        if runtime_dir.exists() {
            info!("✓ Node {} already cached", version);
            return Ok(runtime_dir);
        }

        toolchain_out!("⬇️  Downloading Node {} runtime...", version);

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let full_version = RuntimeFetcher::resolve_node_full_version(version).await?;
        let (filename, is_zip) = RuntimeFetcher::node_artifact_filename(&full_version, &os, &arch)?;

        let base_url = format!("https://nodejs.org/dist/v{}", full_version);
        let download_url = format!("{}/{}", base_url, filename);

        let shasums = ChecksumManifest::new(
            format!("{}/SHASUMS256.txt", base_url),
            Some(format!("{}/SHASUMS256.txt.asc", base_url)),
        );

        let expected_sha256 = provider
            .fetch_expected_sha256(&shasums.unsigned_url, Some(&filename))
            .await
            .context("Failed to fetch Node SHASUMS256.txt")?;

        let archive_path = if is_zip {
            provider.cache_dir().join(format!("node-{}.zip", version))
        } else {
            provider
                .cache_dir()
                .join(format!("node-{}.tar.gz", version))
        };

        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        provider
            .verify_sha256_of_file(&archive_path, &expected_sha256)
            .context("Downloaded Node runtime failed sha256 verification")?;

        let temp_dir = provider.cache_dir().join(format!("tmp-node-{}", version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        toolchain_out!("📦 Extracting Node {} runtime...", version);
        if is_zip {
            RuntimeFetcher::extract_zip_from_file(&archive_path, &temp_dir)?;
        } else {
            RuntimeFetcher::extract_archive_from_file(&archive_path, &temp_dir)?;
        }

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir)
            .context("Failed to move extracted Node runtime")?;

        let _ = std::fs::remove_file(&archive_path);
        toolchain_out!("✓ Node {} installed at {:?}", version, runtime_dir);
        Ok(runtime_dir)
    }
}

pub(crate) struct DenoFetcher;

#[async_trait]
impl ToolchainFetcher for DenoFetcher {
    fn language(&self) -> &'static str {
        "deno"
    }

    async fn download_runtime(
        &self,
        provider: &RuntimeFetcher,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let runtime_dir = provider.get_runtime_path("deno", version);
        if runtime_dir.exists() {
            info!("✓ Deno {} already cached", version);
            return Ok(runtime_dir);
        }

        toolchain_out!("⬇️  Downloading Deno {} runtime...", version);

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let deno_version = RuntimeFetcher::normalize_semverish(version);

        let target = match (os.as_str(), arch.as_str()) {
            ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
            ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
            _ => anyhow::bail!("Unsupported platform for Deno: {} {}", os, arch),
        };

        let filename = format!("deno-{}.zip", target);
        let download_url = format!(
            "https://github.com/denoland/deno/releases/download/v{}/{}",
            deno_version, filename
        );
        let sha_url = format!("{}.sha256sum", download_url);

        let expected_sha256 = provider
            .fetch_expected_sha256(&sha_url, Some(&filename))
            .await
            .context("Failed to fetch Deno sha256sum")?;

        let archive_path = provider.cache_dir().join(format!("deno-{}.zip", version));
        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        provider
            .verify_sha256_of_file(&archive_path, &expected_sha256)
            .context("Downloaded Deno runtime failed sha256 verification")?;

        let temp_dir = provider.cache_dir().join(format!("tmp-deno-{}", version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        toolchain_out!("📦 Extracting Deno {} runtime...", version);
        RuntimeFetcher::extract_zip_from_file(&archive_path, &temp_dir)?;

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir)
            .context("Failed to move extracted Deno runtime")?;

        let _ = std::fs::remove_file(&archive_path);
        toolchain_out!("✓ Deno {} installed at {:?}", version, runtime_dir);
        Ok(runtime_dir)
    }
}

pub(crate) struct BunFetcher;

#[async_trait]
impl ToolchainFetcher for BunFetcher {
    fn language(&self) -> &'static str {
        "bun"
    }

    async fn download_runtime(
        &self,
        provider: &RuntimeFetcher,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let runtime_dir = provider.get_runtime_path("bun", version);
        if runtime_dir.exists() {
            info!("✓ Bun {} already cached", version);
            return Ok(runtime_dir);
        }

        toolchain_out!("⬇️  Downloading Bun {} runtime...", version);

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let bun_version = RuntimeFetcher::normalize_semverish(version);

        let filename = match (os.as_str(), arch.as_str()) {
            ("macos", "x86_64") => "bun-darwin-x64.zip".to_string(),
            ("macos", "aarch64") => "bun-darwin-aarch64.zip".to_string(),
            ("linux", "x86_64") => "bun-linux-x64.zip".to_string(),
            ("linux", "aarch64") => "bun-linux-aarch64.zip".to_string(),
            ("windows", "x86_64") => "bun-windows-x64.zip".to_string(),
            _ => anyhow::bail!("Unsupported platform for Bun: {} {}", os, arch),
        };

        let base_url = format!(
            "https://github.com/oven-sh/bun/releases/download/bun-v{}",
            bun_version
        );
        let download_url = format!("{}/{}", base_url, filename);

        let shasums = ChecksumManifest::new(
            format!("{}/SHASUMS256.txt", base_url),
            Some(format!("{}/SHASUMS256.txt.asc", base_url)),
        );

        let expected_sha256 = provider
            .fetch_expected_sha256(&shasums.unsigned_url, Some(&filename))
            .await
            .context("Failed to fetch Bun SHASUMS256.txt")?;

        let archive_path = provider.cache_dir().join(format!("bun-{}.zip", version));
        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        provider
            .verify_sha256_of_file(&archive_path, &expected_sha256)
            .context("Downloaded Bun runtime failed sha256 verification")?;

        let temp_dir = provider.cache_dir().join(format!("tmp-bun-{}", version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        toolchain_out!("📦 Extracting Bun {} runtime...", version);
        RuntimeFetcher::extract_zip_from_file(&archive_path, &temp_dir)?;

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir).context("Failed to move extracted Bun runtime")?;

        let _ = std::fs::remove_file(&archive_path);
        toolchain_out!("✓ Bun {} installed at {:?}", version, runtime_dir);
        Ok(runtime_dir)
    }
}
