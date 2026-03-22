use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};

use super::{EngineManager, ENGINE_LOCK_DIR};

pub(super) struct EngineInstallLock {
    _file: File,
}

pub(super) struct ConfigUpdateLock {
    _file: File,
}

impl EngineManager {
    pub(super) fn acquire_install_lock(
        &self,
        name: &str,
        version: &str,
    ) -> Result<EngineInstallLock> {
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

pub(super) fn acquire_config_lock() -> Result<ConfigUpdateLock> {
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

#[cfg(test)]
pub(super) fn env_lock() -> &'static std::sync::Mutex<()> {
    static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    ENV_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
