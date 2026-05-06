use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::SystemTime;

const PORT_MAP_FILE: &str = "port_map.json";
const PORT_RANGE_START: u16 = 10000;
const PORT_RANGE_END: u16 = 19999;
const RUN_DIR: &str = ".ato/run";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PortAllocation {
    port: u16,
    pid: u32,
    allocated_at: SystemTime,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PortMap {
    allocations: HashMap<String, PortAllocation>,
}

pub struct PortManager {
    map_path: PathBuf,
}

impl PortManager {
    pub fn new() -> Result<Self> {
        let run_dir = capsule_core::common::paths::ato_path_or_workspace_tmp("run");

        if !run_dir.exists() {
            fs::create_dir_all(&run_dir).with_context(|| {
                format!("Failed to create run directory: {}", run_dir.display())
            })?;
        }

        Ok(Self {
            map_path: run_dir.join(PORT_MAP_FILE),
        })
    }

    /// Resolve a unique port for the given capsule identity.
    ///
    /// 1. If already allocated and port is free → return (sticky)
    /// 2. Compute preferred port via hash
    /// 3. If preferred is free → allocate
    /// 4. Otherwise scan forward for next free port
    pub fn resolve_port(&self, identity: &str) -> Result<u16> {
        let mut map = self.load_map();

        // Sticky: reuse existing allocation if port is still free
        if let Some(alloc) = map.allocations.get(identity) {
            if is_port_available(alloc.port) {
                // Update PID to current process
                let port = alloc.port;
                map.allocations.insert(
                    identity.to_string(),
                    PortAllocation {
                        port,
                        pid: std::process::id(),
                        allocated_at: SystemTime::now(),
                    },
                );
                self.save_map(&map)?;
                return Ok(port);
            }
        }

        // Compute deterministic preferred port
        let preferred = preferred_port(identity);

        // Try preferred port first
        if is_port_available(preferred) && !is_allocated(&map, preferred) {
            map.allocations.insert(
                identity.to_string(),
                PortAllocation {
                    port: preferred,
                    pid: std::process::id(),
                    allocated_at: SystemTime::now(),
                },
            );
            self.save_map(&map)?;
            return Ok(preferred);
        }

        // Scan forward from preferred
        let range_size = PORT_RANGE_END - PORT_RANGE_START + 1;
        for offset in 1..range_size {
            let candidate = PORT_RANGE_START + (preferred - PORT_RANGE_START + offset) % range_size;
            if is_port_available(candidate) && !is_allocated(&map, candidate) {
                map.allocations.insert(
                    identity.to_string(),
                    PortAllocation {
                        port: candidate,
                        pid: std::process::id(),
                        allocated_at: SystemTime::now(),
                    },
                );
                self.save_map(&map)?;
                return Ok(candidate);
            }
        }

        anyhow::bail!("No available port in range {PORT_RANGE_START}-{PORT_RANGE_END}")
    }

    /// Release a port allocation for the given identity.
    #[allow(dead_code)]
    pub fn release(&self, identity: &str) -> Result<()> {
        let mut map = self.load_map();
        map.allocations.remove(identity);
        self.save_map(&map)
    }

    /// Remove allocations whose PID is no longer alive.
    pub fn gc(&self) -> Result<usize> {
        let mut map = self.load_map();
        let before = map.allocations.len();
        map.allocations
            .retain(|_, alloc| alloc.pid == 0 || is_pid_alive(alloc.pid));
        let removed = before - map.allocations.len();
        if removed > 0 {
            self.save_map(&map)?;
        }
        Ok(removed)
    }

    fn load_map(&self) -> PortMap {
        fs::read_to_string(&self.map_path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    fn save_map(&self, map: &PortMap) -> Result<()> {
        use fs2::FileExt;
        use std::io::Write;

        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.map_path)
            .with_context(|| format!("Failed to open port map: {}", self.map_path.display()))?;

        file.lock_exclusive()
            .with_context(|| "Failed to lock port map file")?;

        let json = serde_json::to_string_pretty(map)?;
        let mut writer = std::io::BufWriter::new(&file);
        writer.write_all(json.as_bytes())?;
        writer.flush()?;

        file.unlock().ok();
        Ok(())
    }
}

/// Compute a deterministic preferred port from identity string.
fn preferred_port(identity: &str) -> u16 {
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    let hash = hasher.finish();
    let range_size = (PORT_RANGE_END - PORT_RANGE_START + 1) as u64;
    PORT_RANGE_START + (hash % range_size) as u16
}

/// Check if a port is available by attempting to bind.
fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port))
        .map(|listener| {
            drop(listener);
            true
        })
        .unwrap_or(false)
}

/// Check if a port is already allocated to another identity.
fn is_allocated(map: &PortMap, port: u16) -> bool {
    map.allocations.values().any(|a| a.port == port)
}

/// Check if a process with the given PID is still alive.
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    #[cfg(unix)]
    {
        // kill(pid, 0) checks existence without sending a signal
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
            .output()
            .map(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_manager() -> (PortManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let mgr = PortManager {
            map_path: dir.path().join(PORT_MAP_FILE),
        };
        (mgr, dir)
    }

    #[test]
    fn preferred_port_is_deterministic() {
        let a = preferred_port("test/capsule");
        let b = preferred_port("test/capsule");
        assert_eq!(a, b);
        assert!((PORT_RANGE_START..=PORT_RANGE_END).contains(&a));
        let b = preferred_port("test/capsule-b");
        // Could theoretically collide but extremely unlikely with 10000 range
        assert_ne!(a, b);
    }

    #[test]
    fn resolve_port_returns_port_in_range() {
        let (mgr, _dir) = test_manager();
        let port = mgr.resolve_port("test/my-app").unwrap();
        assert!((PORT_RANGE_START..=PORT_RANGE_END).contains(&port));
    }

    #[test]
    fn resolve_port_is_sticky() {
        let (mgr, _dir) = test_manager();
        let first = mgr.resolve_port("test/sticky").unwrap();
        let second = mgr.resolve_port("test/sticky").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn resolve_port_gives_different_ports_for_different_ids() {
        let (mgr, _dir) = test_manager();
        let a = mgr.resolve_port("test/app-a").unwrap();
        let b = mgr.resolve_port("test/app-b").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn release_removes_allocation() {
        let (mgr, _dir) = test_manager();
        let _port = mgr.resolve_port("test/release-me").unwrap();
        mgr.release("test/release-me").unwrap();
        let map = mgr.load_map();
        assert!(!map.allocations.contains_key("test/release-me"));
    }

    #[test]
    fn gc_removes_dead_pid_entries() {
        let (mgr, _dir) = test_manager();
        // Insert an allocation with a definitely-dead PID
        let mut map = PortMap::default();
        map.allocations.insert(
            "dead/process".to_string(),
            PortAllocation {
                port: 10500,
                pid: 999_999_999, // very unlikely to be alive
                allocated_at: SystemTime::now(),
            },
        );
        mgr.save_map(&map).unwrap();

        let removed = mgr.gc().unwrap();
        assert_eq!(removed, 1);

        let after = mgr.load_map();
        assert!(after.allocations.is_empty());
    }
}
