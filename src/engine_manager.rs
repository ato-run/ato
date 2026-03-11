use anyhow::{Context, Result};
use fs2::FileExt;
use sha2::Digest;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

const ENGINES_DIR: &str = ".ato/engines";
const ENGINE_LOCK_DIR: &str = ".locks";
const DEFAULT_NACELLE_RELEASE_BASE_URL: &str = "https://releases.capsule.dev/nacelle";
const AUTO_BOOTSTRAP_ENV: &str = "ATO_NACELLE_AUTO_BOOTSTRAP";
const OFFLINE_ENV: &str = "ATO_OFFLINE";
const DISABLE_NETWORK_BOOTSTRAP_ENV: &str = "ATO_DISABLE_NETWORK_BOOTSTRAP";
const NACELLE_VERSION_ENV: &str = "ATO_NACELLE_VERSION";
const NACELLE_RELEASE_BASE_URL_ENV: &str = "ATO_NACELLE_RELEASE_BASE_URL";

pub const PINNED_NACELLE_VERSION: &str = "v0.2.1";

#[cfg(test)]
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInfo {
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub arch: String,
    pub os: String,
}

#[allow(dead_code)]
pub(crate) struct EngineInstallResult {
    pub version: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NacelleBootstrapPolicy {
    pub version: String,
    pub release_base_url: String,
    pub network_allowed: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoBootstrapMode {
    Auto,
    Force,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NacelleRelease {
    version: String,
    binary_name: String,
    url: String,
    sha256: String,
}

struct EngineInstallLock {
    _file: File,
}

struct ConfigUpdateLock {
    _file: File,
}

pub struct EngineManager {
    engines_dir: PathBuf,
}

fn http_get_bytes(url: &str) -> Result<(u16, Vec<u8>)> {
    let url = url.to_string();
    let panic_url = url.clone();
    std::thread::spawn(move || -> Result<(u16, Vec<u8>)> {
        let response = reqwest::blocking::get(&url)
            .with_context(|| format!("Failed to download from: {}", url))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .with_context(|| format!("Failed to read response body from: {}", url))?;
        Ok((status, bytes.to_vec()))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("HTTP worker thread panicked while fetching {}", panic_url))?
}

fn http_get_text(url: &str) -> Result<(u16, String)> {
    let url = url.to_string();
    let panic_url = url.clone();
    std::thread::spawn(move || -> Result<(u16, String)> {
        let response = reqwest::blocking::get(&url)
            .with_context(|| format!("Failed to download from: {}", url))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .with_context(|| format!("Failed to read response body from: {}", url))?;
        Ok((status, text))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("HTTP worker thread panicked while fetching {}", panic_url))?
}

impl EngineManager {
    pub fn new() -> Result<Self> {
        let engines_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?
            .join(ENGINES_DIR);

        if !engines_dir.exists() {
            fs::create_dir_all(&engines_dir).with_context(|| {
                format!(
                    "Failed to create engines directory: {}",
                    engines_dir.display()
                )
            })?;
        }

        Ok(Self { engines_dir })
    }

    pub fn engine_path(&self, name: &str, version: &str) -> PathBuf {
        self.engines_dir.join(format!("{}-{}", name, version))
    }

    #[cfg(test)]
    fn parse_engine_filename(&self, filename: &str) -> Option<EngineInfo> {
        let parts: Vec<&str> = filename.split('-').collect();
        if parts.len() < 3 {
            return None;
        }

        let name = parts[0];
        let version = parts[1];
        let os_arch = parts[2..].join("-");

        let (os, arch) = if os_arch.contains('-') {
            let os_arch_parts: Vec<&str> = os_arch.splitn(2, '-').collect();
            (os_arch_parts[0], os_arch_parts[1])
        } else {
            ("unknown", os_arch.as_str())
        };

        Some(EngineInfo {
            name: name.to_string(),
            version: version.to_string(),
            url: String::new(),
            sha256: String::new(),
            arch: arch.to_string(),
            os: os.to_string(),
        })
    }

    pub fn download_engine(
        &self,
        name: &str,
        version: &str,
        url: &str,
        sha256: &str,
        reporter: &dyn capsule_core::CapsuleReporter,
    ) -> Result<PathBuf> {
        let _lock = self.acquire_install_lock(name, version)?;
        let output_path = self.engine_path(name, version);

        if output_path.exists() {
            futures::executor::block_on(
                reporter.notify(format!("✅ Engine {} v{} already installed", name, version)),
            )?;
            return Ok(output_path);
        }

        futures::executor::block_on(
            reporter.notify(format!("⬇️  Downloading {} v{}...", name, version)),
        )?;

        let temp_path = output_path.with_extension("tmp");
        let _ = fs::remove_file(&temp_path);

        let (status, content) = http_get_bytes(url)?;

        if !(200..300).contains(&status) {
            anyhow::bail!("Download failed with status: {}", status);
        }

        if !sha256.is_empty() {
            let actual_sha256 = sha2::Sha256::digest(&content)
                .as_slice()
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>();

            if actual_sha256 != sha256 {
                anyhow::bail!(
                    "SHA256 mismatch: expected {}, got {}",
                    sha256,
                    actual_sha256
                );
            }
        }

        fs::write(&temp_path, &content)
            .with_context(|| format!("Failed to write to: {}", temp_path.display()))?;

        fs::rename(&temp_path, &output_path)
            .with_context(|| format!("Failed to move to: {}", output_path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&output_path, fs::Permissions::from_mode(0o755)).with_context(
                || {
                    format!(
                        "Failed to set executable permission on: {}",
                        output_path.display()
                    )
                },
            )?;
        }

        futures::executor::block_on(reporter.notify(format!(
            "✅ Installed {} v{} to {}",
            name,
            version,
            output_path.display()
        )))?;

        Ok(output_path)
    }

    fn acquire_install_lock(&self, name: &str, version: &str) -> Result<EngineInstallLock> {
        let lock_dir = self.engines_dir.join(ENGINE_LOCK_DIR);
        fs::create_dir_all(&lock_dir)
            .with_context(|| format!("Failed to create lock dir: {}", lock_dir.display()))?;

        let lock_path = lock_dir.join(format!("{}-{}.lock", name, version));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("Failed to open lock file: {}", lock_path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("Failed to lock engine install: {}", lock_path.display()))?;

        Ok(EngineInstallLock { _file: file })
    }
}

fn acquire_config_lock() -> Result<ConfigUpdateLock> {
    let config_dir = capsule_core::config::config_dir()?;
    fs::create_dir_all(&config_dir)
        .with_context(|| format!("Failed to create config dir: {}", config_dir.display()))?;

    let lock_path = config_dir.join("config.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open config lock: {}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("Failed to lock config updates: {}", lock_path.display()))?;

    Ok(ConfigUpdateLock { _file: file })
}

#[allow(dead_code)]
pub(crate) fn install_engine_release(
    engine: &str,
    requested_version: Option<&str>,
    skip_verify: bool,
    reporter: &dyn capsule_core::CapsuleReporter,
) -> Result<EngineInstallResult> {
    match engine {
        "nacelle" => install_nacelle_release(requested_version, skip_verify, reporter),
        _ => anyhow::bail!(
            "Unknown engine: {}. Currently only 'nacelle' is supported.",
            engine
        ),
    }
}

pub(crate) fn auto_bootstrap_nacelle(
    reporter: &dyn capsule_core::CapsuleReporter,
) -> Result<EngineInstallResult> {
    let policy = resolve_auto_bootstrap_policy_from_env();
    if !policy.network_allowed {
        let reason = policy
            .disabled_reason
            .unwrap_or_else(|| "auto-bootstrap policy disabled network access".to_string());
        anyhow::bail!(
            "Tier 2 execution requires nacelle {}, but auto-bootstrap is disabled: {}. Run `ato config engine install --engine nacelle --version {}` first.",
            policy.version,
            reason,
            policy.version
        );
    }

    futures::executor::block_on(reporter.notify(format!(
        "⚙️  Bootstrapping compatible nacelle engine {}...",
        policy.version
    )))?;

    install_nacelle_release_with_base_url(
        Some(policy.version.as_str()),
        false,
        reporter,
        &policy.release_base_url,
    )
}

pub(crate) fn resolve_auto_bootstrap_policy_from_env() -> NacelleBootstrapPolicy {
    let mode = parse_auto_bootstrap_mode(std::env::var(AUTO_BOOTSTRAP_ENV).ok().as_deref());
    let version =
        non_empty_env(NACELLE_VERSION_ENV).unwrap_or_else(|| PINNED_NACELLE_VERSION.to_string());
    let release_base_url = configured_nacelle_release_base_url();
    let ci = env_is_truthy("CI");
    let offline = env_is_truthy(OFFLINE_ENV) || env_is_truthy(DISABLE_NETWORK_BOOTSTRAP_ENV);

    resolve_auto_bootstrap_policy(mode, version, release_base_url, ci, offline)
}

fn fetch_latest_nacelle_version_from_base_url(release_base_url: &str) -> Result<String> {
    let (status, resp) = http_get_text(&format!(
        "{}/latest.txt",
        release_base_url.trim_end_matches('/')
    ))
    .context("Failed to fetch latest nacelle version")?;
    if !(200..300).contains(&status) {
        anyhow::bail!(
            "Latest nacelle version download failed with status: {}",
            status
        );
    }
    let version = resp.trim();
    if version.is_empty() {
        anyhow::bail!("Latest nacelle version response was empty");
    }
    Ok(version.to_string())
}

pub(crate) fn fetch_release_sha256(base_url: &str, binary_name: &str) -> Result<String> {
    let checksum_urls = [
        format!("{}/{}.sha256", base_url, binary_name),
        format!("{}/SHA256SUMS", base_url),
        format!("{}/SHA256SUMS.txt", base_url),
        format!("{}/sha256sums.txt", base_url),
    ];

    for checksum_url in checksum_urls {
        let (status, body) = match http_get_text(&checksum_url) {
            Ok(response) => response,
            Err(_) => continue,
        };
        if !(200..300).contains(&status) {
            continue;
        }

        if let Some(hash) = parse_sha256_for_artifact(&body, binary_name) {
            return Ok(hash);
        }

        if checksum_url.ends_with(".sha256") {
            if let Some(hash) = extract_first_sha256_hex(&body) {
                return Ok(hash);
            }
        }
    }

    anyhow::bail!(
        "Failed to resolve SHA256 for {} (checked release checksum endpoints)",
        binary_name
    )
}

pub(crate) fn parse_sha256_for_artifact(checksum_body: &str, binary_name: &str) -> Option<String> {
    for line in checksum_body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some((name, hash)) = trimmed
            .strip_prefix("SHA256 (")
            .and_then(|rest| rest.split_once(") = "))
            .or_else(|| {
                trimmed
                    .strip_prefix("sha256 (")
                    .and_then(|rest| rest.split_once(") = "))
            })
        {
            if name.trim() == binary_name && is_sha256_hex(hash) {
                return Some(hash.to_ascii_lowercase());
            }
            continue;
        }

        let mut parts = trimmed.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        let normalized_name = name.trim_start_matches('*');
        if normalized_name == binary_name && is_sha256_hex(hash) {
            return Some(hash.to_ascii_lowercase());
        }
    }

    None
}

pub(crate) fn extract_first_sha256_hex(raw: &str) -> Option<String> {
    raw.split_whitespace()
        .find(|token| is_sha256_hex(token))
        .map(|value| value.to_ascii_lowercase())
}

#[allow(dead_code)]
fn install_nacelle_release(
    requested_version: Option<&str>,
    skip_verify: bool,
    reporter: &dyn capsule_core::CapsuleReporter,
) -> Result<EngineInstallResult> {
    install_nacelle_release_with_base_url(
        requested_version,
        skip_verify,
        reporter,
        &configured_nacelle_release_base_url(),
    )
}

fn install_nacelle_release_with_base_url(
    requested_version: Option<&str>,
    skip_verify: bool,
    reporter: &dyn capsule_core::CapsuleReporter,
    release_base_url: &str,
) -> Result<EngineInstallResult> {
    let resolved_version = match requested_version {
        Some(version) if !version.trim().is_empty() && version != "latest" => version.to_string(),
        _ => fetch_latest_nacelle_version_from_base_url(release_base_url)?,
    };
    let release = resolve_nacelle_release(&resolved_version, release_base_url, skip_verify)?;

    let engine_manager = EngineManager::new()?;
    let path = engine_manager.download_engine(
        "nacelle",
        &release.version,
        &release.url,
        &release.sha256,
        reporter,
    )?;
    register_engine("nacelle", &path, true)?;

    Ok(EngineInstallResult {
        version: release.version,
        path,
    })
}

fn resolve_nacelle_release(
    requested_version: &str,
    release_base_url: &str,
    skip_verify: bool,
) -> Result<NacelleRelease> {
    let (os, arch) = host_platform_parts()?;
    let normalized_base = release_base_url.trim_end_matches('/');
    let binary_name = format!("nacelle-{}-{}-{}", requested_version, os, arch);
    let version_base_url = format!("{}/{}", normalized_base, requested_version);
    let url = format!("{}/{}", version_base_url, binary_name);
    let sha256 = if skip_verify {
        String::new()
    } else {
        fetch_release_sha256(&version_base_url, &binary_name)?
    };

    Ok(NacelleRelease {
        version: requested_version.to_string(),
        binary_name,
        url,
        sha256,
    })
}

fn host_platform_parts() -> Result<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        anyhow::bail!("Unsupported OS");
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        anyhow::bail!("Unsupported architecture");
    };
    Ok((os, arch))
}

fn register_engine(name: &str, path: &Path, set_default_if_missing: bool) -> Result<()> {
    let _lock = acquire_config_lock()?;
    let mut cfg = capsule_core::config::load_config()?;
    cfg.engines.insert(
        name.to_string(),
        capsule_core::config::EngineRegistration {
            path: path.display().to_string(),
        },
    );
    if set_default_if_missing && cfg.default_engine.is_none() {
        cfg.default_engine = Some(name.to_string());
    }
    capsule_core::config::save_config(&cfg)?;
    Ok(())
}

fn resolve_auto_bootstrap_policy(
    mode: AutoBootstrapMode,
    version: String,
    release_base_url: String,
    ci: bool,
    offline: bool,
) -> NacelleBootstrapPolicy {
    let disabled_reason = if mode == AutoBootstrapMode::Disabled {
        Some(format!("{} disables network bootstrap", AUTO_BOOTSTRAP_ENV))
    } else if offline {
        Some(format!(
            "{} or {} is set",
            OFFLINE_ENV, DISABLE_NETWORK_BOOTSTRAP_ENV
        ))
    } else if ci && mode != AutoBootstrapMode::Force {
        Some("CI environment requires prefetched nacelle".to_string())
    } else {
        None
    };

    NacelleBootstrapPolicy {
        version,
        release_base_url,
        network_allowed: disabled_reason.is_none(),
        disabled_reason,
    }
}

fn parse_auto_bootstrap_mode(value: Option<&str>) -> AutoBootstrapMode {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return AutoBootstrapMode::Auto;
    };
    let normalized = value.to_ascii_lowercase();
    match normalized.as_str() {
        "0" | "false" | "off" | "never" | "disable" | "disabled" => AutoBootstrapMode::Disabled,
        "1" | "true" | "on" | "always" | "force" | "enabled" => AutoBootstrapMode::Force,
        _ => AutoBootstrapMode::Auto,
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn configured_nacelle_release_base_url() -> String {
    non_empty_env(NACELLE_RELEASE_BASE_URL_ENV)
        .unwrap_or_else(|| DEFAULT_NACELLE_RELEASE_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn env_is_truthy(key: &str) -> bool {
    non_empty_env(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_engine_filename() {
        let em = EngineManager::new().unwrap();
        let info = em
            .parse_engine_filename("nacelle-v1.2.3-darwin-x64")
            .unwrap();
        assert_eq!(info.name, "nacelle");
        assert_eq!(info.version, "v1.2.3");
        assert_eq!(info.os, "darwin");
        assert_eq!(info.arch, "x64");
    }

    #[test]
    fn test_parse_engine_filename_linux_arm64() {
        let em = EngineManager::new().unwrap();
        let info = em
            .parse_engine_filename("nacelle-v2.0.0-linux-arm64")
            .unwrap();
        assert_eq!(info.name, "nacelle");
        assert_eq!(info.version, "v2.0.0");
        assert_eq!(info.os, "linux");
        assert_eq!(info.arch, "arm64");
    }

    #[test]
    fn test_parse_engine_filename_invalid() {
        let em = EngineManager::new().unwrap();
        let info = em.parse_engine_filename("invalid");
        assert!(info.is_none());
    }

    #[test]
    fn test_parse_engine_filename_too_short() {
        let em = EngineManager::new().unwrap();
        let info = em.parse_engine_filename("nacelle-v1");
        assert!(info.is_none());
    }

    #[test]
    fn test_engine_path() {
        let em = EngineManager::new().unwrap();
        let path = em.engine_path("nacelle", "v1.2.3");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("nacelle-v1.2.3"));
        assert!(path_str.contains(".ato/engines"));
    }

    #[test]
    fn test_engine_info_serialization() {
        let info = EngineInfo {
            name: "nacelle".to_string(),
            version: "v1.2.3".to_string(),
            url: "https://example.com/nacelle".to_string(),
            sha256: "abc123".to_string(),
            arch: "x64".to_string(),
            os: "darwin".to_string(),
        };

        let serialized = serde_json::to_string(&info).expect("Failed to serialize");
        let deserialized: EngineInfo =
            serde_json::from_str(&serialized).expect("Failed to deserialize");

        assert_eq!(info.name, deserialized.name);
        assert_eq!(info.version, deserialized.version);
        assert_eq!(info.url, deserialized.url);
        assert_eq!(info.sha256, deserialized.sha256);
        assert_eq!(info.arch, deserialized.arch);
        assert_eq!(info.os, deserialized.os);
    }

    #[test]
    fn parse_sha256_for_artifact_supports_sha256sums_format() {
        let body = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  nacelle-v1.2.3-darwin-arm64
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  nacelle-v1.2.3-linux-x64
";
        let parsed = parse_sha256_for_artifact(body, "nacelle-v1.2.3-linux-x64");
        assert_eq!(
            parsed.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn parse_sha256_for_artifact_supports_bsd_style_format() {
        let body = "SHA256 (nacelle-v1.2.3-darwin-arm64) = CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let parsed = parse_sha256_for_artifact(body, "nacelle-v1.2.3-darwin-arm64");
        assert_eq!(
            parsed.as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
        );
    }

    #[test]
    fn extract_first_sha256_hex_reads_single_file_checksum() {
        let body = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  nacelle-v1.2.3-darwin-arm64";
        let parsed = extract_first_sha256_hex(body);
        assert_eq!(
            parsed.as_deref(),
            Some("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
        );
    }

    #[test]
    fn auto_bootstrap_policy_defaults_to_pinned_release() {
        let policy = resolve_auto_bootstrap_policy(
            AutoBootstrapMode::Auto,
            PINNED_NACELLE_VERSION.to_string(),
            DEFAULT_NACELLE_RELEASE_BASE_URL.to_string(),
            false,
            false,
        );
        assert!(policy.network_allowed);
        assert_eq!(policy.version, PINNED_NACELLE_VERSION);
        assert_eq!(policy.release_base_url, DEFAULT_NACELLE_RELEASE_BASE_URL);
    }

    #[test]
    fn auto_bootstrap_policy_disables_network_in_ci_by_default() {
        let policy = resolve_auto_bootstrap_policy(
            AutoBootstrapMode::Auto,
            PINNED_NACELLE_VERSION.to_string(),
            DEFAULT_NACELLE_RELEASE_BASE_URL.to_string(),
            true,
            false,
        );
        assert!(!policy.network_allowed);
        assert_eq!(
            policy.disabled_reason.as_deref(),
            Some("CI environment requires prefetched nacelle")
        );
    }

    #[test]
    fn auto_bootstrap_policy_force_mode_overrides_ci() {
        let policy = resolve_auto_bootstrap_policy(
            AutoBootstrapMode::Force,
            PINNED_NACELLE_VERSION.to_string(),
            DEFAULT_NACELLE_RELEASE_BASE_URL.to_string(),
            true,
            false,
        );
        assert!(policy.network_allowed);
        assert!(policy.disabled_reason.is_none());
    }

    #[test]
    fn auto_bootstrap_policy_reads_env_overrides() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var(AUTO_BOOTSTRAP_ENV, "true");
        std::env::set_var(NACELLE_VERSION_ENV, "v9.9.9");
        std::env::set_var(
            NACELLE_RELEASE_BASE_URL_ENV,
            "https://mirror.example.com/nacelle/",
        );
        std::env::set_var("CI", "true");

        let policy = resolve_auto_bootstrap_policy_from_env();
        assert!(policy.network_allowed);
        assert_eq!(policy.version, "v9.9.9");
        assert_eq!(
            policy.release_base_url,
            "https://mirror.example.com/nacelle"
        );

        std::env::remove_var(AUTO_BOOTSTRAP_ENV);
        std::env::remove_var(NACELLE_VERSION_ENV);
        std::env::remove_var(NACELLE_RELEASE_BASE_URL_ENV);
        std::env::remove_var("CI");
    }
}
