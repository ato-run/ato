//! JIT runtime fetcher for pack-time bundling.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use futures::StreamExt;
use reqwest::StatusCode;

use tracing::{debug, info};

use crate::error::{CapsuleError, Result};

// uv 0.4.19 (Oct 2024) cannot resolve modern PyTorch cu128 wheels off an
// `--extra-index-url` — its resolver rejects e.g. `torchaudio==2.9.1+cu128-cp312`
// as having "no wheels with a matching Python implementation tag", which breaks
// the SGLang managed-venv build. A current uv resolves it. Verified on a real
// A6000: 0.4.19 fails the `sglang[srt]==0.5.9` resolve, 0.11.24 succeeds.
const DEFAULT_UV_VERSION: &str = "0.11.24";

struct RuntimeInstallLock {
    _file: File,
}

mod fetcher;
mod verifier;

pub(crate) use fetcher::llamacpp_cache_key;
// `sglang_venv_python` is re-exported `pub` (not `pub(crate)`) so the CLI's
// `nvidia-cuda` doctor can probe the managed venv at the SAME canonical path the
// launcher resolves — they must never disagree on where the venv python lives.
pub use fetcher::sglang_venv_python;
pub use verifier::{ArtifactVerifier, ChecksumVerifier};

/// Whether this host has managed llama.cpp prebuilts for the native-inference
/// runtime, and which acceleration the default build provides. Powers
/// `ato doctor native-inference`: it reuses the exact platform → artifact
/// mapping the fetcher applies on a real run, so the doctor can never disagree
/// with what would actually download.
#[derive(Debug, Clone)]
pub struct LlamaCppPlatformSupport {
    /// Normalized OS (`macos` | `linux` | `windows`).
    pub os: String,
    /// Normalized arch (`x86_64` | `aarch64`).
    pub arch: String,
    /// The default build (CPU on Linux/Windows, Metal on macOS) is available.
    pub default_available: bool,
    /// The `engine_variant = "vulkan"` Linux NVIDIA build is available here.
    pub vulkan_available: bool,
}

/// Probe managed llama.cpp engine availability for `version` (e.g. `"b9754"`)
/// on this host. `Err` only when the OS/arch itself is unsupported (the fetcher
/// cannot even name a platform).
pub fn llama_cpp_platform_support(version: &str) -> Result<LlamaCppPlatformSupport> {
    let (os, arch) = RuntimeFetcher::detect_platform()?;
    let default_available = fetcher::llama_cpp_artifact_filename(version, &os, &arch, None).is_ok();
    let vulkan_available =
        fetcher::llama_cpp_artifact_filename(version, &os, &arch, Some("vulkan")).is_ok();
    Ok(LlamaCppPlatformSupport {
        os,
        arch,
        default_available,
        vulkan_available,
    })
}

pub struct RuntimeFetcher {
    cache_dir: PathBuf,
    verifier: Arc<dyn ArtifactVerifier>,
    fetchers: HashMap<&'static str, Box<dyn fetcher::ToolchainFetcher>>,
    reporter: Arc<dyn crate::reporter::CapsuleReporter + 'static>,
}

#[allow(dead_code)]
impl RuntimeFetcher {
    pub fn new() -> Result<Self> {
        Self::new_with_reporter(Arc::new(crate::reporter::NoOpReporter))
    }

    pub fn new_with_verifier(verifier: Arc<dyn ArtifactVerifier>) -> Result<Self> {
        Self::new_with_verifier_and_reporter(verifier, Arc::new(crate::reporter::NoOpReporter))
    }

    pub fn new_with_reporter(
        reporter: Arc<dyn crate::reporter::CapsuleReporter + 'static>,
    ) -> Result<Self> {
        Self::new_with_verifier_and_reporter(Arc::new(ChecksumVerifier), reporter)
    }

    pub fn new_with_verifier_and_reporter(
        verifier: Arc<dyn ArtifactVerifier>,
        reporter: Arc<dyn crate::reporter::CapsuleReporter + 'static>,
    ) -> Result<Self> {
        let cache_dir = toolchain_cache_dir()?;
        fs::create_dir_all(&cache_dir).map_err(|e| {
            CapsuleError::Pack(format!("Failed to create toolchain cache directory: {}", e))
        })?;

        Ok(Self {
            cache_dir,
            verifier,
            fetchers: fetcher::default_fetchers(),
            reporter,
        })
    }

    fn canonical_fetcher_key(language: &str) -> Option<&'static str> {
        match language.to_lowercase().as_str() {
            "python" => Some("python"),
            "node" | "nodejs" => Some("node"),
            "deno" => Some("deno"),
            "bun" => Some("bun"),
            // Native-inference engines: the engine-string → toolchain-key mapping
            // is owned by `EngineId` (the single source of the engine vocabulary)
            // so a fetcher key never drifts from the launcher's dispatch.
            other => crate::routing::native_inference::EngineId::from_manifest(other)
                .map(|id| id.toolchain_key()),
        }
    }

    async fn download_runtime_with_progress(
        &self,
        language: &str,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let key = Self::canonical_fetcher_key(language).ok_or_else(|| {
            CapsuleError::Pack(format!("Unsupported runtime language: {}", language))
        })?;

        let runtime_dir = self.get_runtime_path(key, version);
        if runtime_dir.exists() {
            return Ok(runtime_dir);
        }

        let _lock = self.acquire_install_lock(key, version).await.map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to acquire install lock for {} {}: {}",
                key, version, e
            ))
        })?;

        if runtime_dir.exists() {
            return Ok(runtime_dir);
        }

        let fetcher = self.fetchers.get(key).ok_or_else(|| {
            CapsuleError::Pack(format!("No runtime fetcher registered for: {}", key))
        })?;

        debug!("Using runtime fetcher: {}", fetcher.language());

        fetcher
            .download_runtime(self, version, show_progress)
            .await
            .map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to download runtime: {} {} ({})",
                    key, version, e
                ))
            })
    }

    fn sanitize_lock_component(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn lock_path(cache_dir: &Path, language: &str, version: &str) -> PathBuf {
        let lock_dir = cache_dir.join(".locks");
        let v = Self::sanitize_lock_component(version);
        lock_dir.join(format!("{}-{}.lock", language, v))
    }

    async fn acquire_install_lock(
        &self,
        language: &str,
        version: &str,
    ) -> Result<RuntimeInstallLock> {
        let lock_path = Self::lock_path(&self.cache_dir, language, version);
        let lock_path = lock_path.clone();

        tokio::task::spawn_blocking(move || -> Result<RuntimeInstallLock> {
            if let Some(parent) = lock_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    CapsuleError::Pack(format!("Failed to create lock directory: {}", e))
                })?;
            }

            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&lock_path)
                .map_err(|e| {
                    CapsuleError::Pack(format!("Failed to open lock file {:?}: {}", lock_path, e))
                })?;

            match file.try_lock_exclusive() {
                Ok(()) => Ok(RuntimeInstallLock { _file: file }),
                Err(e) if e.kind() == fs2::lock_contended_error().kind() => {
                    file.lock_exclusive().map_err(|e| {
                        CapsuleError::Pack(format!(
                            "Failed to wait for lock {:?}: {}",
                            lock_path, e
                        ))
                    })?;
                    Ok(RuntimeInstallLock { _file: file })
                }
                Err(e) => Err(CapsuleError::Pack(format!(
                    "Failed to lock runtime install: {}",
                    e
                ))),
            }
        })
        .await
        .map_err(|e| CapsuleError::Pack(format!("Failed to join lock acquisition task: {}", e)))?
    }

    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    pub fn is_cached(&self, language: &str, version: &str) -> bool {
        let runtime_dir = self.cache_dir.join(format!("{}-{}", language, version));
        runtime_dir.exists()
    }

    pub fn get_runtime_path(&self, language: &str, version: &str) -> PathBuf {
        self.cache_dir.join(format!("{}-{}", language, version))
    }

    pub async fn download_python_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_python_runtime_with_progress(version, true)
            .await
    }

    pub async fn ensure_python(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self
            .download_python_runtime_with_progress(version, true)
            .await?;
        let python_bin = Self::find_python_binary(&runtime_dir)?;
        info!("Python {} ready at {:?}", version, python_bin);
        Ok(python_bin)
    }

    async fn download_python_runtime_with_progress(
        &self,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        self.download_runtime_with_progress("python", version, show_progress)
            .await
    }

    pub async fn ensure_node(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self
            .download_node_runtime_with_progress(version, true)
            .await?;
        let node_bin = Self::find_binary_recursive(&runtime_dir, &["node", "node.exe"])?;
        info!("Node {} ready at {:?}", version, node_bin);
        Ok(node_bin)
    }

    pub async fn ensure_deno(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self
            .download_deno_runtime_with_progress(version, true)
            .await?;
        let deno_bin = Self::find_binary_recursive(&runtime_dir, &["deno", "deno.exe"])?;
        info!("Deno {} ready at {:?}", version, deno_bin);
        Ok(deno_bin)
    }

    pub async fn ensure_bun(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self
            .download_bun_runtime_with_progress(version, true)
            .await?;
        let bun_bin = Self::find_binary_recursive(&runtime_dir, &["bun", "bun.exe"])?;
        info!("Bun {} ready at {:?}", version, bun_bin);
        Ok(bun_bin)
    }

    /// Download (if needed) the `llama-server` binary for a pinned llama.cpp
    /// release (`version` = build tag, e.g. `"b4231"`) and build `variant`
    /// (`None` = default CPU/Metal; `Some("vulkan")` = GPU). Returns its canonical
    /// path `<cache>/llamacpp-<key>/llama-server[.exe]` — the exact path the
    /// launcher resolves. Used by the native-inference engine ensure-step.
    pub async fn ensure_llamacpp(&self, version: &str, variant: Option<&str>) -> Result<PathBuf> {
        // The cache KEY embeds the variant so GPU/CPU builds of the same tag never
        // share a directory; the fetcher reverses it to the build tag + asset slug.
        let key = fetcher::llamacpp_cache_key(version, variant);

        // Pre-discard an incomplete/corrupt cache BEFORE the dispatch's
        // exists-only short-circuit can reuse it (the dispatch can't tell a
        // partial dir from a complete one; the fetcher's validity check only
        // runs once download_runtime is reached).
        let runtime_dir = self.get_runtime_path("llamacpp", &key);
        if runtime_dir.exists() && !fetcher::llamacpp_cache_is_valid(&runtime_dir) {
            let _ = fs::remove_dir_all(&runtime_dir);
        }

        let runtime_dir = self
            .download_runtime_with_progress("llamacpp", &key, true)
            .await?;
        // download_runtime guarantees the canonical binary as a post-condition.
        let server_bin = runtime_dir.join(fetcher::llamacpp_server_filename());
        if !server_bin.exists() {
            return Err(CapsuleError::Pack(format!(
                "llama.cpp {key}: canonical llama-server missing after fetch at {server_bin:?}"
            )));
        }
        info!("llama.cpp {} ready at {:?}", key, server_bin);
        Ok(server_bin)
    }

    /// Create (if needed) a managed Python venv for the pinned SGLang `version`
    /// (the sglang wheel version, e.g. `"0.4.10.post2"`) and `pip install` the
    /// pinned sglang + torch (cu124) + sgl-kernel/flashinfer requirements, then
    /// run an `import sglang` smoke as a post-condition. Returns the canonical
    /// venv python path `<cache>/sglang-<version>/bin/python[.exe]` — the exact
    /// path the launcher resolves as the server command. Used by the SGLang
    /// native-inference engine ensure-step.
    ///
    /// The venv install runs on any Linux+NVIDIA host; the `import sglang` smoke
    /// requires the CUDA toolchain/kernels to load, so it only passes on a real
    /// CUDA host (host-pending) — but the command is real, not stubbed.
    pub async fn ensure_sglang(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self
            .download_runtime_with_progress("sglang", version, true)
            .await?;
        let python = fetcher::sglang_venv_python(&runtime_dir);
        if !python.is_file() {
            return Err(CapsuleError::Pack(format!(
                "sglang {version}: venv python missing at {python:?} after install"
            )));
        }
        // Smoke: the venv must be able to import sglang (proves the install +
        // its CUDA kernels resolve). On a non-CUDA host this fails honestly
        // rather than reporting a usable engine.
        let output = tokio::process::Command::new(&python)
            .args(["-c", "import sglang"])
            .output()
            .await
            .map_err(|e| {
                CapsuleError::Pack(format!(
                    "sglang import smoke: failed to run {python:?}: {e}"
                ))
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CapsuleError::Pack(format!(
                "sglang {version}: `import sglang` failed in the managed venv \
                 (CUDA toolchain/kernels required): {}",
                stderr.trim()
            )));
        }
        info!("sglang {} ready at {:?}", version, python);
        Ok(python)
    }

    pub async fn ensure_uv(&self, version: Option<&str>) -> Result<PathBuf> {
        let version = version.unwrap_or(DEFAULT_UV_VERSION);
        let runtime_dir = self.download_uv_tool_with_progress(version, true).await?;
        let uv_bin = Self::find_binary_recursive(&runtime_dir, &["uv", "uv.exe"])?;
        info!("uv {} ready at {:?}", version, uv_bin);
        Ok(uv_bin)
    }

    pub async fn download_node_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_node_runtime_with_progress(version, true)
            .await
    }

    pub async fn download_deno_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_deno_runtime_with_progress(version, true)
            .await
    }

    pub async fn download_bun_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_bun_runtime_with_progress(version, true).await
    }

    async fn download_node_runtime_with_progress(
        &self,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        self.download_runtime_with_progress("node", version, show_progress)
            .await
    }

    async fn download_deno_runtime_with_progress(
        &self,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        self.download_runtime_with_progress("deno", version, show_progress)
            .await
    }

    async fn download_uv_tool_with_progress(
        &self,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let install_dir = self.cache_dir.join(format!("uv-{}", version));
        // Validate-before-reuse: a bare `exists()` reuses a corrupt or
        // half-extracted cache entry forever (it wedges). Reuse only when a
        // usable `uv` binary is actually present, mirroring `ensure_llamacpp`.
        if uv_cache_is_valid(&install_dir) {
            return Ok(install_dir);
        }

        let _lock = self.acquire_install_lock("uv", version).await?;
        if uv_cache_is_valid(&install_dir) {
            return Ok(install_dir);
        }

        let target = if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
            "aarch64-apple-darwin"
        } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
            "x86_64-apple-darwin"
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
            "x86_64-unknown-linux-gnu"
        } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
            "aarch64-unknown-linux-gnu"
        } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
            "x86_64-pc-windows-msvc"
        } else {
            return Err(CapsuleError::Pack(
                "Unsupported platform for uv runtime acquisition".to_string(),
            ));
        };

        let extension = if target.ends_with("windows-msvc") {
            "zip"
        } else {
            "tar.gz"
        };
        let filename = format!("uv-{}.{}", target, extension);
        let url = format!(
            "https://github.com/astral-sh/uv/releases/download/{}/{}",
            version, filename
        );
        let checksum_url = format!("{}.sha256", url);

        if show_progress {
            self.reporter
                .notify(format!("⬇️  Downloading uv {}", version))
                .await?;
        }

        let sha256 = self.fetch_expected_sha256(&checksum_url, None).await?;
        let archive_path = self
            .cache_dir
            .join(format!("uv-{}-{}.{}", version, target, extension));
        self.download_with_progress(&url, &archive_path, show_progress)
            .await?;
        self.verify_sha256_of_file(&archive_path, &sha256)?;

        // Extract into a temp dir, then atomically rename it into place. An
        // in-place extract that is interrupted leaves a half-populated
        // `install_dir` that the validity check above would wrongly reuse;
        // the temp-dir + rename pattern (same as the Python/Node fetchers)
        // guarantees `install_dir` only ever appears complete.
        let temp_dir = self
            .cache_dir
            .join(format!("tmp-uv-{}-{}", version, target));
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to reset temp uv extract directory {:?}: {}",
                    temp_dir, e
                ))
            })?;
        }
        fs::create_dir_all(&temp_dir).map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to create temp uv extract directory {:?}: {}",
                temp_dir, e
            ))
        })?;

        if extension == "zip" {
            Self::extract_zip_from_file(&archive_path, &temp_dir)?;
        } else {
            Self::extract_archive_from_file(&archive_path, &temp_dir)?;
        }

        // Refuse to promote a directory that lacks a usable uv binary, so we
        // never publish a cache entry the validity check would later reject.
        if locate_runtime_binary(&temp_dir, &["uv", "uv.exe"]).is_none() {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(CapsuleError::Pack(format!(
                "uv {} archive did not contain a uv binary after extraction",
                version
            )));
        }

        if install_dir.exists() {
            fs::remove_dir_all(&install_dir).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to reset existing uv install directory {:?}: {}",
                    install_dir, e
                ))
            })?;
        }
        fs::rename(&temp_dir, &install_dir).map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to move extracted uv runtime into cache {:?}: {}",
                install_dir, e
            ))
        })?;
        let _ = fs::remove_file(&archive_path);

        Ok(install_dir)
    }

    async fn download_bun_runtime_with_progress(
        &self,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        self.download_runtime_with_progress("bun", version, show_progress)
            .await
    }

    pub(crate) async fn fetch_expected_sha256(
        &self,
        url: &str,
        filename_hint: Option<&str>,
    ) -> Result<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let response = client
            .get(url)
            .send()
            .await
            .map_err(CapsuleError::Network)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CapsuleError::AuthRequired(url.to_string()));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(CapsuleError::NotFound(url.to_string()));
        }

        if !response.status().is_success() {
            return Err(CapsuleError::Network(
                response.error_for_status().unwrap_err(),
            ));
        }

        let text = response.text().await.map_err(CapsuleError::Network)?;
        Self::parse_sha256_from_text(&text, filename_hint)
    }

    fn parse_sha256_from_text(text: &str, filename_hint: Option<&str>) -> Result<String> {
        if let Some(filename) = filename_hint {
            for line in text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
                if !line.contains(filename) {
                    continue;
                }
                for token in line
                    .split(|c: char| c.is_whitespace() || c == '=' || c == '(' || c == ')')
                    .filter(|s| !s.is_empty())
                {
                    let t = token.trim();
                    if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                        return Ok(t.to_ascii_lowercase());
                    }
                }
            }
        }

        for token in text
            .split(|c: char| c.is_whitespace() || c == '=' || c == '(' || c == ')')
            .filter(|s| !s.is_empty())
        {
            let t = token.trim();
            if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(t.to_ascii_lowercase());
            }
        }

        Err(CapsuleError::Pack(
            "Could not parse sha256 from text".to_string(),
        ))
    }

    fn verify_sha256_of_file(&self, path: &PathBuf, expected_hex: &str) -> Result<()> {
        match self.verifier.verify_sha256(path.as_path(), expected_hex) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(path);
                Err(e)
            }
        }
    }

    async fn download_with_progress(
        &self,
        url: &str,
        dest: &PathBuf,
        show_progress: bool,
    ) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()?;

        let response = client
            .get(url)
            .send()
            .await
            .map_err(CapsuleError::Network)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CapsuleError::AuthRequired(url.to_string()));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(CapsuleError::NotFound(url.to_string()));
        }

        if !response.status().is_success() {
            return Err(CapsuleError::Network(
                response.error_for_status().unwrap_err(),
            ));
        }

        let total_size = response.content_length();

        if show_progress {
            self.reporter
                .progress_start(format!("Downloading {}", url), total_size)
                .await?;
        }

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut file = File::create(dest)
            .map_err(|e| CapsuleError::Pack(format!("Failed to create download file: {}", e)))?;
        let mut stream = response.bytes_stream();
        let mut _downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(CapsuleError::Network)?;
            file.write_all(&chunk)
                .map_err(|e| CapsuleError::Pack(format!("Failed to write to file: {}", e)))?;
            _downloaded += chunk.len() as u64;

            if show_progress {
                self.reporter.progress_inc(chunk.len() as u64).await?;
            }
        }

        if show_progress {
            self.reporter
                .progress_finish(Some("Download complete".to_string()))
                .await?;
        }

        Ok(())
    }

    fn find_python_binary(runtime_dir: &PathBuf) -> Result<PathBuf> {
        let candidates = [
            runtime_dir.join("python/bin/python3"),
            runtime_dir.join("python/bin/python"),
            runtime_dir.join("bin/python3"),
            runtime_dir.join("bin/python"),
            runtime_dir.join("python/python.exe"),
            runtime_dir.join("python.exe"),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                return Ok(candidate.clone());
            }
        }

        Err(CapsuleError::Pack(format!(
            "Python binary not found in runtime directory: {:?}",
            runtime_dir
        )))
    }

    fn extract_archive_from_file(archive_path: &Path, dest: &Path) -> Result<()> {
        use flate2::read::GzDecoder;
        use tar::Archive;

        let file = File::open(archive_path).map_err(|e| {
            CapsuleError::Pack(format!("Failed to open archive {:?}: {}", archive_path, e))
        })?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);

        archive
            .unpack(dest)
            .map_err(|e| CapsuleError::Pack(format!("Failed to extract archive: {}", e)))?;

        Ok(())
    }

    fn extract_zip_from_file(archive_path: &Path, dest: &Path) -> Result<()> {
        use std::io::copy;
        use zip::ZipArchive;

        let file = File::open(archive_path).map_err(|e| {
            CapsuleError::Pack(format!("Failed to open zip {:?}: {}", archive_path, e))
        })?;
        let mut zip = ZipArchive::new(file)
            .map_err(|e| CapsuleError::Pack(format!("Failed to read zip archive: {}", e)))?;

        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| CapsuleError::Pack(format!("Failed to read zip entry: {}", e)))?;
            let out_rel = match entry.enclosed_name() {
                Some(p) => p.to_owned(),
                None => continue,
            };

            let out_path = dest.join(out_rel);
            if entry.is_dir() {
                fs::create_dir_all(&out_path)?;
                continue;
            }

            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut outfile = File::create(&out_path).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to create extracted file {:?}: {}",
                    out_path, e
                ))
            })?;
            copy(&mut entry, &mut outfile).map_err(|e| {
                CapsuleError::Pack(format!(
                    "Failed to extract zip entry to {:?}: {}",
                    out_path, e
                ))
            })?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(name) = out_path.file_name().and_then(|s| s.to_str())
                    && (name == "node" || name == "deno" || name == "bun")
                {
                    let mut perms = fs::metadata(&out_path)?.permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&out_path, perms)?;
                }
            }
        }

        Ok(())
    }

    fn find_binary_recursive(runtime_dir: &PathBuf, candidates: &[&str]) -> Result<PathBuf> {
        for candidate in candidates {
            let direct = runtime_dir.join(candidate);
            if direct.is_file() {
                return Ok(direct);
            }
        }

        fn walk(dir: &std::path::Path, candidates: &[&str]) -> std::io::Result<Option<PathBuf>> {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = walk(&path, candidates)? {
                        return Ok(Some(found));
                    }
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|s| s.to_str())
                    && candidates.iter().any(|c| c.eq_ignore_ascii_case(name))
                {
                    return Ok(Some(path));
                }
            }
            Ok(None)
        }

        match walk(runtime_dir, candidates)
            .map_err(|e| CapsuleError::Pack(format!("Failed to search runtime directory: {}", e)))?
        {
            Some(p) => Ok(p),
            None => Err(CapsuleError::Pack(format!(
                "Binary not found in runtime directory: {:?} (candidates={:?})",
                runtime_dir, candidates
            ))),
        }
    }

    fn normalize_semverish(version: &str) -> String {
        let mut v = version.trim();
        for prefix in ["bun-v", "v", "^", ">=", "==", "=", "~="] {
            if let Some(rest) = v.strip_prefix(prefix) {
                v = rest.trim();
            }
        }

        let mut out = String::new();
        for ch in v.chars() {
            if ch.is_ascii_digit() || ch == '.' {
                out.push(ch);
            } else {
                break;
            }
        }

        if out.is_empty() {
            version.trim().to_string()
        } else {
            out
        }
    }

    pub(crate) fn detect_platform() -> Result<(String, String)> {
        let os = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            return Err(CapsuleError::Pack("Unsupported OS".to_string()));
        };

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            return Err(CapsuleError::Pack("Unsupported architecture".to_string()));
        };

        Ok((os.to_string(), arch.to_string()))
    }

    pub(crate) fn get_python_download_url(version: &str, os: &str, arch: &str) -> Result<String> {
        let full_version = match version {
            "3.11" => "3.11.10",
            "3.12" => "3.12.7",
            "3.13" => "3.13.0rc3",
            _ => version,
        };

        let build_date = "20241002";

        let (triple, variant) = match (os, arch) {
            ("linux", "x86_64") => ("x86_64-unknown-linux-gnu", "install_only"),
            ("linux", "aarch64") => ("aarch64-unknown-linux-gnu", "install_only"),
            ("macos", "x86_64") => ("x86_64-apple-darwin", "install_only"),
            ("macos", "aarch64") => ("aarch64-apple-darwin", "install_only"),
            ("windows", "x86_64") => ("x86_64-pc-windows-msvc", "shared-install_only"),
            _ => {
                return Err(CapsuleError::Pack(format!(
                    "Unsupported platform: {} {}",
                    os, arch
                )));
            }
        };

        let filename = format!(
            "cpython-{}+{}-{}-{}.tar.gz",
            full_version, build_date, triple, variant
        );

        let base_url = "https://github.com/astral-sh/python-build-standalone/releases/download";
        let release_tag = build_date;

        Ok(format!("{}/{}/{}", base_url, release_tag, filename))
    }

    pub(crate) async fn resolve_node_full_version(version_hint: &str) -> Result<String> {
        let hint = Self::normalize_semverish(version_hint);
        let parts: Vec<&str> = hint.split('.').filter(|s| !s.is_empty()).collect();
        if parts.len() >= 3 {
            return Ok(hint);
        }

        let prefix = if parts.len() == 2 {
            format!("{}.{}.", parts[0], parts[1])
        } else if parts.len() == 1 {
            format!("{}.", parts[0])
        } else {
            return Err(CapsuleError::Config(format!(
                "Invalid Node version hint: {}",
                version_hint
            )));
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let response = client
            .get("https://nodejs.org/dist/index.json")
            .send()
            .await
            .map_err(CapsuleError::Network)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CapsuleError::AuthRequired(
                "https://nodejs.org/dist/index.json".to_string(),
            ));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(CapsuleError::NotFound(
                "https://nodejs.org/dist/index.json".to_string(),
            ));
        }

        if !response.status().is_success() {
            return Err(CapsuleError::Network(
                response.error_for_status().unwrap_err(),
            ));
        }

        let json: serde_json::Value = response.json().await.map_err(CapsuleError::Network)?;
        let arr = json
            .as_array()
            .ok_or_else(|| CapsuleError::Pack("Node index.json is not an array".to_string()))?;

        for item in arr {
            let v = match item.get("version").and_then(|v| v.as_str()) {
                Some(v) => v,
                None => continue,
            };
            let v = v.trim_start_matches('v');
            if v.starts_with(&prefix) {
                return Ok(v.to_string());
            }
        }

        Err(CapsuleError::Pack(format!(
            "Could not resolve Node version for hint: {}",
            version_hint
        )))
    }

    pub(crate) fn node_artifact_filename(
        full_version: &str,
        os: &str,
        arch: &str,
    ) -> Result<(String, bool)> {
        let (platform, is_zip) = match os {
            "linux" => ("linux", false),
            "macos" => ("darwin", false),
            "windows" => ("win", true),
            _ => {
                return Err(CapsuleError::Pack(format!(
                    "Unsupported OS for Node: {}",
                    os
                )));
            }
        };

        let arch = match (os, arch) {
            ("windows", "x86_64") => "x64",
            ("windows", "aarch64") => "arm64",
            (_, "x86_64") => "x64",
            (_, "aarch64") => "arm64",
            _ => {
                return Err(CapsuleError::Pack(format!(
                    "Unsupported arch for Node: {}",
                    arch
                )));
            }
        };

        let filename = if is_zip {
            format!("node-v{}-{}-{}.zip", full_version, platform, arch)
        } else {
            format!("node-v{}-{}-{}.tar.gz", full_version, platform, arch)
        };

        Ok((filename, is_zip))
    }
}

fn toolchain_cache_dir() -> Result<PathBuf> {
    crate::common::paths::toolchain_cache_dir()
}

/// Locate a managed-runtime executable inside an already-extracted toolchain
/// directory (`<cache>/<tool>-<version>/`), matching the same layout
/// [`RuntimeFetcher::ensure_node`] / `ensure_uv` / `ensure_python` produce.
///
/// Returns the first `candidates` filename found (direct child first, then a
/// recursive walk). This is the read-only counterpart used by Runtime Setup
/// status detection to verify a cached toolchain actually contains a usable
/// binary — not just a (possibly corrupt/empty) version directory.
pub fn locate_runtime_binary(runtime_dir: &Path, candidates: &[&str]) -> Option<PathBuf> {
    for candidate in candidates {
        let direct = runtime_dir.join(candidate);
        if direct.is_file() {
            return Some(direct);
        }
    }

    fn walk(dir: &Path, candidates: &[&str]) -> std::io::Result<Option<PathBuf>> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk(&path, candidates)? {
                    return Ok(Some(found));
                }
                continue;
            }
            if let Some(name) = path.file_name().and_then(|s| s.to_str())
                && candidates.iter().any(|c| c.eq_ignore_ascii_case(name))
            {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    walk(runtime_dir, candidates).ok().flatten()
}

/// True when `install_dir` holds a usable managed uv install — i.e. a
/// `uv`/`uv.exe` binary is actually present, not merely the version directory.
/// Used by the uv fetcher to validate-before-reuse so a corrupt or partially
/// extracted cache entry self-heals (gets rebuilt) instead of wedging forever.
/// Mirrors `fetcher::llamacpp_cache_is_valid`; reuses `locate_runtime_binary`.
fn uv_cache_is_valid(install_dir: &Path) -> bool {
    locate_runtime_binary(install_dir, &["uv", "uv.exe"]).is_some()
}

#[cfg(test)]
mod tests {
    use super::{RuntimeFetcher, llama_cpp_platform_support, uv_cache_is_valid};

    #[test]
    fn test_normalize_semverish() {
        assert_eq!(RuntimeFetcher::normalize_semverish("v1.2.3"), "1.2.3");
        assert_eq!(RuntimeFetcher::normalize_semverish("^3.11"), "3.11");
    }

    // CI/dev hosts are macOS or Linux on x64/arm64 — all have a managed
    // llama.cpp default prebuilt — so the native-inference probe must report
    // platform-supported, and Vulkan must be Linux-only.
    #[test]
    fn llama_cpp_platform_support_reports_current_host() {
        let s = llama_cpp_platform_support("b9754").expect("host platform is supported");
        assert!(
            s.default_available,
            "{}-{} should have a managed default build",
            s.os, s.arch
        );
        if s.os == "macos" {
            assert!(!s.vulkan_available, "macOS has no Vulkan prebuilt");
        }
    }

    // validate-before-reuse self-heal: an empty/corrupt uv cache dir must be
    // reported invalid (so it is rebuilt), and only a dir that actually holds
    // a `uv` binary may be reused.
    #[test]
    fn uv_cache_is_valid_requires_a_uv_binary() {
        let dir = tempfile::tempdir().unwrap();
        // A missing dir is invalid.
        assert!(!uv_cache_is_valid(&dir.path().join("uv-0.0.0")));
        // An empty (partial/corrupt) dir is invalid -> triggers self-heal.
        assert!(!uv_cache_is_valid(dir.path()));
        // A dir containing the uv binary is valid.
        let bin_name = if cfg!(windows) { "uv.exe" } else { "uv" };
        std::fs::write(dir.path().join(bin_name), b"uv").unwrap();
        assert!(uv_cache_is_valid(dir.path()));
    }
}
