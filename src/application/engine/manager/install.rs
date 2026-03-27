use anyhow::{Context, Result};
use sha2::Digest;
use std::fs;
use std::path::Path;

use super::http::http_get_bytes;
use super::locks::acquire_config_lock;
use super::policy::{configured_nacelle_release_base_url, resolve_auto_bootstrap_policy_from_env};
use super::release::{fetch_latest_nacelle_version_from_base_url, resolve_nacelle_release};
use super::{EngineInstallResult, EngineManager};

impl EngineManager {
    pub fn download_engine(
        &self,
        name: &str,
        version: &str,
        url: &str,
        sha256: &str,
        reporter: &dyn capsule_core::CapsuleReporter,
    ) -> Result<std::path::PathBuf> {
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
            "Tier 2 execution requires nacelle {}, but auto-bootstrap is disabled: {}. Install the matching nacelle runtime and register it before retrying.",
            policy.version,
            reason
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
