use anyhow::{Context, Result};
use sha2::Digest;
use std::fs;
use std::future::Future;
use std::io::{Cursor, Read};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll, RawWaker, RawWakerVTable, Waker};

use super::http::http_get_bytes;
use super::locks::acquire_config_lock;
use super::policy::{configured_nacelle_release_base_url, resolve_auto_bootstrap_policy_from_env};
use super::release::{fetch_latest_nacelle_version_from_base_url, resolve_nacelle_release};
use super::{EngineInstallResult, EngineManager};

/// Drive an async future to completion synchronously without entering
/// the futures-executor `LocalPool`.
///
/// Why we can't just use `futures::executor::block_on`:
/// `auto_bootstrap_nacelle` and `download_engine` are reached
/// transitively from inside a `tokio::runtime::Runtime::block_on(...)`
/// call on the orchestration session-start path
/// (`session::start_orchestration_session_in_process` →
/// `runtime_handle.block_on(execute_until_ready_and_detach)` →
/// `OrchestratorStartupRuntime::start_service` →
/// `preflight_native_sandbox` → `auto_bootstrap_nacelle`). Tokio's
/// `block_on` sets `futures_executor::enter()`'s thread-local guard,
/// and the legacy `futures::executor::block_on(reporter.notify(...))`
/// then panics with "cannot execute LocalPool from within another
/// executor: EnterError". The panic only fires on first-time nacelle
/// download — cached nacelle short-circuits the notify path entirely,
/// which is why the regression escaped existing tests.
///
/// Why we can't move the future to a fresh thread either:
/// `CapsuleReporter::notify(&self, ...)` is `async fn` — the returned
/// future borrows `&self` for an anonymous lifetime, so it's neither
/// `Send + 'static` nor cloneable. We can't `std::thread::spawn`
/// it.
///
/// What we do instead: hand-roll a single-poll driver with a no-op
/// waker. `CliReporter::notify` (the only reporter implementation
/// reachable from the CLI's nacelle bootstrap path) is structurally
/// synchronous — it serialises the message and `eprintln!`s it, with
/// no `.await` between. So polling the returned future exactly once
/// resolves it. Falls back to `panic!` if the future returns
/// `Pending` so a future async-reporter regression fails loudly here
/// instead of silently dropping notifications. This trade-off is
/// fine because the alternative — making the reporter trait return
/// `Send + 'static` futures or pinning a multi_thread tokio runtime
/// for these sync helpers — is a much larger refactor than the
/// notification-emission path warrants.
fn drive_sync_async<F: Future>(future: F) -> F::Output {
    // `RawWaker` with all-no-op vtable. Constructing a waker by hand
    // sidesteps `futures-executor`'s `enter()` guard entirely — the
    // panic we're avoiding lives inside that crate's `LocalPool`,
    // which we don't touch here at all.
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    let raw = RawWaker::new(std::ptr::null(), &VTABLE);
    // Safety: the vtable above is `'static` and all functions are
    // no-ops, satisfying the contract of `RawWaker::new`.
    let waker = unsafe { Waker::from_raw(raw) };
    let mut cx = TaskContext::from_waker(&waker);

    // Pin on the stack and poll once.
    let mut future = std::pin::pin!(future);
    match Future::poll(future.as_mut() as Pin<&mut F>, &mut cx) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!(
            "drive_sync_async: reporter future returned Pending on first poll. \
             This helper assumes structurally-synchronous reporters; if a \
             reporter implementation now actually awaits, the engine bootstrap \
             notify path needs to be made async end-to-end."
        ),
    }
}

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
            drive_sync_async(
                reporter.notify(format!("✅ Engine {} {} already installed", name, version)),
            )?;
            return Ok(output_path);
        }

        drive_sync_async(reporter.notify(format!("⬇️  Downloading {} {}...", name, version)))?;

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

        if url.ends_with(".tar.xz") {
            write_binary_from_tar_xz(&content, name, &temp_path)?;
        } else if url.ends_with(".zip") {
            write_binary_from_zip(&content, name, &temp_path)?;
        } else {
            fs::write(&temp_path, &content)
                .with_context(|| format!("Failed to write to: {}", temp_path.display()))?;
        }

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

        drive_sync_async(reporter.notify(format!(
            "✅ Installed {} {} to {}",
            name,
            version,
            output_path.display()
        )))?;

        Ok(output_path)
    }
}

fn write_binary_from_tar_xz(content: &[u8], engine_name: &str, output_path: &Path) -> Result<()> {
    let decoder = xz2::read::XzDecoder::new(Cursor::new(content));
    let mut archive = tar::Archive::new(decoder);
    let binary_file_name = engine_binary_file_name(engine_name);

    for entry in archive.entries().context("Failed to read engine archive")? {
        let mut entry = entry.context("Failed to read engine archive entry")?;
        let path = entry
            .path()
            .context("Failed to read engine archive entry path")?;
        if path.file_name().and_then(|name| name.to_str()) != Some(binary_file_name.as_str()) {
            continue;
        }

        let mut out = fs::File::create(output_path)
            .with_context(|| format!("Failed to create: {}", output_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("Failed to extract engine to: {}", output_path.display()))?;
        return Ok(());
    }

    anyhow::bail!(
        "Engine archive did not contain expected binary '{}'",
        binary_file_name
    )
}

fn write_binary_from_zip(content: &[u8], engine_name: &str, output_path: &Path) -> Result<()> {
    let cursor = Cursor::new(content);
    let mut archive = zip::ZipArchive::new(cursor).context("Failed to read engine zip archive")?;
    let binary_file_name = engine_binary_file_name(engine_name);

    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .context("Failed to read engine zip entry")?;
        let Some(name) = Path::new(file.name())
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        if name != binary_file_name {
            continue;
        }

        let mut out = fs::File::create(output_path)
            .with_context(|| format!("Failed to create: {}", output_path.display()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .context("Failed to read engine binary from zip")?;
        std::io::copy(&mut Cursor::new(bytes), &mut out)
            .with_context(|| format!("Failed to extract engine to: {}", output_path.display()))?;
        return Ok(());
    }

    anyhow::bail!(
        "Engine zip archive did not contain expected binary '{}'",
        binary_file_name
    )
}

fn engine_binary_file_name(engine_name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{engine_name}.exe")
    } else {
        engine_name.to_string()
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
    let boundary = policy.bootstrap_boundary();
    if !boundary.network_policy.network_allowed {
        let reason = policy
            .disabled_reason
            .clone()
            .unwrap_or_else(|| "auto-bootstrap policy disabled network access".to_string());
        anyhow::bail!(
            "Tier 2 execution requires nacelle {}, but auto-bootstrap is disabled: {}. Install the matching nacelle runtime and register it before retrying.",
            policy.version,
            reason
        );
    }

    drive_sync_async(reporter.notify(format!(
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
