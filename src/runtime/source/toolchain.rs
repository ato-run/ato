//! Toolchain detection and version management
//!
//! Detects installed language runtimes and validates version compatibility.
//!
//! v2.0: JIT Provisioning - Downloads and caches runtimes on-demand
//!
//! # Example
//! ```no_run
//! use capsuled::runtime::source::toolchain::RuntimeFetcher;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let fetcher = RuntimeFetcher::new()?;
//!     let python_path = fetcher.ensure_python("3.11").await?;
//!     println!("Python installed at: {:?}", python_path);
//!     Ok(())
//! }
//! ```

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tracing::{debug, info, warn};

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
}

impl RuntimeFetcher {
    /// Create a new RuntimeFetcher with the default cache directory
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::home_dir()
            .context("Failed to determine home directory")?
            .join(".capsuled")
            .join("toolchain");

        fs::create_dir_all(&cache_dir).context("Failed to create toolchain cache directory")?;

        Ok(Self { cache_dir })
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
        let runtime_dir = self.get_runtime_path("python", version);

        // Check if already cached
        if runtime_dir.exists() {
            info!("✓ Python {} already cached", version);
            return Ok(runtime_dir);
        }

        println!("⬇️  Downloading Python {} runtime...", version);

        // Determine the platform-specific URL
        let (os, arch) = Self::detect_platform()?;
        let download_url = Self::get_python_download_url(version, &os, &arch)?;

        debug!("Fetching from: {}", download_url);

        // Download with progress bar
        let archive_path = self.cache_dir.join(format!("python-{}.tar.gz", version));
        self.download_with_progress(&download_url, &archive_path, show_progress)
            .await?;

        // Create temporary extraction directory
        let temp_dir = self.cache_dir.join(format!("tmp-python-{}", version));
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;

        // Extract the archive
        println!("📦 Extracting Python {} runtime...", version);
        Self::extract_archive_from_file(&archive_path, &temp_dir)?;

        // Move to final location
        if runtime_dir.exists() {
            fs::remove_dir_all(&runtime_dir)?;
        }
        fs::rename(&temp_dir, &runtime_dir).context("Failed to move extracted runtime to cache")?;

        // Clean up archive
        let _ = fs::remove_file(&archive_path);

        println!("✓ Python {} installed at {:?}", version, runtime_dir);
        Ok(runtime_dir)
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
            "3.12" => "3.12.8",
            "3.13" => "3.13.1",
            _ => version, // Assume full version is provided
        };

        // Construct filename based on platform
        // Format: cpython-{version}+{date}-{triple}-install_only.tar.gz
        // Using 20241002 release tag
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
        let base_url = "https://github.com/indygreg/python-build-standalone/releases/download";
        let release_tag = build_date;

        Ok(format!("{}/{}/{}", base_url, release_tag, filename))
    }

    /// Download a Node.js runtime (placeholder for future implementation)
    pub async fn download_node_runtime(&self, _version: &str) -> Result<PathBuf> {
        // TODO: Implement Node.js download
        anyhow::bail!("Node.js JIT provisioning not yet implemented")
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
}
