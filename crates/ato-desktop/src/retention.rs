//! Retained-session table for the v0 "Surface close ≠ Session stop"
//! contract (RFC: SURFACE_CLOSE_SEMANTICS).
//!
//! ## Why this exists
//!
//! Before this layer, `WebViewManager::stop_launched_session` invoked
//! `ato app session stop` synchronously when a pane closed, deleting
//! the session record. The next click had no record to fast-path
//! against, so close → re-click fell back to the cold path
//! (~6 s observed in PR 4A.1 measurement).
//!
//! `RetentionTable` demotes pane-close from an immediate-stop into a
//! TTL-bounded retention. The session record stays on disk, the
//! process stays alive, and the existing Phase 1 fast path
//! (`try_session_record_fast_path`) hits naturally on reopen. TTL
//! expiry, app quit, and LRU overflow all stop the session via a
//! best-effort, non-blocking, fire-and-forget background thread —
//! the UI thread is never blocked on `ato app session stop`.
//!
//! ## What lives here vs. on `WebViewManager`
//!
//! This module owns the table data structure, the LRU policy, and
//! the spawn-and-forget `spawn_graceful_stop` helper. It does **not**
//! own:
//!
//! - the GuestLaunchSession value (we only keep the bits needed to
//!   stop the session — `session_id` + `handle` for logging),
//! - the SURFACE-TIMING emission (orchestrator already emits the
//!   right stages on reopen),
//! - the UI surface for explicit Stop (next PR — context menu /
//!   command palette).
//!
//! ## When TTL is swept
//!
//! `WebViewManager::sync_from_state` is called every GPUI render
//! pass, so the simplest sweep cadence is "opportunistic on every
//! sync". Idle apps may keep a session past its TTL until the next
//! render — acceptable for v0 because:
//! - the `Drop` path on app quit drains everything, so nothing
//!   leaks across process lifetimes,
//! - users only notice retention if they reopen, which itself
//!   triggers a render.
//!
//! A periodic background timer is a v1 refinement (RFC §12 open
//! question on idle drift).

use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::orchestrator::stop_guest_session;

/// Default retention TTL. Matches `SURFACE_MATERIALIZATION.md` §9.5
/// and `SURFACE_CLOSE_SEMANTICS.md` §5.1. v0 constant; per-user /
/// per-capsule overrides are a v1 question.
pub(crate) const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

/// Maximum number of simultaneously retained sessions. Matches
/// `SURFACE_MATERIALIZATION.md` §9.5. LRU eviction triggers when
/// retention size exceeds this cap.
pub(crate) const DEFAULT_MAX_RETAINED: usize = 8;

/// One entry in the retention table. Tracks just enough to issue a
/// best-effort stop later: `session_id` is the primary key the CLI
/// stop command takes, `handle` is for log lines, `retained_at`
/// drives TTL eviction.
#[derive(Clone, Debug)]
pub(crate) struct RetainedSession {
    pub session_id: String,
    pub handle: String,
    pub retained_at: Instant,
}

/// Reason a session was evicted from the retention table. Returned
/// by the eviction APIs so the caller can produce the right log
/// line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EvictionReason {
    TtlExpired,
    LruOverflow,
    AppQuit,
}

impl EvictionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TtlExpired => "ttl_expired",
            Self::LruOverflow => "lru_overflow",
            Self::AppQuit => "app_quit",
        }
    }
}

/// FIFO/LRU table of retained capsule sessions. Insertion order = LRU
/// order; the back of the deque is the most recently retained, so
/// LRU eviction pops the front. With cap=8 the linear-scan removes
/// for `take_by_session_id` are negligible (<1 µs).
#[derive(Debug)]
pub(crate) struct RetentionTable {
    entries: Vec<RetainedSession>,
    ttl: Duration,
    max_size: usize,
}

impl RetentionTable {
    pub fn new(ttl: Duration, max_size: usize) -> Self {
        Self {
            entries: Vec::new(),
            ttl,
            max_size,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_TTL, DEFAULT_MAX_RETAINED)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Add a session to the retention table. If the table is already
    /// at capacity, the oldest entry is returned for the caller to
    /// stop. The caller is responsible for issuing the stop — this
    /// layer never blocks on it.
    pub fn retain(
        &mut self,
        session_id: String,
        handle: String,
        now: Instant,
    ) -> Vec<(RetainedSession, EvictionReason)> {
        // De-dup: if the same session_id is already retained, refresh
        // its retention timestamp instead of adding a second entry.
        // This can happen if a rare reopen-without-fast-path-hit path
        // re-enters retention for an already-warm session.
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

    /// Remove and return the entry matching `session_id`, if any.
    /// Used by the reopen path: when the fast path attaches to a
    /// retained session_id, the slot is no longer "retained" but
    /// "active". No stop is issued — the session is now in use.
    pub fn take_by_session_id(&mut self, session_id: &str) -> Option<RetainedSession> {
        let idx = self
            .entries
            .iter()
            .position(|e| e.session_id == session_id)?;
        Some(self.entries.remove(idx))
    }

    /// Walk the table, evicting any entry whose `retained_at + ttl`
    /// is in the past relative to `now`. Returns the evicted entries
    /// for the caller to graceful-stop.
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

/// Stop a retained session in a fire-and-forget background thread so
/// the UI never blocks on `ato app session stop`. The retention
/// slot is dropped from the table *before* this runs — caller's
/// invariant.
///
/// `reason` is logged so post-mortem inspection can distinguish
/// TTL / LRU / quit-driven stops from explicit user-initiated ones
/// (the explicit-Stop UI lands in a follow-up PR and uses a
/// different code path).
pub(crate) fn spawn_graceful_stop(session: RetainedSession, reason: EvictionReason) {
    let session_id = session.session_id.clone();
    let handle = session.handle.clone();
    let reason_label = reason.as_str();
    std::thread::spawn(move || {
        debug!(
            session_id = %session_id,
            handle = %handle,
            reason = reason_label,
            "graceful stop scheduled for retained session"
        );
        match stop_guest_session(&session_id) {
            Ok(true) => info!(
                session_id = %session_id,
                handle = %handle,
                reason = reason_label,
                "retained session stopped"
            ),
            Ok(false) => debug!(
                session_id = %session_id,
                handle = %handle,
                reason = reason_label,
                "retained session was already inactive"
            ),
            Err(err) => warn!(
                session_id = %session_id,
                handle = %handle,
                reason = reason_label,
                error = %err,
                "graceful stop of retained session failed; record may linger"
            ),
        }
    });
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
        // Refreshed timestamp pushes TTL further out: an eviction sweep
        // at the original retain+TTL should NOT remove this entry.
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
        // These literal strings appear in tracing output; downstream
        // log-grep tooling depends on stable spellings.
        assert_eq!(EvictionReason::TtlExpired.as_str(), "ttl_expired");
        assert_eq!(EvictionReason::LruOverflow.as_str(), "lru_overflow");
        assert_eq!(EvictionReason::AppQuit.as_str(), "app_quit");
    }
}
