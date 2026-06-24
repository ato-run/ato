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
    fetchers.insert("sglang", Box::new(SgLangFetcher));
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
        // The dispatch `version` is the cache KEY: `<build-tag>` for the default
        // build or `<build-tag>@<variant>` (e.g. `b9754@vulkan`). The cache dir
        // uses the full key (so variants never collide); the download URL uses
        // the real build tag + the variant's asset slug.
        let (build_tag, variant) = parse_llamacpp_key(version);

        // Defense-in-depth: the build tag is interpolated into the download URL,
        // archive name, and cache path. Manifest validation enforces this, but
        // reject unsafe values here too (this runs under the install lock).
        if !crate::foundation::types::manifest::is_safe_engine_version(build_tag) {
            return Err(CapsuleError::Pack(format!(
                "unsafe llama.cpp engine_version {build_tag:?} \
                 (expected a build tag / version id)"
            )));
        }

        let runtime_dir = provider.get_runtime_path("llamacpp", version);
        // Reuse the cache only when it is COMPLETE (the canonical server binary
        // is present + executable). A partial/corrupt dir — e.g. an earlier
        // interrupted extract that left an empty or half-written directory — is
        // discarded and rebuilt rather than failing forever.
        if llamacpp_cache_is_valid(&runtime_dir) {
            info!("✓ llama.cpp {} already cached", version);
            return Ok(runtime_dir);
        }
        if runtime_dir.exists() {
            info!("llama.cpp {} cache is incomplete; rebuilding", version);
            std::fs::remove_dir_all(&runtime_dir)?;
        }

        provider
            .reporter
            .notify(format!(
                "⬇️  Downloading llama.cpp {} (llama-server)...",
                version
            ))
            .await?;

        let (os, arch) = RuntimeFetcher::detect_platform()?;
        let (filename, is_zip) = llama_cpp_artifact_filename(build_tag, &os, &arch, variant)?;
        // The release tag IS the build tag (e.g. "b4231") — no "v" prefix.
        let download_url = format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{}/{}",
            build_tag, filename
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

        // llama.cpp ships no per-asset checksum sidecar; integrity rests on the
        // immutable, official release tag (`version`) fetched over HTTPS. (No
        // upstream checksum is available to verify against, so none is claimed.)
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
        // Flatten it, then GUARANTEE the canonical server binary at
        // `<dir>/llama-server[.exe]` — the exact path the launcher resolves —
        // regardless of the archive's internal layout. This makes the
        // deterministic launch path a structural post-condition of the fetch.
        flatten_single_subdir(&temp_dir)?;
        ensure_canonical_llama_server(&temp_dir)?;

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

/// The Python version the SGLang managed venv is created with. SGLang + torch
/// cu124 wheels target CPython 3.12.
pub(crate) const SGLANG_VENV_PYTHON: &str = "3.12";

/// The PyTorch cu124 wheel index. SGLang's CUDA path is pinned to a CUDA-12
/// baseline (driver ≥ R550) rather than the bleeding-edge cu13 default for
/// reproducibility on a typical Ampere host.
pub(crate) const SGLANG_TORCH_INDEX_URL: &str = "https://download.pytorch.org/whl/cu124";

/// The pinned torch triple installed from [`SGLANG_TORCH_INDEX_URL`] BEFORE
/// sglang, so sglang's own resolver cannot silently upgrade torch off the cu124
/// index (the torch auto-upgrade footgun). Pinned to a cu124-compatible release.
pub(crate) const SGLANG_TORCH_PINS: &[&str] = &["torch==2.6.0", "torchvision", "torchaudio"];

/// The CUDA kernel packages SGLang needs on Ampere, installed alongside the
/// sglang wheel (matching cu124 wheels resolved from PyPI / the torch index).
pub(crate) const SGLANG_KERNEL_PINS: &[&str] = &["sgl-kernel", "flashinfer-python"];

/// The full pinned requirements set the SGLang fetcher installs for `version`
/// (the sglang wheel version). Returned as two install phases: torch first (off
/// the cu124 index), then sglang + its kernels. Surfaced for the doctor /
/// receipt and unit tests so the exact pins are inspectable.
pub(crate) fn sglang_requirements(version: &str) -> SgLangRequirements {
    SgLangRequirements {
        python: SGLANG_VENV_PYTHON,
        torch_index_url: SGLANG_TORCH_INDEX_URL,
        torch_pins: SGLANG_TORCH_PINS.iter().map(|s| s.to_string()).collect(),
        sglang_pin: format!("sglang=={version}"),
        kernel_pins: SGLANG_KERNEL_PINS.iter().map(|s| s.to_string()).collect(),
    }
}

/// The resolved, pinned SGLang requirements lock (see [`sglang_requirements`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SgLangRequirements {
    pub python: &'static str,
    pub torch_index_url: &'static str,
    pub torch_pins: Vec<String>,
    pub sglang_pin: String,
    pub kernel_pins: Vec<String>,
}

/// The canonical venv python path inside an SGLang runtime directory
/// (`<dir>/bin/python` on Unix, `<dir>/Scripts/python.exe` on Windows). This is
/// the exact path the launcher resolves as the server command, so both the
/// fetcher (post-condition) and the engine (`resolve_server_command`) derive it
/// from this single helper.
pub fn sglang_venv_python(runtime_dir: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        runtime_dir.join("Scripts").join("python.exe")
    } else {
        runtime_dir.join("bin").join("python")
    }
}

pub(crate) struct SgLangFetcher;

#[async_trait]
impl ToolchainFetcher for SgLangFetcher {
    fn language(&self) -> &'static str {
        "sglang"
    }

    async fn download_runtime(
        &self,
        provider: &RuntimeFetcher,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        // Defense-in-depth: the version is interpolated into the pip pin and the
        // cache path. Manifest validation enforces this, but reject unsafe values
        // here too (this runs under the install lock).
        if !crate::foundation::types::manifest::is_safe_engine_version(version) {
            return Err(CapsuleError::Pack(format!(
                "unsafe sglang engine_version {version:?} (expected a wheel version, e.g. \"0.4.10.post2\")"
            )));
        }

        let runtime_dir = provider.get_runtime_path("sglang", version);
        // Reuse only a COMPLETE venv (the canonical python is present). A partial
        // venv from an interrupted install is discarded and rebuilt.
        if sglang_venv_is_valid(&runtime_dir) {
            info!("✓ sglang {} venv already present", version);
            return Ok(runtime_dir);
        }
        if runtime_dir.exists() {
            info!("sglang {} venv is incomplete; rebuilding", version);
            std::fs::remove_dir_all(&runtime_dir)?;
        }

        // The whole install is built in a temp dir and atomically promoted, so a
        // failed pip never leaves a half-built venv that the exists-check trusts.
        let temp_dir = provider.cache_dir().join(format!("tmp-sglang-{version}"));
        if temp_dir.exists() {
            std::fs::remove_dir_all(&temp_dir)?;
        }
        if let Some(parent) = temp_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let uv = provider.ensure_uv(None).await?;
        let reqs = sglang_requirements(version);
        let venv_python = sglang_venv_python(&temp_dir);

        // 1. Create the CPython 3.12 venv.
        provider
            .reporter
            .notify(format!(
                "🐍 Creating sglang {version} venv (python {})...",
                reqs.python
            ))
            .await?;
        run_uv(
            &uv,
            &[
                "venv".to_string(),
                "--python".to_string(),
                reqs.python.to_string(),
                temp_dir.to_string_lossy().to_string(),
            ],
            show_progress,
        )
        .await?;

        // 2. torch first, off the cu124 index, so sglang's resolver can't bump it.
        provider
            .reporter
            .notify("⬇️  Installing torch (cu124)...".to_string())
            .await?;
        {
            let mut args = vec![
                "pip".to_string(),
                "install".to_string(),
                "--python".to_string(),
                venv_python.to_string_lossy().to_string(),
                "--index-url".to_string(),
                reqs.torch_index_url.to_string(),
            ];
            args.extend(reqs.torch_pins.iter().cloned());
            run_uv(&uv, &args, show_progress).await?;
        }

        // 3. sglang + its CUDA kernels (sgl-kernel / flashinfer).
        provider
            .reporter
            .notify(format!("⬇️  Installing {} + CUDA kernels...", reqs.sglang_pin))
            .await?;
        {
            let mut args = vec![
                "pip".to_string(),
                "install".to_string(),
                "--python".to_string(),
                venv_python.to_string_lossy().to_string(),
                // torch is already installed off cu124; let kernels resolve their
                // matching wheels from the same index + PyPI.
                "--extra-index-url".to_string(),
                reqs.torch_index_url.to_string(),
                reqs.sglang_pin.clone(),
            ];
            args.extend(reqs.kernel_pins.iter().cloned());
            run_uv(&uv, &args, show_progress).await?;
        }

        // Post-condition: the canonical venv python must exist before promotion.
        if !sglang_venv_is_valid(&temp_dir) {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(CapsuleError::Pack(format!(
                "sglang {version}: venv python missing after install at {:?}",
                sglang_venv_python(&temp_dir)
            )));
        }

        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)?;
        }
        std::fs::rename(&temp_dir, &runtime_dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to move sglang venv to cache: {e}"))
        })?;

        provider
            .reporter
            .notify(format!("✓ sglang {version} installed at {runtime_dir:?}"))
            .await?;
        Ok(runtime_dir)
    }
}

/// `true` when an SGLang venv directory is complete: the canonical venv python
/// exists. (The `import sglang` smoke is run separately by `ensure_sglang`,
/// which can only meaningfully pass on a CUDA host.)
pub(crate) fn sglang_venv_is_valid(runtime_dir: &std::path::Path) -> bool {
    sglang_venv_python(runtime_dir).is_file()
}

/// Run a `uv` subcommand, mapping a non-zero exit (or spawn failure) to a
/// `Pack` error with captured stderr. `uv` itself prints progress to stderr, so
/// `show_progress` controls whether that is inherited (live) or captured.
async fn run_uv(uv: &std::path::Path, args: &[String], show_progress: bool) -> Result<()> {
    let mut cmd = tokio::process::Command::new(uv);
    cmd.args(args);
    if show_progress {
        // Inherit stdio so uv's download/build progress streams to the user.
        let status = cmd.status().await.map_err(|e| {
            CapsuleError::Pack(format!("failed to run uv {}: {e}", args.first().cloned().unwrap_or_default()))
        })?;
        if !status.success() {
            return Err(CapsuleError::Pack(format!(
                "uv {} failed with status {status}",
                args.first().cloned().unwrap_or_default()
            )));
        }
        Ok(())
    } else {
        let output = cmd.output().await.map_err(|e| {
            CapsuleError::Pack(format!("failed to run uv {}: {e}", args.first().cloned().unwrap_or_default()))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CapsuleError::Pack(format!(
                "uv {} failed: {}",
                args.first().cloned().unwrap_or_default(),
                stderr.trim()
            )));
        }
        Ok(())
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

/// The canonical server-binary filename the launcher resolves at
/// `<runtime_dir>/<this>`. Platform-specific (`.exe` on Windows).
pub(crate) fn llamacpp_server_filename() -> &'static str {
    if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// A llama.cpp toolchain cache is valid only when the canonical server binary
/// exists and (on Unix) carries an executable bit. Empty/partial dirs are
/// invalid → callers discard and re-fetch.
pub(crate) fn llamacpp_cache_is_valid(runtime_dir: &std::path::Path) -> bool {
    let bin = runtime_dir.join(llamacpp_server_filename());
    let Ok(meta) = std::fs::metadata(&bin) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// Guarantee `<dir>/llama-server[.exe]` exists. When the binary is nested (a
/// layout `flatten_single_subdir` didn't fully normalize), link (Unix) / copy
/// (Windows) it to the canonical root. A symlink preserves `$ORIGIN`/
/// `@loader_path` so the real binary still finds its sibling shared libs.
fn ensure_canonical_llama_server(dir: &std::path::Path) -> Result<()> {
    let name = llamacpp_server_filename();
    let canonical = dir.join(name);
    if canonical.exists() {
        return Ok(());
    }
    let real = find_file_recursive(dir, name).ok_or_else(|| {
        CapsuleError::Pack(format!("llama.cpp archive did not contain a {name} binary"))
    })?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real, &canonical)
            .map_err(|e| CapsuleError::Pack(format!("link canonical {name}: {e}")))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(&real, &canonical)
            .map_err(|e| CapsuleError::Pack(format!("copy canonical {name}: {e}")))?;
    }
    Ok(())
}

fn find_file_recursive(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file_recursive(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}

/// Toolchain cache key for a llama.cpp engine: `<build-tag>` for the default
/// CPU/Metal build, or `<build-tag>@<variant>` for a GPU build (so variants
/// never share a cache directory). Used by both the fetcher and the launcher's
/// deterministic path so they always agree.
pub(crate) fn llamacpp_cache_key(version: &str, variant: Option<&str>) -> String {
    match normalize_engine_variant(variant) {
        Some(v) => format!("{version}@{v}"),
        None => version.to_string(),
    }
}

/// Split a cache key back into its build tag and optional variant.
fn parse_llamacpp_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once('@') {
        Some((tag, variant)) => (tag, Some(variant)),
        None => (key, None),
    }
}

/// Normalize a manifest `engine_variant` to its canonical slug, treating the
/// default CPU/Metal build (`None` / `""` / `default` / `cpu` / `metal`) as `None`.
fn normalize_engine_variant(variant: Option<&str>) -> Option<String> {
    let v = variant?.trim().to_ascii_lowercase();
    match v.as_str() {
        "" | "default" | "cpu" | "metal" => None,
        other => Some(other.to_string()),
    }
}

/// Map (build tag, host platform, variant) → llama.cpp release asset
/// (name, is_zip), failing closed for unsupported combinations.
///
/// * default (None): the CPU/Metal build (macOS = Metal).
/// * `vulkan`: GPU-accelerated, Linux only (NVIDIA via the driver's Vulkan ICD).
/// * `cuda`: no Linux prebuilt exists → fail closed (use `engine_path`).
pub(crate) fn llama_cpp_artifact_filename(
    version: &str,
    os: &str,
    arch: &str,
    variant: Option<&str>,
) -> Result<(String, bool)> {
    let variant = normalize_engine_variant(variant);
    let (slug, is_zip) = match variant.as_deref() {
        None => match (os, arch) {
            ("macos", "aarch64") => ("macos-arm64", false),
            ("macos", "x86_64") => ("macos-x64", false),
            ("linux", "x86_64") => ("ubuntu-x64", false),
            ("linux", "aarch64") => ("ubuntu-arm64", false),
            ("windows", "x86_64") => ("win-cpu-x64", true),
            ("windows", "aarch64") => ("win-cpu-arm64", true),
            _ => {
                return Err(CapsuleError::Pack(format!(
                    "Unsupported llama.cpp platform: {os} {arch}"
                )));
            }
        },
        Some("vulkan") => match (os, arch) {
            ("linux", "x86_64") => ("ubuntu-vulkan-x64", false),
            ("linux", "aarch64") => ("ubuntu-vulkan-arm64", false),
            ("macos", _) => {
                return Err(CapsuleError::Pack(
                    "engine_variant=\"vulkan\" is not supported on macOS — use the default \
                     (Metal-accelerated) build by omitting engine_variant"
                        .to_string(),
                ));
            }
            _ => {
                return Err(CapsuleError::Pack(format!(
                    "engine_variant=\"vulkan\" has no llama.cpp prebuilt for {os} {arch} \
                     (Linux x64/arm64 only)"
                )));
            }
        },
        Some("cuda") => {
            return Err(CapsuleError::Pack(
                "engine_variant=\"cuda\" has no llama.cpp Linux prebuilt for this release; \
                 set an explicit `engine_path`, or use engine_variant=\"vulkan\" for managed \
                 GPU acceleration"
                    .to_string(),
            ));
        }
        Some(other) => {
            return Err(CapsuleError::Pack(format!(
                "unknown engine_variant {other:?} (supported: vulkan; default = CPU/Metal)"
            )));
        }
    };
    let ext = if is_zip { "zip" } else { "tar.gz" };
    Ok((format!("llama-{version}-bin-{slug}.{ext}"), is_zip))
}

#[cfg(test)]
mod tests {
    use super::{
        deno_artifact_filename, ensure_canonical_llama_server, llama_cpp_artifact_filename,
        llamacpp_cache_is_valid, llamacpp_cache_key, llamacpp_server_filename, sglang_requirements,
        sglang_venv_python,
    };

    #[cfg(unix)]
    fn write_exec(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn cache_is_invalid_when_binary_missing_and_valid_when_present() {
        let dir = tempfile::tempdir().unwrap();
        // Empty dir → invalid.
        assert!(!llamacpp_cache_is_valid(dir.path()));

        let bin = dir.path().join(llamacpp_server_filename());
        #[cfg(unix)]
        {
            // Non-executable file → still invalid.
            std::fs::write(&bin, b"x").unwrap();
            assert!(!llamacpp_cache_is_valid(dir.path()));
            // Executable → valid.
            write_exec(&bin);
            assert!(llamacpp_cache_is_valid(dir.path()));
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&bin, b"x").unwrap();
            assert!(llamacpp_cache_is_valid(dir.path()));
        }
    }

    #[test]
    #[cfg(unix)]
    fn ensure_canonical_links_nested_binary_to_root() {
        let dir = tempfile::tempdir().unwrap();
        // A non-standard layout the single-subdir flatten didn't normalize:
        // the binary is nested under a sub-directory alongside its libs.
        let nested = dir.path().join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        write_exec(&nested.join("llama-server"));

        assert!(
            !llamacpp_cache_is_valid(dir.path()),
            "precondition: not at root"
        );
        ensure_canonical_llama_server(dir.path()).unwrap();

        let canonical = dir.path().join("llama-server");
        assert!(canonical.exists(), "canonical root binary must exist");
        assert!(
            llamacpp_cache_is_valid(dir.path()),
            "cache must be valid via the canonical path"
        );
    }

    #[test]
    fn llama_cpp_artifact_filename_maps_platforms() {
        // Default (CPU/Metal) build — unchanged from Inc2.
        assert_eq!(
            llama_cpp_artifact_filename("b4231", "macos", "aarch64", None).unwrap(),
            ("llama-b4231-bin-macos-arm64.tar.gz".to_string(), false)
        );
        assert_eq!(
            llama_cpp_artifact_filename("b4231", "linux", "x86_64", None).unwrap(),
            ("llama-b4231-bin-ubuntu-x64.tar.gz".to_string(), false)
        );
        assert_eq!(
            llama_cpp_artifact_filename("b4231", "windows", "x86_64", None).unwrap(),
            ("llama-b4231-bin-win-cpu-x64.zip".to_string(), true)
        );
        assert!(llama_cpp_artifact_filename("b4231", "plan9", "x86_64", None).is_err());
        // "cpu"/"metal"/"default" normalize to the default build.
        assert_eq!(
            llama_cpp_artifact_filename("b4231", "macos", "aarch64", Some("metal")).unwrap(),
            ("llama-b4231-bin-macos-arm64.tar.gz".to_string(), false)
        );
    }

    #[test]
    fn llama_cpp_vulkan_variant_linux_only() {
        assert_eq!(
            llama_cpp_artifact_filename("b9754", "linux", "x86_64", Some("vulkan")).unwrap(),
            (
                "llama-b9754-bin-ubuntu-vulkan-x64.tar.gz".to_string(),
                false
            )
        );
        assert_eq!(
            llama_cpp_artifact_filename("b9754", "linux", "aarch64", Some("vulkan")).unwrap(),
            (
                "llama-b9754-bin-ubuntu-vulkan-arm64.tar.gz".to_string(),
                false
            )
        );
        // macOS vulkan → explicit error (use the Metal default).
        let mac = llama_cpp_artifact_filename("b9754", "macos", "aarch64", Some("vulkan"));
        assert!(mac.is_err() && mac.unwrap_err().to_string().contains("macOS"));
    }

    #[test]
    fn llama_cpp_cuda_variant_fails_closed() {
        let err = llama_cpp_artifact_filename("b9754", "linux", "x86_64", Some("cuda"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cuda") && err.contains("no llama.cpp Linux prebuilt"));
        // Never silently falls back to a CPU build.
        assert!(!err.contains("ubuntu-x64"));
    }

    #[test]
    fn llamacpp_cache_key_separates_variants() {
        assert_eq!(llamacpp_cache_key("b9754", None), "b9754");
        assert_eq!(llamacpp_cache_key("b9754", Some("cpu")), "b9754");
        assert_eq!(llamacpp_cache_key("b9754", Some("vulkan")), "b9754@vulkan");
    }

    #[test]
    fn sglang_requirements_pins_torch_first_then_sglang_on_cu124() {
        let reqs = sglang_requirements("0.4.10.post2");
        assert_eq!(reqs.python, "3.12");
        assert_eq!(reqs.torch_index_url, "https://download.pytorch.org/whl/cu124");
        // torch is pinned and installed BEFORE sglang (defeats the auto-upgrade).
        assert!(reqs.torch_pins.iter().any(|p| p == "torch==2.6.0"));
        assert!(reqs.torch_pins.iter().any(|p| p == "torchvision"));
        assert!(reqs.torch_pins.iter().any(|p| p == "torchaudio"));
        // The wheel version flows through verbatim into the sglang pin.
        assert_eq!(reqs.sglang_pin, "sglang==0.4.10.post2");
        // The CUDA kernels SGLang needs on Ampere.
        assert!(reqs.kernel_pins.iter().any(|p| p == "sgl-kernel"));
        assert!(reqs.kernel_pins.iter().any(|p| p == "flashinfer-python"));
    }

    #[test]
    fn sglang_venv_python_is_canonical_bin_python() {
        let dir = std::path::Path::new("/cache/sglang-0.4.10.post2");
        let py = sglang_venv_python(dir);
        if cfg!(windows) {
            assert!(py.ends_with("Scripts/python.exe") || py.ends_with("Scripts\\python.exe"));
        } else {
            assert_eq!(py, std::path::Path::new("/cache/sglang-0.4.10.post2/bin/python"));
        }
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
