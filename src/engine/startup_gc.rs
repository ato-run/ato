// tracing macros used via fully-qualified paths

/// Cleanup orphaned nacelle cgroups under /sys/fs/cgroup/nacelle.
///
/// - Removes directories whose `cgroup.procs` is empty.
/// - Logs warnings on failure but does not abort startup.
pub fn cleanup_orphan_cgroups() {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = ();
    }

    #[cfg(target_os = "linux")]
    {
        let root = std::path::Path::new("/sys/fs/cgroup/nacelle");
        if !root.exists() {
            return;
        }

        if let Err(err) = cleanup_dir(root) {
            tracing::warn!("Startup GC failed: {}", err);
        }

        cleanup_bpf_links();
    }
}

#[cfg(target_os = "linux")]
fn cleanup_dir(dir: &std::path::Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let procs_path = path.join("cgroup.procs");
        let procs_empty = std::fs::read_to_string(&procs_path)
            .map(|s| s.trim().is_empty())
            .unwrap_or(false);

        if procs_empty {
            if let Err(err) = std::fs::remove_dir_all(&path) {
                tracing::warn!("Failed to remove orphaned cgroup {:?}: {}", path, err);
            } else {
                tracing::info!("Removed orphaned cgroup {:?}", path);
            }
        } else {
            // Recurse into nested cgroups.
            let _ = cleanup_dir(&path);
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn cleanup_bpf_links() {
    let bpf_root = std::path::Path::new("/sys/fs/bpf/nacelle");
    if !bpf_root.exists() {
        return;
    }

    if let Err(err) = std::fs::remove_dir_all(bpf_root) {
        tracing::warn!("Failed to cleanup BPF links at {:?}: {}", bpf_root, err);
    } else {
        tracing::info!("Removed BPF links at {:?}", bpf_root);
    }
}
