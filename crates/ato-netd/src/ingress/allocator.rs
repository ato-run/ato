//! Persistent port allocator for ingress reverse-proxy routes.
//!
//! Maps opaque `logical_key` strings to `u16` TCP ports in the
//! range [`PORT_RANGE_START`..`PORT_RANGE_END`].  The mapping is
//! persisted as JSON to avoid re-assigning ports across daemon
//! restarts (desktop apps rely on the `127.0.0.1:<port>` origin
//! being stable).
//!
//! # Atomic write contract
//!
//! Every mutation writes a `.json.tmp` sibling file then renames it over
//! the live file.  The rename is atomic on all POSIX-compliant file
//! systems; on Windows it falls back to a best-effort overwrite (not
//! POSIX-atomic but still safer than a partial write).
//!
//! # `PersistedPortTaken` error
//!
//! This error is **not** returned for normal re-registration of an
//! existing key (that succeeds via the normal get-or-assign path).
//! It is reserved for the pathological case where the JSON file on
//! disk was hand-edited and two different keys have been assigned the
//! same port number.  Callers should treat it as a fatal configuration
//! error.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// First port number in the allocation range (inclusive).
pub const PORT_RANGE_START: u16 = 40_000;
/// Last port number in the allocation range (inclusive).
pub const PORT_RANGE_END: u16 = 49_999;

/// Errors returned by [`PortAllocator`].
#[derive(Debug, Error)]
pub enum AllocError {
    /// The persisted JSON file contains two different keys mapped to the
    /// same port number.  This indicates manual file corruption.
    #[error(
        "port {port} is already claimed by key {other_key:?} in the persisted allocation table"
    )]
    PersistedPortTaken { port: u16, other_key: String },

    /// The port range is exhausted.  Increase [`PORT_RANGE_END`] or
    /// deregister unused routes.
    #[error("port allocation range {PORT_RANGE_START}–{PORT_RANGE_END} exhausted")]
    RangeExhausted,

    /// I/O error while reading or writing the JSON file.
    #[error("allocator I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON parse error while loading the persisted table.
    #[error("allocator JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// In-memory view of `stable_origin_ports.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedTable {
    #[serde(default)]
    ports: HashMap<String, u16>,
}

/// Daemon-owned persistent port allocator.
///
/// `PortAllocator` is intentionally **not** `Clone` — there should be
/// exactly one instance per daemon, owned by `IngressManager`.  Callers
/// outside this module obtain ports only through `IngressManager`.
#[derive(Debug)]
pub struct PortAllocator {
    /// Path to `${ATO_HOME}/state/netd/stable_origin_ports.json`.
    path: PathBuf,
    /// Authoritative in-memory mapping — kept in sync with the JSON.
    table: PersistedTable,
    /// Reverse map: port → key, used for `PersistedPortTaken` detection.
    by_port: HashMap<u16, String>,
}

impl PortAllocator {
    /// Load the allocator from `path`.  Creates the file (and parent
    /// directories) if it does not exist.
    pub async fn load(path: impl Into<PathBuf>) -> Result<Self, AllocError> {
        let path = path.into();

        // Ensure parent directory exists.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Read existing table or start fresh.
        let table: PersistedTable = if path.exists() {
            let raw = tokio::fs::read_to_string(&path).await?;
            serde_json::from_str(&raw)?
        } else {
            PersistedTable::default()
        };

        // Build reverse map and validate uniqueness.
        let mut by_port: HashMap<u16, String> = HashMap::new();
        for (key, &port) in &table.ports {
            if let Some(other_key) = by_port.insert(port, key.clone()) {
                if other_key != *key {
                    return Err(AllocError::PersistedPortTaken { port, other_key });
                }
            }
        }

        Ok(Self {
            path,
            table,
            by_port,
        })
    }

    /// Return the persisted port for `key`, or allocate a new one.
    ///
    /// This is idempotent: calling `get_or_assign` twice with the same key
    /// always returns the same port.
    pub async fn get_or_assign(&mut self, key: &str) -> Result<u16, AllocError> {
        if let Some(&port) = self.table.ports.get(key) {
            return Ok(port);
        }

        let port = self.next_free_port()?;
        self.table.ports.insert(key.to_string(), port);
        self.by_port.insert(port, key.to_string());
        self.persist().await?;
        Ok(port)
    }

    /// Remove `key` from the table and persist.  No-op if key is unknown.
    pub async fn remove(&mut self, key: &str) -> Result<(), AllocError> {
        if let Some(port) = self.table.ports.remove(key) {
            self.by_port.remove(&port);
            self.persist().await?;
        }
        Ok(())
    }

    /// Snapshot: key → port pairs for all allocated routes.
    pub fn snapshot(&self) -> HashMap<String, u16> {
        self.table.ports.clone()
    }

    // ── private ────────────────────────────────────────────────────────

    fn next_free_port(&self) -> Result<u16, AllocError> {
        for port in PORT_RANGE_START..=PORT_RANGE_END {
            if !self.by_port.contains_key(&port) {
                return Ok(port);
            }
        }
        Err(AllocError::RangeExhausted)
    }

    async fn persist(&self) -> Result<(), AllocError> {
        let json = serde_json::to_string_pretty(&self.table)?;
        let tmp_path = tmp_path_for(&self.path);

        // Write to .tmp first, then rename atomically.
        tokio::fs::write(&tmp_path, &json).await?;
        tokio::fs::rename(&tmp_path, &self.path).await?;

        Ok(())
    }
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let mut name = tmp.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    tmp.set_file_name(name);
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn make_allocator() -> (PortAllocator, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state/netd/stable_origin_ports.json");
        let alloc = PortAllocator::load(&path).await.unwrap();
        (alloc, dir)
    }

    #[tokio::test]
    async fn same_key_returns_same_port() {
        let (mut alloc, _dir) = make_allocator().await;
        let p1 = alloc.get_or_assign("key-a").await.unwrap();
        let p2 = alloc.get_or_assign("key-a").await.unwrap();
        assert_eq!(p1, p2);
    }

    #[tokio::test]
    async fn different_keys_get_different_ports() {
        let (mut alloc, _dir) = make_allocator().await;
        let pa = alloc.get_or_assign("key-a").await.unwrap();
        let pb = alloc.get_or_assign("key-b").await.unwrap();
        assert_ne!(pa, pb);
    }

    #[tokio::test]
    async fn port_persists_across_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state/netd/stable_origin_ports.json");

        let port = {
            let mut alloc = PortAllocator::load(&path).await.unwrap();
            alloc.get_or_assign("my-capsule").await.unwrap()
        };

        // Reload from disk.
        let mut alloc2 = PortAllocator::load(&path).await.unwrap();
        let port2 = alloc2.get_or_assign("my-capsule").await.unwrap();
        assert_eq!(port, port2);
    }

    #[tokio::test]
    async fn remove_releases_key() {
        let (mut alloc, _dir) = make_allocator().await;
        let p1 = alloc.get_or_assign("key-a").await.unwrap();
        alloc.remove("key-a").await.unwrap();
        // Re-assigning may return the same port (it is now free) — the
        // important thing is it does not fail.
        let p2 = alloc.get_or_assign("key-a").await.unwrap();
        let _ = (p1, p2);
    }

    #[tokio::test]
    async fn persisted_port_taken_error_on_corrupt_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state/netd/stable_origin_ports.json");
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();

        // Write a corrupt table: two keys sharing port 40000.
        let corrupt = r#"{"ports":{"key-a":40000,"key-b":40000}}"#;
        tokio::fs::write(&path, corrupt).await.unwrap();

        let result = PortAllocator::load(&path).await;
        assert!(
            matches!(
                result,
                Err(AllocError::PersistedPortTaken { port: 40000, .. })
            ),
            "expected PersistedPortTaken, got: {result:?}",
        );
    }
}
