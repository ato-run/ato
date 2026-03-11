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

struct RuntimeInstallLock {
    _file: File,
}

mod fetcher;
mod verifier;

pub use verifier::{ArtifactVerifier, ChecksumVerifier};

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
            _ => None,
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
                )))
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
                )))
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
                )))
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
    let home = dirs::home_dir()
        .ok_or_else(|| CapsuleError::Config("Failed to determine home directory".to_string()))?;
    Ok(home.join(".ato").join("toolchains"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_semverish() {
        assert_eq!(RuntimeFetcher::normalize_semverish("v1.2.3"), "1.2.3");
        assert_eq!(RuntimeFetcher::normalize_semverish("^3.11"), "3.11");
    }
}
