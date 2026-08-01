//! Session lifecycle over a [`crate::supervisor::ProcessSupervisor`].
//!
//! Holds the host-agnostic **retention policy** ([`RetentionTable`]): a warm
//! session's process is stopped, but its on-disk record is kept for a bounded
//! window so a reopen hits the fast path instead of a cold launch. TTL expiry,
//! LRU overflow, and app-quit all surface entries for the host to stop —
//! *how* to stop them (the CLI `session stop` call) is host policy and stays
//! with the host (the desktop shell's `retention::spawn_graceful_stop`); this
//! layer only owns the table, the TTL/LRU eviction rules, and the reason
//! labels.
//!
//! The launch / stop / restart / list wiring ([`SessionSupervisor`]) is still a
//! module boundary — that flow is desktop-resident today (bound to desktop
//! domain types) and lands here only if/when it is made host-agnostic.

use std::time::{Duration, Instant};

/// Stable identifier for a supervised session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

/// Lifecycle state of a supervised session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Spawned, not yet observed ready.
    Starting,
    /// Running and observed ready.
    Running,
    /// Teardown requested, not yet confirmed stopped.
    Stopping,
    /// Confirmed stopped.
    Stopped,
    /// Exited abnormally.
    Failed,
}

// ── Retention (TTL / LRU) ───────────────────────────────────────────────────

/// Default retention TTL. Matches `SURFACE_MATERIALIZATION.md` §9.5 and
/// `SURFACE_CLOSE_SEMANTICS.md` §5.1. v0 constant; per-user / per-capsule
/// overrides are a v1 question.
pub const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

/// Maximum number of simultaneously retained sessions. Matches
/// `SURFACE_MATERIALIZATION.md` §9.5. LRU eviction triggers when retention size
/// exceeds this cap.
pub const DEFAULT_MAX_RETAINED: usize = 8;

/// One entry in the retention table. Tracks just enough to issue a best-effort
/// stop later: `session_id` is the primary key the CLI stop command takes,
/// `handle` is for log lines, `retained_at` drives TTL eviction.
#[derive(Clone, Debug)]
pub struct RetainedSession {
    pub session_id: String,
    pub handle: String,
    pub retained_at: Instant,
}

/// Reason a session was evicted from the retention table. Returned by the
/// eviction APIs so the caller can produce the right log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictionReason {
    TtlExpired,
    LruOverflow,
    AppQuit,
}

impl EvictionReason {
    /// Stable grep-friendly label emitted in tracing output. Downstream
    /// log-grep tooling depends on these spellings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TtlExpired => "ttl_expired",
            Self::LruOverflow => "lru_overflow",
            Self::AppQuit => "app_quit",
        }
    }
}

/// FIFO/LRU table of retained capsule sessions. Insertion order = LRU order;
/// the back of the deque is the most recently retained, so LRU eviction pops
/// the front. With cap=8 the linear-scan removes for `take_by_session_id` are
/// negligible (<1 µs).
#[derive(Debug)]
pub struct RetentionTable {
    entries: Vec<RetainedSession>,
    ttl: Duration,
    max_size: usize,
}

impl RetentionTable {
    /// A table with an explicit TTL and LRU cap.
    pub fn new(ttl: Duration, max_size: usize) -> Self {
        Self {
            entries: Vec::new(),
            ttl,
            max_size,
        }
    }

    /// A table with the v0 defaults ([`DEFAULT_TTL`] / [`DEFAULT_MAX_RETAINED`]).
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_TTL, DEFAULT_MAX_RETAINED)
    }

    /// Number of retained entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Add a session to the retention table. If the table is already at
    /// capacity, the oldest entry is returned for the caller to stop. The
    /// caller is responsible for issuing the stop — this layer never blocks on
    /// it.
    pub fn retain(
        &mut self,
        session_id: String,
        handle: String,
        now: Instant,
    ) -> Vec<(RetainedSession, EvictionReason)> {
        // De-dup: if the same session_id is already retained, refresh its
        // retention timestamp instead of adding a second entry. This can happen
        // if a rare reopen-without-fast-path-hit path re-enters retention for an
        // already-warm session.
        if let Some(existing) = self.entries.iter_mut().find(|e| e.session_id == session_id) {
            existing.retained_at = now;
            existing.handle = handle;
            return Vec::new();
        }

        self.entries.push(RetainedSession {
            session_id,
            handle,
            retained_at: now,
        });

        // LRU overflow: pop the oldest until under cap.
        let mut evicted = Vec::new();
        while self.entries.len() > self.max_size {
            let oldest = self.entries.remove(0);
            evicted.push((oldest, EvictionReason::LruOverflow));
        }
        evicted
    }

    /// Remove and return the entry matching `session_id`, if any. Used by the
    /// reopen path: when the fast path attaches to a retained session_id, the
    /// slot is no longer "retained" but "active". No stop is issued — the
    /// session is now in use.
    pub fn take_by_session_id(&mut self, session_id: &str) -> Option<RetainedSession> {
        let idx = self
            .entries
            .iter()
            .position(|e| e.session_id == session_id)?;
        Some(self.entries.remove(idx))
    }

    /// Walk the table, evicting any entry whose `retained_at + ttl` is in the
    /// past relative to `now`. Returns the evicted entries for the caller to
    /// graceful-stop.
    pub fn evict_expired(&mut self, now: Instant) -> Vec<(RetainedSession, EvictionReason)> {
        let ttl = self.ttl;
        let mut evicted = Vec::new();
        self.entries.retain(|entry| {
            if now.duration_since(entry.retained_at) >= ttl {
                evicted.push((entry.clone(), EvictionReason::TtlExpired));
                false
            } else {
                true
            }
        });
        evicted
    }

    /// Drain every entry (for app-quit / Drop). Caller stops them.
    pub fn drain(&mut self) -> Vec<(RetainedSession, EvictionReason)> {
        self.entries
            .drain(..)
            .map(|e| (e, EvictionReason::AppQuit))
            .collect()
    }
}

impl Default for RetentionTable {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ── Launch / stop / restart wiring (boundary) ───────────────────────────────

/// Supervises the set of live sessions on a host. Placeholder — the
/// launch/stop wiring is desktop-resident today (bound to desktop domain
/// types); it lands here only if/when made host-agnostic. Exists so the module
/// boundary and public surface are fixed.
#[derive(Debug, Default)]
pub struct SessionSupervisor {
    _private: (),
}

impl SessionSupervisor {
    /// Create an empty session supervisor.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    fn entry(table: &mut RetentionTable, id: &str, when: Instant) {
        let evicted = table.retain(id.to_string(), format!("h:{id}"), when);
        assert!(evicted.is_empty(), "no LRU eviction expected at this size");
    }

    #[test]
    fn retain_adds_entries_in_lru_order() {
        let mut t = RetentionTable::new(DEFAULT_TTL, 8);
        let t0 = now();
        entry(&mut t, "a", t0);
        entry(&mut t, "b", t0 + Duration::from_secs(1));
        entry(&mut t, "c", t0 + Duration::from_secs(2));
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn retain_dedups_on_same_session_id_and_refreshes_timestamp() {
        let mut t = RetentionTable::new(DEFAULT_TTL, 8);
        let t0 = now();
        let _ = t.retain("a".into(), "h".into(), t0);
        let _ = t.retain("a".into(), "h".into(), t0 + Duration::from_secs(10));
        assert_eq!(t.len(), 1);
        // Refreshed timestamp pushes TTL further out: an eviction sweep at the
        // original retain+TTL should NOT remove this entry.
        let after = t0 + DEFAULT_TTL;
        let evicted = t.evict_expired(after);
        assert!(
            evicted.is_empty(),
            "refreshed entry must outlast its first window"
        );
    }

    #[test]
    fn retain_evicts_lru_when_over_cap() {
        let cap = 3;
        let mut t = RetentionTable::new(DEFAULT_TTL, cap);
        let t0 = now();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            entry(&mut t, id, t0 + Duration::from_secs(i as u64));
        }
        assert_eq!(t.len(), 3);
        let evicted = t.retain("d".into(), "h".into(), t0 + Duration::from_secs(10));
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0.session_id, "a");
        assert_eq!(evicted[0].1, EvictionReason::LruOverflow);
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn take_by_session_id_returns_and_removes() {
        let mut t = RetentionTable::with_defaults();
        let t0 = now();
        entry(&mut t, "a", t0);
        entry(&mut t, "b", t0);
        let taken = t.take_by_session_id("a").expect("entry present");
        assert_eq!(taken.session_id, "a");
        assert_eq!(t.len(), 1);
        assert!(t.take_by_session_id("a").is_none());
    }

    #[test]
    fn evict_expired_only_removes_aged_entries() {
        let ttl = Duration::from_secs(5);
        let mut t = RetentionTable::new(ttl, 8);
        let t0 = now();
        entry(&mut t, "old", t0);
        entry(&mut t, "fresh", t0 + Duration::from_secs(4));

        let now2 = t0 + Duration::from_secs(6); // past `old` but not `fresh`'s deadline
        let evicted = t.evict_expired(now2);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0.session_id, "old");
        assert_eq!(evicted[0].1, EvictionReason::TtlExpired);
        assert_eq!(t.len(), 1);
        assert_eq!(t.entries[0].session_id, "fresh");
    }

    #[test]
    fn drain_returns_everything_with_quit_reason() {
        let mut t = RetentionTable::with_defaults();
        let t0 = now();
        entry(&mut t, "a", t0);
        entry(&mut t, "b", t0);
        let drained = t.drain();
        assert_eq!(drained.len(), 2);
        assert!(drained.iter().all(|(_, r)| *r == EvictionReason::AppQuit));
        assert!(t.is_empty());
    }

    #[test]
    fn eviction_reason_labels_are_grep_friendly() {
        assert_eq!(EvictionReason::TtlExpired.as_str(), "ttl_expired");
        assert_eq!(EvictionReason::LruOverflow.as_str(), "lru_overflow");
        assert_eq!(EvictionReason::AppQuit.as_str(), "app_quit");
    }
}
