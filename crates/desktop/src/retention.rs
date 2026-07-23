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
//! ## What lives here vs. in `runner::session`
//!
//! The table data structure and the TTL/LRU policy are host-agnostic and
//! single-sourced in `runner::session` ([`RetentionTable`] / [`RetainedSession`]
//! / [`EvictionReason`] + the TTL/LRU constants), re-exported here so every
//! `crate::retention::…` call site is unchanged. This module keeps only the
//! host-specific stop action — [`spawn_graceful_stop`], which shells out to the
//! CLI `ato app session stop`. It does **not** own:
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

use tracing::{debug, info, warn};

use crate::orchestrator::stop_guest_session;

// The retention table + TTL/LRU policy are single-sourced in runner::session,
// re-exported so the `crate::retention::{RetentionTable, RetainedSession,
// EvictionReason, DEFAULT_TTL, DEFAULT_MAX_RETAINED}` reference paths are
// unchanged.
pub(crate) use runner::session::{
    DEFAULT_MAX_RETAINED, DEFAULT_TTL, EvictionReason, RetainedSession, RetentionTable,
};

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
