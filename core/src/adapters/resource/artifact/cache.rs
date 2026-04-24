use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRuntime {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub path: PathBuf,
    pub last_used: SystemTime,
    pub size_bytes: u64,
}

/// Manages local cache of runtime artifacts.
#[derive(Debug)]
pub struct ArtifactCache {
    base_path: PathBuf,
}

impl ArtifactCache {
    pub fn new(base_path: PathBuf) -> Self {
        Self { base_path }
    }

    pub fn get_runtime_path(&self, name: &str, version: &str, platform: &str) -> PathBuf {
        self.base_path.join(name).join(version).join(platform)
    }

    pub async fn exists(&self, name: &str, version: &str, platform: &str) -> bool {
        let path = self.get_runtime_path(name, version, platform);
        let marker = path.join(".binary_path");
        path.exists() && marker.exists()
    }

    pub async fn list_cached(&self) -> Vec<CachedRuntime> {
        let mut cached = Vec::new();
        // TODO: Walk directory structure to find cached runtimes
        // ~/.gumball/runtimes/{name}/{version}/{platform}/

        let mut entries = match fs::read_dir(&self.base_path).await {
            Ok(e) => e,
            Err(_) => return cached,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(file_type) = entry.file_type().await {
                if file_type.is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Iterate versions
                    if let Ok(mut ver_entries) = fs::read_dir(entry.path()).await {
                        while let Ok(Some(ver_entry)) = ver_entries.next_entry().await {
                            if let Ok(ver_type) = ver_entry.file_type().await {
                                if ver_type.is_dir() {
                                    let version =
                                        ver_entry.file_name().to_string_lossy().to_string();
                                    // Iterate platforms
                                    if let Ok(mut plat_entries) =
                                        fs::read_dir(ver_entry.path()).await
                                    {
                                        while let Ok(Some(plat_entry)) =
                                            plat_entries.next_entry().await
                                        {
                                            if let Ok(plat_type) = plat_entry.file_type().await {
                                                if plat_type.is_dir() {
                                                    let platform = plat_entry
                                                        .file_name()
                                                        .to_string_lossy()
                                                        .to_string();
                                                    let path = plat_entry.path();

                                                    // Basic metadata (could be improved)
                                                    let metadata = fs::metadata(&path).await.ok();
                                                    let last_used = metadata
                                                        .as_ref()
                                                        .and_then(|m| m.accessed().ok())
                                                        .unwrap_or(SystemTime::now());
                                                    let size_bytes = 0; // TODO: Calculate size

                                                    cached.push(CachedRuntime {
                                                        name: name.clone(),
                                                        version: version.clone(),
                                                        platform,
                                                        path,
                                                        last_used,
                                                        size_bytes,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        cached
    }

    pub async fn clear_cache(&self, name: &str) -> std::io::Result<()> {
        let path = self.base_path.join(name);
        if path.exists() {
            fs::remove_dir_all(path).await?;
        }
        Ok(())
    }
}
