use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use tracing::{debug, info};

use super::RuntimeFetcher;
use crate::error::{CapsuleError, Result};

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
    fetchers.insert("llamacpp", Box::new(LlamaCppFetcher));
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

        provider
            .reporter
            .notify(format!("⬇️  Downloading Python {} runtime...", version))
            .await?;

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let download_url = RuntimeFetcher::get_python_download_url(version, &os, &arch)?;

        debug!("Fetching from: {}", download_url);

        let expected_sha256 = provider
            .fetch_expected_sha256(&(download_url.clone() + ".sha256"), None)
            .await?;

        let archive_path = provider
            .cache_dir()
            .join(format!("python-{}.tar.gz", version));
        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        provider.verify_sha256_of_file(&archive_path, &expected_sha256)?;

        let temp_dir = provider.cache_dir().join(format!("tmp-python-{}", version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        provider
            .reporter
            .notify(format!("📦 Extracting Python {} runtime...", version))
            .await?;
        RuntimeFetcher::extract_archive_from_file(&archive_path, &temp_dir)?;

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to move extracted runtime to cache: {}", e))
        })?;

        let _ = std::fs::remove_file(&archive_path);

        provider
            .reporter
            .notify(format!(
                "✓ Python {} installed at {:?}",
                version, runtime_dir
            ))
            .await?;
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

        provider
            .reporter
            .notify(format!("⬇️  Downloading Node {} runtime...", version))
            .await?;

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let full_version = RuntimeFetcher::resolve_node_full_version(version).await?;

        let (filename, is_zip) = RuntimeFetcher::node_artifact_filename(&full_version, &os, &arch)?;
        let download_url = format!("https://nodejs.org/dist/v{}/{}", full_version, filename);

        debug!("Fetching from: {}", download_url);

        let archive_path = provider.cache_dir().join(format!(
            "node-{}{}",
            full_version,
            if is_zip { ".zip" } else { ".tar.gz" }
        ));

        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        let expected_sha256 = provider
            .fetch_expected_sha256(
                &format!("https://nodejs.org/dist/v{}/SHASUMS256.txt", full_version),
                Some(&filename),
            )
            .await?;

        provider.verify_sha256_of_file(&archive_path, &expected_sha256)?;

        let temp_dir = provider
            .cache_dir()
            .join(format!("tmp-node-{}", full_version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        provider
            .reporter
            .notify(format!("📦 Extracting Node {} runtime...", full_version))
            .await?;
        if is_zip {
            RuntimeFetcher::extract_zip_from_file(&archive_path, &temp_dir)?;
        } else {
            RuntimeFetcher::extract_archive_from_file(&archive_path, &temp_dir)?;
        }

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to move extracted runtime to cache: {}", e))
        })?;

        let _ = std::fs::remove_file(&archive_path);

        provider
            .reporter
            .notify(format!(
                "✓ Node {} installed at {:?}",
                full_version, runtime_dir
            ))
            .await?;
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

        provider
            .reporter
            .notify(format!("⬇️  Downloading Deno {} runtime...", version))
            .await?;

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let filename = deno_artifact_filename(&os, &arch)?;
        let download_url = format!(
            "https://github.com/denoland/deno/releases/download/v{}/{}",
            version, filename
        );

        debug!("Fetching from: {}", download_url);

        let archive_path = provider.cache_dir().join(format!("deno-{}.zip", version));

        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        let expected_sha256 = match resolve_deno_sha256(provider, version, &filename).await {
            Ok(sum) => sum,
            Err(CapsuleError::NotFound(_)) => {
                provider
                    .reporter
                    .warn(format!(
                        "⚠️  Deno checksum asset not found for v{}; falling back to TOFU hash",
                        version
                    ))
                    .await?;
                sha256_of_file(&archive_path)?
            }
            Err(err) => return Err(err),
        };

        provider.verify_sha256_of_file(&archive_path, &expected_sha256)?;

        let temp_dir = provider.cache_dir().join(format!("tmp-deno-{}", version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        provider
            .reporter
            .notify(format!("📦 Extracting Deno {} runtime...", version))
            .await?;
        RuntimeFetcher::extract_zip_from_file(&archive_path, &temp_dir)?;

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to move extracted runtime to cache: {}", e))
        })?;

        let _ = std::fs::remove_file(&archive_path);

        provider
            .reporter
            .notify(format!("✓ Deno {} installed at {:?}", version, runtime_dir))
            .await?;
        Ok(runtime_dir)
    }
}

fn deno_artifact_filename(os: &str, arch: &str) -> Result<String> {
    let target = match (os, arch) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => {
            return Err(CapsuleError::Pack(format!(
                "Unsupported Deno platform: {} {}",
                os, arch
            )));
        }
    };
    Ok(format!("deno-{}.zip", target))
}

async fn resolve_deno_sha256(
    provider: &RuntimeFetcher,
    version: &str,
    filename: &str,
) -> Result<String> {
    let candidates = [
        (
            format!(
                "https://github.com/denoland/deno/releases/download/v{}/{}.sha256sum",
                version, filename
            ),
            None,
        ),
        (
            format!(
                "https://github.com/denoland/deno/releases/download/v{}/{}.sha256",
                version, filename
            ),
            None,
        ),
        (
            format!(
                "https://github.com/denoland/deno/releases/download/v{}/SHASUMS256.txt",
                version
            ),
            Some(filename),
        ),
    ];

    let mut last_not_found = None;
    for (checksum_url, hint) in candidates {
        match provider.fetch_expected_sha256(&checksum_url, hint).await {
            Ok(sum) => return Ok(sum),
            Err(CapsuleError::NotFound(_)) => {
                last_not_found = Some(checksum_url);
            }
            Err(err) => return Err(err),
        }
    }

    let detail = last_not_found.unwrap_or_else(|| "Deno checksum".to_string());
    Err(CapsuleError::NotFound(detail))
}

fn sha256_of_file(path: &std::path::Path) -> Result<String> {
    let mut file = File::open(path)
        .map_err(|e| CapsuleError::Pack(format!("Failed to open downloaded file: {}", e)))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|e| CapsuleError::Pack(format!("Failed to read downloaded file: {}", e)))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
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

        provider
            .reporter
            .notify(format!("⬇️  Downloading Bun {} runtime...", version))
            .await?;

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let full_version = RuntimeFetcher::normalize_semverish(version);

        let download_url = format!(
            "https://github.com/oven-sh/bun/releases/download/bun-v{}/bun-{}-{}.zip",
            full_version, os, arch
        );

        debug!("Fetching from: {}", download_url);

        let archive_path = provider
            .cache_dir()
            .join(format!("bun-{}.zip", full_version));

        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        let expected_sha256 = provider
            .fetch_expected_sha256(&(download_url.clone() + ".sha256"), None)
            .await?;

        provider.verify_sha256_of_file(&archive_path, &expected_sha256)?;

        let temp_dir = provider
            .cache_dir()
            .join(format!("tmp-bun-{}", full_version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        provider
            .reporter
            .notify(format!("📦 Extracting Bun {} runtime...", full_version))
            .await?;
        RuntimeFetcher::extract_zip_from_file(&archive_path, &temp_dir)?;

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to move extracted runtime to cache: {}", e))
        })?;

        let _ = std::fs::remove_file(&archive_path);

        provider
            .reporter
            .notify(format!(
                "✓ Bun {} installed at {:?}",
                full_version, runtime_dir
            ))
            .await?;
        Ok(runtime_dir)
    }
}

pub(crate) struct LlamaCppFetcher;

#[async_trait]
impl ToolchainFetcher for LlamaCppFetcher {
    fn language(&self) -> &'static str {
        "llamacpp"
    }

    async fn download_runtime(
        &self,
        provider: &RuntimeFetcher,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let runtime_dir = provider.get_runtime_path("llamacpp", version);
        if runtime_dir.exists() {
            info!("✓ llama.cpp {} already cached", version);
            return Ok(runtime_dir);
        }

        provider
            .reporter
            .notify(format!(
                "⬇️  Downloading llama.cpp {} (llama-server)...",
                version
            ))
            .await?;

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let (filename, is_zip) = llama_cpp_artifact_filename(version, &os, &arch)?;
        // The release tag IS the version (e.g. "b4231") — no "v" prefix.
        let download_url = format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{}/{}",
            version, filename
        );

        debug!("Fetching from: {}", download_url);

        let archive_path = provider.cache_dir().join(format!(
            "llamacpp-{}{}",
            version,
            if is_zip { ".zip" } else { ".tar.gz" }
        ));

        provider
            .download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        // llama.cpp releases ship no per-asset checksum sidecar, so pin integrity
        // on the immutable release tag (`version`) + HTTPS and record a TOFU hash.
        let expected_sha256 = sha256_of_file(&archive_path)?;
        provider.verify_sha256_of_file(&archive_path, &expected_sha256)?;

        let temp_dir = provider
            .cache_dir()
            .join(format!("tmp-llamacpp-{}", version));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        std::fs::create_dir_all(&temp_dir)?;

        provider
            .reporter
            .notify(format!("📦 Extracting llama.cpp {}...", version))
            .await?;
        if is_zip {
            RuntimeFetcher::extract_zip_from_file(&archive_path, &temp_dir)?;
        } else {
            // tar.gz preserves the executable bit on llama-server and ships the
            // sibling shared libs (libllama, libggml) it dlopen/rpath-loads.
            RuntimeFetcher::extract_archive_from_file(&archive_path, &temp_dir)?;
        }

        // llama.cpp archives nest everything under a single `llama-<tag>/` dir.
        // Flatten it so `llama-server` sits at a deterministic path
        // (`<runtime_dir>/llama-server`) next to its sibling shared libs, which
        // lets the launcher resolve the engine path without a filesystem search.
        flatten_single_subdir(&temp_dir)?;

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to move extracted runtime to cache: {}", e))
        })?;

        let _ = std::fs::remove_file(&archive_path);

        provider
            .reporter
            .notify(format!(
                "✓ llama.cpp {} installed at {:?}",
                version, runtime_dir
            ))
            .await?;
        Ok(runtime_dir)
    }
}

/// When `dir` contains exactly one entry and it is a directory, move that
/// directory's contents up into `dir` and remove the now-empty wrapper. This
/// strips the single `llama-<tag>/` leading component that llama.cpp archives
/// use, leaving `llama-server` at `dir`'s root.
fn flatten_single_subdir(dir: &std::path::Path) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| CapsuleError::Pack(format!("read extracted dir: {e}")))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect::<Vec<_>>();
    if entries.len() != 1 || !entries[0].is_dir() {
        return Ok(());
    }
    let inner = entries.remove(0);
    for child in std::fs::read_dir(&inner)
        .map_err(|e| CapsuleError::Pack(format!("read nested dir: {e}")))?
        .filter_map(|e| e.ok())
    {
        let from = child.path();
        let to = dir.join(child.file_name());
        std::fs::rename(&from, &to)
            .map_err(|e| CapsuleError::Pack(format!("flatten {from:?} -> {to:?}: {e}")))?;
    }
    let _ = std::fs::remove_dir_all(&inner);
    Ok(())
}

/// Map host platform → llama.cpp release asset (name, is_zip). Inc2 selects the
/// default CPU/Metal build; GPU (CUDA/ROCm/Vulkan) variant selection is Inc4.
fn llama_cpp_artifact_filename(version: &str, os: &str, arch: &str) -> Result<(String, bool)> {
    let (slug, is_zip) = match (os, arch) {
        ("macos", "aarch64") => ("macos-arm64", false),
        ("macos", "x86_64") => ("macos-x64", false),
        ("linux", "x86_64") => ("ubuntu-x64", false),
        ("linux", "aarch64") => ("ubuntu-arm64", false),
        ("windows", "x86_64") => ("win-cpu-x64", true),
        ("windows", "aarch64") => ("win-cpu-arm64", true),
        _ => {
            return Err(CapsuleError::Pack(format!(
                "Unsupported llama.cpp platform: {} {}",
                os, arch
            )));
        }
    };
    let ext = if is_zip { "zip" } else { "tar.gz" };
    Ok((format!("llama-{}-bin-{}.{}", version, slug, ext), is_zip))
}

#[cfg(test)]
mod tests {
    use super::{deno_artifact_filename, llama_cpp_artifact_filename};

    #[test]
    fn llama_cpp_artifact_filename_maps_platforms() {
        assert_eq!(
            llama_cpp_artifact_filename("b4231", "macos", "aarch64").unwrap(),
            ("llama-b4231-bin-macos-arm64.tar.gz".to_string(), false)
        );
        assert_eq!(
            llama_cpp_artifact_filename("b4231", "linux", "x86_64").unwrap(),
            ("llama-b4231-bin-ubuntu-x64.tar.gz".to_string(), false)
        );
        assert_eq!(
            llama_cpp_artifact_filename("b4231", "windows", "x86_64").unwrap(),
            ("llama-b4231-bin-win-cpu-x64.zip".to_string(), true)
        );
        assert!(llama_cpp_artifact_filename("b4231", "plan9", "x86_64").is_err());
    }

    #[test]
    fn deno_artifact_filename_uses_release_target_triplets() {
        assert_eq!(
            deno_artifact_filename("macos", "aarch64").unwrap(),
            "deno-aarch64-apple-darwin.zip"
        );
        assert_eq!(
            deno_artifact_filename("linux", "x86_64").unwrap(),
            "deno-x86_64-unknown-linux-gnu.zip"
        );
        assert_eq!(
            deno_artifact_filename("windows", "x86_64").unwrap(),
            "deno-x86_64-pc-windows-msvc.zip"
        );
    }
}
