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
    collections::{HashMap, HashSet, VecDeque},
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
            if let Some(other_key) = by_port.insert(port, key.clone())
                && other_key != *key
            {
                return Err(AllocError::PersistedPortTaken { port, other_key });
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

// ---------------------------------------------------------------------------
// Ephemeral port allocator (in-memory, no persistence)
// ---------------------------------------------------------------------------

/// In-memory port allocator for ephemeral (transient) capsule sessions.
///
/// Unlike [`PortAllocator`], ephemeral ports are **never** written to
/// `stable_origin_ports.json`. This prevents transient runs from polluting
/// the stable origin table.
///
/// # Port reuse protection
///
/// When an ephemeral port is released via [`EphemeralAllocator::release`],
/// it is moved to a bounded `recently_freed` cooldown set and is not
/// reassigned while it stays there. This prevents the scenario where
/// capsule A's port is immediately handed to capsule B.
///
/// The cooldown set is bounded at [`RECENTLY_FREED_CAP`] entries (FIFO:
/// the oldest freed port is evicted first), and `assign` falls back to
/// reusing the oldest freed port when every otherwise-free port is in
/// cooldown. Without the bound, a long-lived daemon servicing many
/// transient sessions would exhaust the whole range (issue #647).
///
/// # Collision avoidance
///
/// The allocator receives a snapshot of stable-allocated ports at assignment
/// time so it never picks a port already in use by a stable route.
#[derive(Debug, Default)]
pub struct EphemeralAllocator {
    /// Currently active ephemeral routes: session key → port.
    active_map: HashMap<String, u16>,
    /// Fast reverse lookup for collision detection.
    active_ports: HashSet<u16>,
    /// Ports freed recently — blocked from immediate reuse. Membership set
    /// for O(1) lookup; bounded by [`RECENTLY_FREED_CAP`].
    recently_freed: HashSet<u16>,
    /// Insertion order of `recently_freed`, oldest at the front. Used to
    /// evict (on overflow) or reuse (when the range is tight) the port
    /// that has been in cooldown the longest.
    recently_freed_order: VecDeque<u16>,
}

/// Maximum number of ports kept in the recently-freed cooldown set.
///
/// When the set is full, the oldest entry is evicted and becomes
/// reassignable again. The bound keeps the immediate-reuse protection
/// meaningful while guaranteeing the allocator can never block the whole
/// 10,000-port range after many transient session start/stop cycles.
const RECENTLY_FREED_CAP: usize = 1_024;

impl EphemeralAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Assign a port for `session_key`. Avoids `stable_occupied` ports,
    /// currently active ports, and recently-freed ports. Idempotent for
    /// the same key.
    ///
    /// When every otherwise-free port is in the recently-freed cooldown
    /// set, the oldest freed port is reused instead of failing —
    /// [`AllocError::RangeExhausted`] is returned only when stable and
    /// active routes genuinely occupy the entire range.
    pub fn assign(
        &mut self,
        session_key: &str,
        stable_occupied: &HashSet<u16>,
    ) -> Result<u16, AllocError> {
        if let Some(&port) = self.active_map.get(session_key) {
            return Ok(port);
        }
        // First pass: prefer ports outside the cooldown set.
        for port in PORT_RANGE_START..=PORT_RANGE_END {
            if !stable_occupied.contains(&port)
                && !self.active_ports.contains(&port)
                && !self.recently_freed.contains(&port)
            {
                self.active_map.insert(session_key.to_string(), port);
                self.active_ports.insert(port);
                return Ok(port);
            }
        }
        // Fallback: every free port is in cooldown. Reuse the oldest freed
        // port (longest cooldown) rather than exhausting the range.
        if let Some(idx) = self
            .recently_freed_order
            .iter()
            .position(|p| !stable_occupied.contains(p) && !self.active_ports.contains(p))
        {
            let port = self
                .recently_freed_order
                .remove(idx)
                .expect("index returned by position() is in range");
            self.recently_freed.remove(&port);
            self.active_map.insert(session_key.to_string(), port);
            self.active_ports.insert(port);
            return Ok(port);
        }
        Err(AllocError::RangeExhausted)
    }

    /// Release the port for `session_key`. Moves it to the bounded
    /// `recently_freed` cooldown set to prevent immediate reuse; when the
    /// set is full, the oldest entry is evicted and becomes reassignable.
    pub fn release(&mut self, session_key: &str) {
        if let Some(port) = self.active_map.remove(session_key) {
            self.active_ports.remove(&port);
            if self.recently_freed.insert(port) {
                self.recently_freed_order.push_back(port);
                if self.recently_freed_order.len() > RECENTLY_FREED_CAP
                    && let Some(oldest) = self.recently_freed_order.pop_front()
                {
                    self.recently_freed.remove(&oldest);
                }
            }
        }
    }

    /// Return the active port for `session_key`, if any.
    #[allow(dead_code)]
    pub fn get(&self, session_key: &str) -> Option<u16> {
        self.active_map.get(session_key).copied()
    }

    /// Snapshot of all active ephemeral routes: session key → port.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> HashMap<String, u16> {
        self.active_map.clone()
    }
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

    // ── EphemeralAllocator tests ──────────────────────────────────────────────

    #[test]
    fn ephemeral_two_keys_get_different_ports() {
        let mut ea = EphemeralAllocator::new();
        let stable: HashSet<u16> = HashSet::new();
        let p1 = ea.assign("session:alpha", &stable).unwrap();
        let p2 = ea.assign("session:beta", &stable).unwrap();
        assert_ne!(p1, p2);
    }

    #[test]
    fn ephemeral_same_key_is_idempotent() {
        let mut ea = EphemeralAllocator::new();
        let stable: HashSet<u16> = HashSet::new();
        let p1 = ea.assign("session:alpha", &stable).unwrap();
        let p2 = ea.assign("session:alpha", &stable).unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn ephemeral_avoids_stable_occupied_ports() {
        let mut ea = EphemeralAllocator::new();
        // Block every port in the range except the last one.
        let stable: HashSet<u16> = (PORT_RANGE_START..PORT_RANGE_END).collect();
        let port = ea.assign("session:alpha", &stable).unwrap();
        assert_eq!(port, PORT_RANGE_END, "should pick the only free port");
    }

    #[test]
    fn ephemeral_released_port_not_immediately_reused() {
        let mut ea = EphemeralAllocator::new();
        let stable: HashSet<u16> = HashSet::new();
        let p1 = ea.assign("session:alpha", &stable).unwrap();
        ea.release("session:alpha");
        // The released port should be in recently_freed — a new session must
        // get a different port.
        let p2 = ea.assign("session:beta", &stable).unwrap();
        assert_ne!(
            p1, p2,
            "released port must not be immediately reused within same daemon lifetime"
        );
    }

    #[test]
    fn ephemeral_stable_and_ephemeral_ports_do_not_collide() {
        let mut ea = EphemeralAllocator::new();
        // Simulate stable allocator holding port 40000.
        let stable: HashSet<u16> = [PORT_RANGE_START].into();
        let ep = ea.assign("session:alpha", &stable).unwrap();
        assert_ne!(
            ep, PORT_RANGE_START,
            "ephemeral must not pick a stable-occupied port"
        );
    }

    /// Verifies that the bind-failure rollback path in `register_ephemeral`
    /// (which calls `ephemeral_alloc.release` after a failed `bind_listener`)
    /// leaves the allocator in a clean state: the key is gone from
    /// `active_map` so a subsequent registration for the same key tries a new
    /// port.
    #[test]
    fn ephemeral_release_after_assign_removes_key_from_active_map() {
        let mut ea = EphemeralAllocator::new();
        let stable: HashSet<u16> = HashSet::new();
        let port = ea.assign("session:gamma", &stable).unwrap();
        assert!(
            ea.get("session:gamma").is_some(),
            "key should be active after assign"
        );
        // Simulate bind failure: caller rolls back the allocation.
        ea.release("session:gamma");
        assert!(
            ea.get("session:gamma").is_none(),
            "key must be removed after release"
        );
        // Re-assigning the same key returns a fresh port (not the released one).
        let port2 = ea.assign("session:gamma", &stable).unwrap();
        assert_ne!(
            port, port2,
            "re-assigned port must differ from the released port within same daemon lifetime"
        );
    }

    /// When every otherwise-free port is in the recently-freed cooldown
    /// set, `assign` must fall back to reusing the oldest freed port and
    /// only report `RangeExhausted` when stable + active routes genuinely
    /// occupy the entire range.
    #[test]
    fn ephemeral_falls_back_to_oldest_freed_port_when_range_tight() {
        let mut ea = EphemeralAllocator::new();
        // Leave only two usable ports in the range.
        let stable: HashSet<u16> = (PORT_RANGE_START + 2..=PORT_RANGE_END).collect();
        let p1 = ea.assign("session:a", &stable).unwrap();
        let p2 = ea.assign("session:b", &stable).unwrap();
        ea.release("session:a");
        ea.release("session:b");
        // Both usable ports are now in cooldown; the allocator must reuse
        // the oldest freed port first instead of reporting exhaustion.
        let p3 = ea.assign("session:c", &stable).unwrap();
        assert_eq!(p3, p1, "oldest freed port should be reused first");
        let p4 = ea.assign("session:d", &stable).unwrap();
        assert_eq!(p4, p2, "next-oldest freed port should be reused second");
        // With both ports active again the range is genuinely exhausted.
        let err = ea.assign("session:e", &stable);
        assert!(
            matches!(err, Err(AllocError::RangeExhausted)),
            "expected RangeExhausted, got: {err:?}",
        );
    }

    /// Regression test for issue #647: a long-lived daemon cycling many
    /// transient sessions must never exhaust the ephemeral range just
    /// because freed ports accumulate in the cooldown set.
    #[test]
    fn ephemeral_many_start_stop_cycles_do_not_exhaust_range() {
        let mut ea = EphemeralAllocator::new();
        let stable: HashSet<u16> = HashSet::new();
        let range_len = usize::from(PORT_RANGE_END - PORT_RANGE_START) + 1;
        // More start/stop cycles than there are ports in the range.
        for i in 0..(range_len + 100) {
            let key = format!("ephemeral:session-{i}");
            ea.assign(&key, &stable)
                .unwrap_or_else(|e| panic!("cycle {i} exhausted the range: {e}"));
            ea.release(&key);
        }
        assert!(
            ea.recently_freed.len() <= RECENTLY_FREED_CAP,
            "recently_freed must stay bounded (len = {})",
            ea.recently_freed.len(),
        );
        assert_eq!(
            ea.recently_freed.len(),
            ea.recently_freed_order.len(),
            "membership set and order queue must stay in sync"
        );
    }

    /// The cooldown set evicts its oldest entry once full, making that
    /// port assignable again without any fallback.
    #[test]
    fn ephemeral_cooldown_evicts_oldest_entry_when_full() {
        let mut ea = EphemeralAllocator::new();
        let stable: HashSet<u16> = HashSet::new();
        // Fill the cooldown set one past its capacity. Each cycle assigns
        // the lowest non-blocked port, so cycle i frees PORT_RANGE_START + i.
        for i in 0..=RECENTLY_FREED_CAP {
            let key = format!("ephemeral:session-{i}");
            ea.assign(&key, &stable).unwrap();
            ea.release(&key);
        }
        // The oldest entry (PORT_RANGE_START) was evicted, so the next
        // assignment picks it up via the normal first-pass scan.
        let port = ea.assign("ephemeral:session-next", &stable).unwrap();
        assert_eq!(
            port, PORT_RANGE_START,
            "evicted oldest port should be assignable again"
        );
    }
}
