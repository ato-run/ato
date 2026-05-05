//! Orphan detection via `<state.dir>/.ato-session` sentinel (RFC §10.4).
//!
//! v1 policy is **warn-only**, no auto-kill / auto-GC. The 4-state
//! decision matrix from RFC §10.4:
//!
//!   sentinel absent              → start fresh
//!   sentinel + dead pid          → warn + sweep + start
//!   sentinel + same alive pid    → resume (caller may skip provider start)
//!   sentinel + other alive pid   → warn + abort
//!
//! Liveness is tested with `kill(pid, 0)` (POSIX) — non-destructive
//! signal-0 returns success if the process exists and the caller has
//! permission to signal it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const SENTINEL_FILENAME: &str = ".ato-session";

// Variant names share a `Failed` suffix because they correspond to
// distinct sentinel-IO operations (read/parse/write/sweep) that each
// fail in their own way. Renaming them away from "X-failed" loses the
// "what was being attempted" framing that error messages depend on.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Error)]
pub enum OrphanError {
    #[error("failed to read sentinel {path}: {source}")]
    ReadFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to parse sentinel {path}: {detail}")]
    ParseFailed { path: PathBuf, detail: String },

    #[error("failed to write sentinel {path}: {source}")]
    WriteFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to remove stale sentinel {path}: {source}")]
    SweepFailed {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSentinel {
    /// Owner Ato session pid that started this dep instance.
    pub session_pid: i32,
    /// Optional provider process pid (for richer diagnostics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_pid: Option<i32>,
    /// RFC 3339 timestamp when sentinel was written.
    pub started_at: String,
    /// Resolved capsule (sha256:...) for diagnostics.
    pub resolved: String,
}

/// Outcome of an orphan check at provider start time. The caller uses
/// this to drive the 4-state RFC §10.4 decision matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanCheckOutcome {
    /// Sentinel file does not exist. Caller must write a fresh sentinel
    /// and start the provider.
    NoSentinel,

    /// Sentinel exists but the owner pid is dead. Caller should warn,
    /// remove the sentinel, then start fresh.
    StaleDeadOwner { sentinel: SessionSentinel },

    /// Sentinel exists and the owner pid matches the *current* Ato
    /// session. Caller may skip provider start and only re-run the ready
    /// probe.
    AliveSameSession { sentinel: SessionSentinel },

    /// Sentinel exists and the owner pid is some other alive Ato session.
    /// Caller must warn and **abort** the start of this dep — running two
    /// providers against the same state.dir would risk corruption.
    AliveOtherSession { sentinel: SessionSentinel },
}

/// Inspect `state_dir` for an orphan sentinel and classify against the
/// current process pid.
pub fn detect_orphan_state(
    state_dir: &Path,
    current_session_pid: i32,
) -> Result<OrphanCheckOutcome, OrphanError> {
    let path = state_dir.join(SENTINEL_FILENAME);
    if !path.exists() {
        return Ok(OrphanCheckOutcome::NoSentinel);
    }
    let text = fs::read_to_string(&path).map_err(|source| OrphanError::ReadFailed {
        path: path.clone(),
        source,
    })?;
    let sentinel: SessionSentinel =
        serde_json::from_str(&text).map_err(|err| OrphanError::ParseFailed {
            path: path.clone(),
            detail: err.to_string(),
        })?;
    if sentinel.session_pid == current_session_pid {
        return Ok(OrphanCheckOutcome::AliveSameSession { sentinel });
    }
    if pid_is_alive(sentinel.session_pid) {
        Ok(OrphanCheckOutcome::AliveOtherSession { sentinel })
    } else {
        Ok(OrphanCheckOutcome::StaleDeadOwner { sentinel })
    }
}

/// Write a fresh `<state_dir>/.ato-session` sentinel. Overwrites any
/// existing file. Caller is responsible for removing the file at clean
/// teardown (`teardown.rs` handles this).
pub fn write_session_sentinel(
    state_dir: &Path,
    sentinel: &SessionSentinel,
) -> Result<(), OrphanError> {
    let path = state_dir.join(SENTINEL_FILENAME);
    let text = serde_json::to_string_pretty(sentinel).map_err(|err| OrphanError::ParseFailed {
        path: path.clone(),
        detail: err.to_string(),
    })?;
    fs::write(&path, text).map_err(|source| OrphanError::WriteFailed { path, source })
}

/// Remove a stale sentinel after a `StaleDeadOwner` outcome. Called by
/// the caller after warning the user.
pub fn sweep_stale_sentinel(state_dir: &Path) -> Result<(), OrphanError> {
    let path = state_dir.join(SENTINEL_FILENAME);
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(&path).map_err(|source| OrphanError::SweepFailed { path, source })
}

/// Outcome of `kill_orphan_provider`, returned for surfacing in the
/// orchestrator's per-dep `warnings` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanProviderKillOutcome {
    /// Sentinel didn't record a `provider_pid`, or the recorded pid was
    /// already dead. Nothing was killed.
    NotPresent,
    /// SIGTERM was delivered. Caller can proceed to start a fresh
    /// provider — the kernel reaps the corpse asynchronously.
    Killed { pid: i32 },
    /// SIGTERM call returned an error other than ESRCH. Treated as
    /// best-effort — the caller still proceeds, since most providers
    /// (e.g. postgres) refuse to start over their own postmaster.pid
    /// and will surface a clearer error than the kill failure.
    KillFailed { pid: i32, detail: String },
}

/// SIGTERM the orphan provider process recorded in a stale sentinel,
/// best-effort. Used in the `StaleDeadOwner` branch when the ato session
/// that started the provider is gone but the provider itself is still
/// holding the state dir (typical for postgres: the postmaster survives
/// SIGKILL of its parent ato session and keeps its PGDATA postmaster.pid
/// locked, blocking the next run's bootstrap from re-binding).
///
/// Falls back to `NotPresent` when the sentinel did not capture a
/// `provider_pid` (older sentinels) — the caller still sweeps the
/// sentinel; postgres-style locks may still need manual cleanup in
/// that legacy case.
#[cfg(unix)]
pub fn kill_orphan_provider(sentinel: &SessionSentinel) -> OrphanProviderKillOutcome {
    let Some(pid) = sentinel.provider_pid else {
        return OrphanProviderKillOutcome::NotPresent;
    };
    if pid <= 0 || !pid_is_alive(pid) {
        return OrphanProviderKillOutcome::NotPresent;
    }
    // SAFETY: kill(pid, SIGTERM) is the standard way to ask a process to
    // terminate. SIGTERM lets postgres run its shutdown handler so PGDATA
    // is left in a clean state. Errors are surfaced as KillFailed; ESRCH
    // (raced with natural exit) is treated as success.
    let res = unsafe { libc::kill(pid, libc::SIGTERM) };
    if res == 0 {
        return OrphanProviderKillOutcome::Killed { pid };
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        OrphanProviderKillOutcome::NotPresent
    } else {
        OrphanProviderKillOutcome::KillFailed {
            pid,
            detail: err.to_string(),
        }
    }
}

#[cfg(not(unix))]
pub fn kill_orphan_provider(_sentinel: &SessionSentinel) -> OrphanProviderKillOutcome {
    OrphanProviderKillOutcome::NotPresent
}

#[cfg(unix)]
fn pid_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) is non-destructive — it only checks whether
    // the kernel can deliver a signal to the target. errno=ESRCH means
    // the pid does not exist; errno=EPERM means the pid exists but we
    // cannot signal it (still alive, just not ours). Anything else: we
    // conservatively report dead.
    let res = unsafe { libc::kill(pid, 0) };
    if res == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: i32) -> bool {
    // Non-unix platforms are not supported by Ato in v1. Fall back to
    // "dead" so the caller treats the sentinel as stale and proceeds.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sentinel(dir: &Path, pid: i32) -> SessionSentinel {
        let sentinel = SessionSentinel {
            session_pid: pid,
            provider_pid: None,
            started_at: "2026-05-04T12:00:00Z".to_string(),
            resolved: "capsule://ato/postgres@sha256:dead".to_string(),
        };
        write_session_sentinel(dir, &sentinel).expect("write");
        sentinel
    }

    #[test]
    fn no_sentinel_when_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = detect_orphan_state(dir.path(), std::process::id() as i32).expect("detect");
        assert!(matches!(outcome, OrphanCheckOutcome::NoSentinel));
    }

    #[test]
    fn alive_same_session_when_pid_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let me = std::process::id() as i32;
        write_sentinel(dir.path(), me);
        let outcome = detect_orphan_state(dir.path(), me).expect("detect");
        assert!(matches!(
            outcome,
            OrphanCheckOutcome::AliveSameSession { .. }
        ));
    }

    #[test]
    fn stale_dead_owner_when_pid_unreachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Pick an absurd pid that cannot exist (negative, but
        // detect_orphan_state stores it as i32 and pid_is_alive returns
        // false for pids ≤ 0).
        write_sentinel(dir.path(), -1);
        let outcome = detect_orphan_state(dir.path(), std::process::id() as i32).expect("detect");
        assert!(
            matches!(outcome, OrphanCheckOutcome::StaleDeadOwner { .. }),
            "got {outcome:?}"
        );
    }

    #[test]
    fn alive_other_session_when_pid_is_a_different_live_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Use pid 1 (init/launchd) — virtually guaranteed to be alive on
        // any unix system this test runs on.
        write_sentinel(dir.path(), 1);
        let outcome = detect_orphan_state(dir.path(), std::process::id() as i32).expect("detect");
        assert!(
            matches!(outcome, OrphanCheckOutcome::AliveOtherSession { .. }),
            "got {outcome:?}"
        );
    }

    #[test]
    fn sweep_removes_stale_sentinel() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_sentinel(dir.path(), -1);
        sweep_stale_sentinel(dir.path()).expect("sweep");
        assert!(!dir.path().join(SENTINEL_FILENAME).exists());
        // Sweeping when missing is a no-op.
        sweep_stale_sentinel(dir.path()).expect("sweep no-op");
    }

    #[test]
    fn rejects_corrupt_sentinel_payload() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(SENTINEL_FILENAME), "not-json").expect("write corrupt");
        let err = detect_orphan_state(dir.path(), 1).expect_err("must reject corrupt");
        assert!(
            matches!(err, OrphanError::ParseFailed { .. }),
            "got {err:?}"
        );
    }
}
