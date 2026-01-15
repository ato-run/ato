//! Toolchain detection and version management
//!
//! Detects installed language runtimes and validates version compatibility.
//!
//! v2.0: JIT Provisioning - Downloads and caches runtimes on-demand
//!
//! # Example
//! ```no_run
//! use nacelle::runtime::source::toolchain::RuntimeFetcher;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let fetcher = RuntimeFetcher::new()?;
//!     let python_path = fetcher.ensure_python("3.11").await?;
//!     println!("Python installed at: {:?}", python_path);
//!     Ok(())
//! }
//! ```

mod verifier;

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use fs2::FileExt;
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tracing::{debug, info, warn};

pub use verifier::{ArtifactVerifier, ChecksumVerifier};

/// Information about an installed toolchain
#[derive(Debug, Clone)]
pub struct ToolchainInfo {
    /// Language name (python, node, etc.)
    pub language: String,
    /// Detected version string
    pub version: String,
    /// Path to the binary
    pub path: PathBuf,
}

/// Manages detection and validation of host toolchains
pub struct ToolchainManager {
    /// Cached toolchain lookups
    cache: std::sync::Mutex<std::collections::HashMap<String, Option<ToolchainInfo>>>,
}

impl ToolchainManager {
    pub fn new() -> Self {
        Self {
            cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Find a compatible toolchain for the given language and version constraint
    pub fn find_toolchain(
        &self,
        language: &str,
        version_constraint: Option<&str>,
    ) -> Option<ToolchainInfo> {
        // Check cache first
        {
            let cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(language) {
                if let Some(ref info) = cached {
                    if self.version_matches(&info.version, version_constraint) {
                        return Some(info.clone());
                    }
                }
                // Cached as not found
                if cached.is_none() {
                    return None;
                }
            }
        }

        // Try to detect toolchain
        let info = self.detect_toolchain(language);

        // Cache the result
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(language.to_string(), info.clone());
        }

        // Check version constraint
        if let Some(ref info) = info {
            if self.version_matches(&info.version, version_constraint) {
                return Some(info.clone());
            } else {
                warn!(
                    "Toolchain {} version {} does not match constraint {:?}",
                    language, info.version, version_constraint
                );
                return None;
            }
        }

        None
    }

    /// Detect a toolchain by language name
    fn detect_toolchain(&self, language: &str) -> Option<ToolchainInfo> {
        let (binaries, version_args) = match language.to_lowercase().as_str() {
            "python" => (vec!["python3", "python"], vec!["--version"]),
            "node" | "nodejs" => (vec!["node"], vec!["--version"]),
            "deno" => (vec!["deno"], vec!["--version"]),
            "bun" => (vec!["bun"], vec!["--version"]),
            "ruby" => (vec!["ruby"], vec!["--version"]),
            "perl" => (vec!["perl"], vec!["--version"]),
            _ => {
                warn!("Unknown language: {}", language);
                return None;
            }
        };

        for binary in binaries {
            if let Ok(path) = which::which(binary) {
                // Try to get version
                if let Ok(output) = Command::new(&path).args(&version_args).output() {
                    if output.status.success() {
                        let version_output = String::from_utf8_lossy(&output.stdout);
                        if let Some(version) = self.parse_version(language, &version_output) {
                            debug!("Found {} {} at {:?}", language, version, path);
                            return Some(ToolchainInfo {
                                language: language.to_string(),
                                version,
                                path,
                            });
                        }
                    }
                }
            }
        }

        None
    }

    /// Parse version string from command output
    fn parse_version(&self, language: &str, output: &str) -> Option<String> {
        let output = output.trim();

        match language.to_lowercase().as_str() {
            "python" => {
                // "Python 3.11.4"
                output.strip_prefix("Python ").map(|s| s.trim().to_string())
            }
            "node" | "nodejs" => {
                // "v18.17.0"
                output.strip_prefix('v').map(|s| s.trim().to_string())
            }
            "deno" => {
                // "deno 1.36.0"
                output.strip_prefix("deno ").map(|s| s.trim().to_string())
            }
            "ruby" => {
                // "ruby 3.2.0 ..."
                output
                    .strip_prefix("ruby ")
                    .and_then(|s| s.split_whitespace().next())
                    .map(|s| s.to_string())
            }
            "perl" => {
                // "This is perl 5, version 36, ..."
                // Or "perl, v5.36.0 ..."
                if output.contains("version") {
                    // Extract version number
                    output
                        .split("version")
                        .nth(1)
                        .and_then(|s| s.split(',').next())
                        .map(|s| format!("5.{}", s.trim()))
                } else {
                    output
                        .split('v')
                        .nth(1)
                        .and_then(|s| s.split_whitespace().next())
                        .map(|s| s.to_string())
                }
            }
            _ => Some(output.to_string()),
        }
    }

    /// Check if a version string matches a constraint
    fn version_matches(&self, version: &str, constraint: Option<&str>) -> bool {
        let constraint = match constraint {
            Some(c) => c,
            None => return true, // No constraint = any version
        };

        // Simple version matching
        // Supports: "3.11", "^3.11", ">=3.11", "3.11.4"
        let constraint = constraint.trim();

        if constraint.starts_with('^') {
            // Caret constraint: compatible versions (same major)
            let required = constraint.trim_start_matches('^');
            self.version_compatible(version, required)
        } else if constraint.starts_with(">=") {
            // Minimum version
            let required = constraint.trim_start_matches(">=");
            self.version_gte(version, required)
        } else if constraint.starts_with('>') {
            let required = constraint.trim_start_matches('>');
            self.version_gt(version, required)
        } else if constraint.starts_with("<=") {
            let required = constraint.trim_start_matches("<=");
            self.version_lte(version, required)
        } else if constraint.starts_with('<') {
            let required = constraint.trim_start_matches('<');
            self.version_lt(version, required)
        } else {
            // Exact or prefix match
            version.starts_with(constraint) || version == constraint
        }
    }

    /// Parse version into components
    fn parse_version_parts(&self, version: &str) -> Vec<u32> {
        version
            .split('.')
            .filter_map(|s| s.parse::<u32>().ok())
            .collect()
    }

    /// Check if version is compatible (same major version)
    fn version_compatible(&self, version: &str, required: &str) -> bool {
        let v_parts = self.parse_version_parts(version);
        let r_parts = self.parse_version_parts(required);

        if v_parts.is_empty() || r_parts.is_empty() {
            return false;
        }

        // Major version must match
        if v_parts[0] != r_parts[0] {
            return false;
        }

        // Must be >= required
        self.version_gte(version, required)
    }

    /// Version greater than or equal
    fn version_gte(&self, version: &str, required: &str) -> bool {
        let v_parts = self.parse_version_parts(version);
        let r_parts = self.parse_version_parts(required);

        for (v, r) in v_parts.iter().zip(r_parts.iter()) {
            if v > r {
                return true;
            }
            if v < r {
                return false;
            }
        }

        v_parts.len() >= r_parts.len()
    }

    /// Version greater than
    fn version_gt(&self, version: &str, required: &str) -> bool {
        let v_parts = self.parse_version_parts(version);
        let r_parts = self.parse_version_parts(required);

        for (v, r) in v_parts.iter().zip(r_parts.iter()) {
            if v > r {
                return true;
            }
            if v < r {
                return false;
            }
        }

        v_parts.len() > r_parts.len()
    }

    /// Version less than or equal
    fn version_lte(&self, version: &str, required: &str) -> bool {
        !self.version_gt(version, required)
    }

    /// Version less than
    fn version_lt(&self, version: &str, required: &str) -> bool {
        !self.version_gte(version, required)
    }
}

impl Default for ToolchainManager {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// v2.0: JIT Provisioning - Runtime Fetcher
// ═══════════════════════════════════════════════════════════════════════════

/// JIT Fetcher for downloading and caching runtimes on-demand
pub struct RuntimeFetcher {
    cache_dir: PathBuf,
    verifier: Arc<dyn ArtifactVerifier>,
    fetchers: HashMap<&'static str, Box<dyn fetcher::ToolchainFetcher>>,
}

struct RuntimeInstallLock {
    file: File,
}

impl Drop for RuntimeInstallLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn is_internal_mode() -> bool {
    std::env::var_os("NACELLE_INTERNAL").is_some()
}

macro_rules! toolchain_out {
    ($($arg:tt)*) => {{
        if $crate::runtime::source::toolchain::is_internal_mode() {
            eprintln!($($arg)*);
        } else {
            println!($($arg)*);
        }
    }};
}

mod fetcher;

impl RuntimeFetcher {
    /// Create a new RuntimeFetcher with the default cache directory
    pub fn new() -> Result<Self> {
        let cache_dir = crate::common::paths::toolchain_cache_dir()?;

        fs::create_dir_all(&cache_dir).context("Failed to create toolchain cache directory")?;

        Ok(Self {
            cache_dir,
            verifier: Arc::new(ChecksumVerifier::default()),
            fetchers: fetcher::default_fetchers(),
        })
    }

    /// Create a RuntimeFetcher with a custom verifier.
    /// Useful for tests and for swapping in signature-capable verifiers.
    pub fn new_with_verifier(verifier: Arc<dyn ArtifactVerifier>) -> Result<Self> {
        let cache_dir = crate::common::paths::toolchain_cache_dir()?;

        fs::create_dir_all(&cache_dir).context("Failed to create toolchain cache directory")?;

        Ok(Self {
            cache_dir,
            verifier,
            fetchers: fetcher::default_fetchers(),
        })
    }

    fn canonical_fetcher_key(language: &str) -> Option<&'static str> {
        match language.to_lowercase().as_str() {
            "python" => Some("python"),
            "node" | "nodejs" => Some("node"),
            "deno" => Some("deno"),
            "bun" => Some("bun"),
            _ => None,
        }
    }

    async fn download_runtime_with_progress(
        &self,
        language: &str,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        let key = Self::canonical_fetcher_key(language)
            .with_context(|| format!("Unsupported runtime language: {}", language))?;

        let runtime_dir = self.get_runtime_path(key, version);
        if runtime_dir.exists() {
            return Ok(runtime_dir);
        }

        let _lock = self
            .acquire_install_lock(key, version)
            .await
            .with_context(|| format!("Failed to acquire install lock for {} {}", key, version))?;

        // Double-check after acquiring the lock: another process may have completed the install.
        if runtime_dir.exists() {
            return Ok(runtime_dir);
        }

        let fetcher = self
            .fetchers
            .get(key)
            .with_context(|| format!("No runtime fetcher registered for: {}", key))?;

        debug!("Using runtime fetcher: {}", fetcher.language());

        fetcher
            .download_runtime(self, version, show_progress)
            .await
            .with_context(|| format!("Failed to download runtime: {} {}", key, version))
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

    fn lock_path(&self, language: &str, version: &str) -> PathBuf {
        let lock_dir = self.cache_dir.join(".locks");
        let v = Self::sanitize_lock_component(version);
        lock_dir.join(format!("{}-{}.lock", language, v))
    }

    async fn acquire_install_lock(&self, language: &str, version: &str) -> Result<RuntimeInstallLock> {
        let lock_path = self.lock_path(language, version);
        let language = language.to_string();
        let version = version.to_string();

        tokio::task::spawn_blocking(move || -> Result<RuntimeInstallLock> {
            if let Some(parent) = lock_path.parent() {
                fs::create_dir_all(parent).context("Failed to create lock directory")?;
            }

            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(&lock_path)
                .with_context(|| format!("Failed to open lock file: {:?}", lock_path))?;

            match file.try_lock_exclusive() {
                Ok(()) => Ok(RuntimeInstallLock { file }),
                Err(e) if e.kind() == fs2::lock_contended_error().kind() => {
                    toolchain_out!(
                        "⏳ Another process is provisioning {} {}. Waiting...",
                        language,
                        version
                    );

                    // Block until the other process releases the lock.
                    file.lock_exclusive()
                        .with_context(|| format!("Failed to wait for lock: {:?}", lock_path))?;
                    Ok(RuntimeInstallLock { file })
                }
                Err(e) => Err(e).context("Failed to lock runtime install (unexpected error)"),
            }
        })
        .await
        .context("Failed to join lock acquisition task")?
    }

    /// Get cache directory path
    pub fn cache_dir(&self) -> &PathBuf {
        &self.cache_dir
    }

    /// Check if a runtime is already cached
    pub fn is_cached(&self, language: &str, version: &str) -> bool {
        let runtime_dir = self.cache_dir.join(format!("{}-{}", language, version));
        runtime_dir.exists()
    }

    /// Get the path to a cached runtime
    pub fn get_runtime_path(&self, language: &str, version: &str) -> PathBuf {
        self.cache_dir.join(format!("{}-{}", language, version))
    }

    /// Download and extract a Python runtime from indygreg/python-build-standalone
    ///
    /// # Arguments
    /// * `version` - Python version (e.g., "3.11", "3.12.1")
    ///
    /// # Returns
    /// Path to the extracted runtime directory
    pub async fn download_python_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_python_runtime_with_progress(version, true)
            .await
    }

    /// Ensure Python runtime is available, downloading if necessary
    ///
    /// This is the main JIT provisioning entry point.
    /// Returns the path to the python binary (e.g., ~/.capsuled/toolchain/python-3.11/python/bin/python3)
    ///
    /// # Arguments
    /// * `version` - Python version constraint (e.g., "3.11", "3.12")
    ///
    /// # Returns
    /// PathBuf to the python binary
    pub async fn ensure_python(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self
            .download_python_runtime_with_progress(version, true)
            .await?;

        // Find the python binary in the extracted runtime
        let python_bin = Self::find_python_binary(&runtime_dir)?;

        info!("Python {} ready at {:?}", version, python_bin);
        Ok(python_bin)
    }

    /// Download Python runtime with optional progress bar
    async fn download_python_runtime_with_progress(
        &self,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        self.download_runtime_with_progress("python", version, show_progress)
            .await
    }

    async fn fetch_expected_sha256(&self, url: &str, filename_hint: Option<&str>) -> Result<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let response = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to download sha256 file: {}", url))?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to download sha256 file: HTTP {} - {}",
                response.status(),
                url
            );
        }

        let text = response.text().await.context("Failed to read sha256 body")?;
        Self::parse_sha256_from_text(&text, filename_hint)
    }

    fn parse_sha256_from_text(text: &str, filename_hint: Option<&str>) -> Result<String> {
        // Common formats:
        // 1) "<hex>  <filename>" (e.g. SHASUMS256.txt / *.sha256sum)
        // 2) "SHA256 (<filename>) = <hex>"
        // 3) "<hex>" (single-hash sidecars)

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

        // Fallback: extract the first 64-hex token in the entire text.
        for token in text
            .split(|c: char| c.is_whitespace() || c == '=' || c == '(' || c == ')')
            .filter(|s| !s.is_empty())
        {
            let t = token.trim();
            if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(t.to_ascii_lowercase());
            }
        }

        anyhow::bail!("Could not parse sha256 from text")
    }

    fn verify_sha256_of_file(&self, path: &PathBuf, expected_hex: &str) -> Result<()> {
        match self
            .verifier
            .verify_sha256(path.as_path(), expected_hex)
            .with_context(|| format!("Failed to verify sha256 for {:?}", path))
        {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(path);
                Err(e)
            }
        }
    }

    /// Download a file with progress bar display
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
            .context("Failed to connect to download server")?;

        if !response.status().is_success() {
            anyhow::bail!("Download failed: HTTP {} - {}", response.status(), url);
        }

        let total_size = response.content_length().unwrap_or(0);

        // Create progress bar
        let pb = if show_progress && total_size > 0 {
            let pb = ProgressBar::new(total_size);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .expect("Invalid progress bar template")
                    .progress_chars("#>-"),
            );
            Some(pb)
        } else {
            None
        };

        // Create parent directory if needed
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        // Stream download to file
        let mut file = File::create(dest).context("Failed to create download file")?;
        let mut stream = response.bytes_stream();
        let mut downloaded: u64 = 0;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Error reading download stream")?;
            file.write_all(&chunk).context("Failed to write to file")?;
            downloaded += chunk.len() as u64;

            if let Some(ref pb) = pb {
                pb.set_position(downloaded);
            }
        }

        if let Some(pb) = pb {
            pb.finish_with_message("Download complete");
        }

        Ok(())
    }

    /// Find the Python binary in the extracted runtime directory
    fn find_python_binary(runtime_dir: &PathBuf) -> Result<PathBuf> {
        // Standard locations for indygreg/python-build-standalone
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

        anyhow::bail!(
            "Python binary not found in runtime directory: {:?}",
            runtime_dir
        )
    }

    /// Extract a tar.gz archive from file to directory
    fn extract_archive_from_file(archive_path: &PathBuf, dest: &PathBuf) -> Result<()> {
        use flate2::read::GzDecoder;
        use tar::Archive;

        let file = File::open(archive_path)
            .with_context(|| format!("Failed to open archive: {:?}", archive_path))?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);

        archive.unpack(dest).context("Failed to extract archive")?;

        Ok(())
    }

    /// Extract a zip archive from file to directory.
    fn extract_zip_from_file(archive_path: &PathBuf, dest: &PathBuf) -> Result<()> {
        use std::io::copy;
        use zip::ZipArchive;

        let file = File::open(archive_path)
            .with_context(|| format!("Failed to open zip: {:?}", archive_path))?;
        let mut zip = ZipArchive::new(file).context("Failed to read zip archive")?;

        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).context("Failed to read zip entry")?;
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

            let mut outfile = File::create(&out_path)
                .with_context(|| format!("Failed to create extracted file: {:?}", out_path))?;
            copy(&mut entry, &mut outfile)
                .with_context(|| format!("Failed to extract zip entry to {:?}", out_path))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(name) = out_path.file_name().and_then(|s| s.to_str()) {
                    if name == "node" || name == "deno" || name == "bun" {
                        let mut perms = fs::metadata(&out_path)?.permissions();
                        perms.set_mode(0o755);
                        fs::set_permissions(&out_path, perms)?;
                    }
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
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    if candidates.iter().any(|c| c.eq_ignore_ascii_case(name)) {
                        return Ok(Some(path));
                    }
                }
            }
            Ok(None)
        }

        match walk(runtime_dir, candidates).context("Failed to search runtime directory")? {
            Some(p) => Ok(p),
            None => anyhow::bail!(
                "Binary not found in runtime directory: {:?} (candidates={:?})",
                runtime_dir,
                candidates
            ),
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

    /// Detect the current platform (OS and architecture)
    fn detect_platform() -> Result<(String, String)> {
        let os = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            anyhow::bail!("Unsupported OS");
        };

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            anyhow::bail!("Unsupported architecture");
        };

        Ok((os.to_string(), arch.to_string()))
    }

    /// Construct the download URL for indygreg/python-build-standalone
    fn get_python_download_url(version: &str, os: &str, arch: &str) -> Result<String> {
        // Map short versions to full versions
        let full_version = match version {
            "3.11" => "3.11.10",
            "3.12" => "3.12.7",
            "3.13" => "3.13.0rc3",
            _ => version, // Assume full version is provided
        };

        // Construct filename based on platform
        // Format: cpython-{version}+{date}-{triple}-install_only.tar.gz
        // Using 20241002 release tag (date-based tag used by python-build-standalone)
        let build_date = "20241002";

        let (triple, variant) = match (os, arch) {
            ("linux", "x86_64") => ("x86_64-unknown-linux-gnu", "install_only"),
            ("linux", "aarch64") => ("aarch64-unknown-linux-gnu", "install_only"),
            ("macos", "x86_64") => ("x86_64-apple-darwin", "install_only"),
            ("macos", "aarch64") => ("aarch64-apple-darwin", "install_only"),
            ("windows", "x86_64") => ("x86_64-pc-windows-msvc", "shared-install_only"),
            _ => anyhow::bail!("Unsupported platform: {} {}", os, arch),
        };

        let filename = format!(
            "cpython-{}+{}-{}-{}.tar.gz",
            full_version, build_date, triple, variant
        );

        // GitHub releases URL pattern
        let base_url = "https://github.com/astral-sh/python-build-standalone/releases/download";
        let release_tag = build_date;

        Ok(format!("{}/{}/{}", base_url, release_tag, filename))
    }

    /// Ensure Node.js runtime is available, downloading if necessary.
    /// Returns the path to the `node` binary.
    pub async fn ensure_node(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self.download_node_runtime_with_progress(version, true).await?;
        let node_bin = Self::find_binary_recursive(&runtime_dir, &["node", "node.exe"])?;
        info!("Node {} ready at {:?}", version, node_bin);
        Ok(node_bin)
    }

    /// Ensure Deno runtime is available, downloading if necessary.
    /// Returns the path to the `deno` binary.
    pub async fn ensure_deno(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self.download_deno_runtime_with_progress(version, true).await?;
        let deno_bin = Self::find_binary_recursive(&runtime_dir, &["deno", "deno.exe"])?;
        info!("Deno {} ready at {:?}", version, deno_bin);
        Ok(deno_bin)
    }

    /// Ensure Bun runtime is available, downloading if necessary.
    /// Returns the path to the `bun` binary.
    pub async fn ensure_bun(&self, version: &str) -> Result<PathBuf> {
        let runtime_dir = self.download_bun_runtime_with_progress(version, true).await?;
        let bun_bin = Self::find_binary_recursive(&runtime_dir, &["bun", "bun.exe"])?;
        info!("Bun {} ready at {:?}", version, bun_bin);
        Ok(bun_bin)
    }

    /// Download a Node.js runtime and return the extracted runtime directory.
    pub async fn download_node_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_node_runtime_with_progress(version, true).await
    }

    /// Download a Deno runtime and return the extracted runtime directory.
    pub async fn download_deno_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_deno_runtime_with_progress(version, true).await
    }

    /// Download a Bun runtime and return the extracted runtime directory.
    pub async fn download_bun_runtime(&self, version: &str) -> Result<PathBuf> {
        self.download_bun_runtime_with_progress(version, true).await
    }

    async fn resolve_node_full_version(version_hint: &str) -> Result<String> {
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
            anyhow::bail!("Invalid Node version hint: {}", version_hint);
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let response = client
            .get("https://nodejs.org/dist/index.json")
            .send()
            .await
            .context("Failed to download Node index.json")?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to download Node index.json: HTTP {}",
                response.status()
            );
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse Node index.json")?;
        let arr = json
            .as_array()
            .context("Node index.json is not an array")?;

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

        anyhow::bail!("Could not resolve Node version for hint: {}", version_hint)
    }

    fn node_artifact_filename(full_version: &str, os: &str, arch: &str) -> Result<(String, bool)> {
        let (platform, is_zip) = match os {
            "linux" => ("linux", false),
            "macos" => ("darwin", false),
            "windows" => ("win", true),
            _ => anyhow::bail!("Unsupported OS for Node: {}", os),
        };

        let arch = match (os, arch) {
            ("windows", "x86_64") => "x64",
            ("windows", "aarch64") => "arm64",
            (_, "x86_64") => "x64",
            (_, "aarch64") => "arm64",
            _ => anyhow::bail!("Unsupported arch for Node: {}", arch),
        };

        let filename = if is_zip {
            format!("node-v{}-{}-{}.zip", full_version, platform, arch)
        } else {
            format!("node-v{}-{}-{}.tar.gz", full_version, platform, arch)
        };

        Ok((filename, is_zip))
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

    async fn download_bun_runtime_with_progress(
        &self,
        version: &str,
        show_progress: bool,
    ) -> Result<PathBuf> {
        self.download_runtime_with_progress("bun", version, show_progress)
            .await
    }
}

impl Default for RuntimeFetcher {
    fn default() -> Self {
        Self::new().expect("Failed to initialize RuntimeFetcher")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python_version() {
        let tm = ToolchainManager::new();
        assert_eq!(
            tm.parse_version("python", "Python 3.11.4"),
            Some("3.11.4".to_string())
        );
    }

    #[test]
    fn test_parse_node_version() {
        let tm = ToolchainManager::new();
        assert_eq!(
            tm.parse_version("node", "v18.17.0"),
            Some("18.17.0".to_string())
        );
    }

    #[test]
    fn test_version_gte() {
        let tm = ToolchainManager::new();
        assert!(tm.version_gte("3.11.4", "3.11"));
        assert!(tm.version_gte("3.11.4", "3.11.4"));
        assert!(tm.version_gte("3.12.0", "3.11.4"));
        assert!(!tm.version_gte("3.10.0", "3.11"));
    }

    #[test]
    fn test_version_compatible() {
        let tm = ToolchainManager::new();
        // ^3.11 means 3.x where x >= 11
        assert!(tm.version_compatible("3.11.4", "3.11"));
        assert!(tm.version_compatible("3.12.0", "3.11"));
        assert!(!tm.version_compatible("4.0.0", "3.11"));
        assert!(!tm.version_compatible("3.10.0", "3.11"));
    }

    #[test]
    fn test_version_matches() {
        let tm = ToolchainManager::new();

        // No constraint
        assert!(tm.version_matches("3.11.4", None));

        // Exact match
        assert!(tm.version_matches("3.11.4", Some("3.11.4")));
        assert!(tm.version_matches("3.11.4", Some("3.11")));

        // Caret
        assert!(tm.version_matches("3.11.4", Some("^3.11")));
        assert!(!tm.version_matches("3.10.0", Some("^3.11")));

        // Comparison
        assert!(tm.version_matches("3.11.4", Some(">=3.11")));
        assert!(!tm.version_matches("3.10.0", Some(">=3.11")));
    }

    #[tokio::test]
    async fn test_install_lock_blocks_contenders() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache_dir = temp.path().join("toolchain");
        fs::create_dir_all(&cache_dir).expect("create cache dir");

        let fetcher1 = RuntimeFetcher {
            cache_dir: cache_dir.clone(),
            verifier: Arc::new(ChecksumVerifier::default()),
            fetchers: fetcher::default_fetchers(),
        };
        let fetcher2 = RuntimeFetcher {
            cache_dir: cache_dir.clone(),
            verifier: Arc::new(ChecksumVerifier::default()),
            fetchers: fetcher::default_fetchers(),
        };

        let lock1 = fetcher1
            .acquire_install_lock("python", "3.11")
            .await
            .expect("acquire lock1");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            let _lock2 = fetcher2
                .acquire_install_lock("python", "3.11")
                .await
                .expect("acquire lock2");
            let _ = tx.send(());
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err(), "contender should be blocked");

        drop(lock1);

        tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("contender should acquire after release")
            .expect("contender message");
    }
}
