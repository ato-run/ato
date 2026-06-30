//! Ready-State orphan overlay sweep (Phase 7.5).
//!
//! A long-lived Firecracker serving session leaves a writable overlay at
//! `<run-root>/ready-state-<pid>/` with a `.fc-session.json` record
//! (`{pid, tap, session_id}`). If the run process (or the host) crashes, the
//! overlay is orphaned. This sweep reclaims them — but **never destroys a live or
//! in-progress session**, so it is conservative:
//!
//! - **usable record + VMM pid alive** → a live serving session; left untouched.
//! - **usable record + VMM pid dead** → orphan; **reaped via the backend's
//!   record-driven `stop`** — the *same* path `ato stop` uses (kill recorded pid,
//!   delete recorded **tap**, remove the lockfile, remove the overlay), never a
//!   re-implementation.
//! - **no usable record, overlay younger than [`ORPHAN_GRACE`]** → **skipped**: a
//!   concurrent `ato run` may have just created the dir and not yet written its
//!   `.fc-session.json`; quarantining it would destroy a live restore.
//! - **no usable record, overlay older than the grace window** → **quarantined**
//!   (moved aside, never blind-deleted) for operator inspection.
//!
//! A record is *usable* only when it has a non-zero `pid` **and** a non-empty
//! `tap` ([`read_usable_record`]). Record-driven Firecracker teardown deletes the
//! **recorded** tap; a record without a usable tap would force `backend.stop` to
//! fall back to a default/current tap and risk deleting the wrong device — so
//! such a record (and any malformed/partially-written one) is treated as *no
//! usable record* and quarantined after the grace window, never reaped.
//!
//! Scope: Linux / Firecracker / Ready-State only (behind `ATO_READY_STATE_ENABLED`).
//! No Desktop Runner, no CRIU, no `BindingLease`. Reuses
//! [`pid_is_alive`](capsule::state::session::process::pid_is_alive),
//! [`RestoredSession`] + [`SnapshotBackend::stop`], and the run-root resolver, so
//! the sweep adds no new teardown logic. The classification is pure
//! ([`classify`]) and unit-tested KVM-free with the Fake backend.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use snapshot::{RestoredSession, SnapshotBackend};

/// A Ready-State overlay dir is named `ready-state-<pid>`.
const OVERLAY_PREFIX: &str = "ready-state-";
/// The on-disk session record `firecracker::restore` writes into the overlay.
const SESSION_RECORD: &str = ".fc-session.json";
/// Sub-dir of the run root that unrecorded overlays are moved into.
const QUARANTINE_DIR: &str = "quarantine";
/// An overlay younger than this with no usable record may be a concurrent
/// restore mid-flight (dir created, record not yet written) — skip it, don't
/// quarantine. Tuned to comfortably exceed the restore→record-write window.
const ORPHAN_GRACE: Duration = Duration::from_secs(30);

/// Parsed `.fc-session.json` — the record the Firecracker restore stamps so a
/// cross-process reap has the authoritative pid **and** tap. `tap` is required:
/// a record missing it is not usable for record-driven teardown.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FcSessionRecord {
    pid: u32,
    tap: String,
    #[serde(default)]
    session_id: Option<String>,
}

/// What the sweep decides for one overlay dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub(crate) enum Disposition {
    /// Usable record + VMM pid alive → a live serving session; untouched.
    Live { pid: u32 },
    /// Usable record + VMM pid dead → orphan; reaped via the backend.
    Reap { pid: u32 },
    /// No usable record but the overlay is younger than the grace window — it may
    /// be a concurrent restore mid-flight; left untouched this pass.
    Skip { reason: String },
    /// No usable record and past the grace window → quarantined for inspection.
    Quarantine { reason: String },
}

/// Pure classification — the side-effecting pid probe and freshness check are
/// injected so the full matrix is unit-tested without a live process / clock.
///
/// `fresh` matters only when there is no usable record (a usable record always
/// resolves to Live/Reap regardless of overlay age).
fn classify(record: Option<&FcSessionRecord>, pid_alive: bool, fresh: bool) -> Disposition {
    match record {
        Some(r) if pid_alive => Disposition::Live { pid: r.pid },
        Some(r) => Disposition::Reap { pid: r.pid },
        None if fresh => Disposition::Skip {
            reason: "fresh overlay with no usable record (possible in-progress restore)"
                .to_string(),
        },
        None => Disposition::Quarantine {
            reason: "no usable .fc-session.json record (pid+tap)".to_string(),
        },
    }
}

/// One overlay's sweep outcome.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OverlayOutcome {
    pub(crate) overlay: String,
    pub(crate) disposition: Disposition,
    /// Whether the reap/quarantine action was carried out (false for Live, Skip,
    /// dry-run, or a recorded failure).
    pub(crate) acted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// The result of a sweep pass.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct SweepReport {
    pub(crate) root: String,
    pub(crate) scanned: usize,
    pub(crate) live: usize,
    pub(crate) reaped: usize,
    pub(crate) skipped: usize,
    pub(crate) quarantined: usize,
    pub(crate) failed: usize,
    pub(crate) outcomes: Vec<OverlayOutcome>,
}

/// Sweep orphan Ready-State overlays under the run root with the live host pid
/// probe and overlay-mtime freshness. Best-effort: a per-overlay failure is
/// recorded, never propagated.
pub(crate) fn sweep(backend: &dyn SnapshotBackend, dry_run: bool) -> SweepReport {
    let root = capsule::common::paths::ato_path_or_workspace_tmp("run");
    sweep_in(
        &root,
        backend,
        dry_run,
        &|pid| capsule::state::session::process::pid_is_alive(pid),
        &|path| overlay_is_fresh(path, ORPHAN_GRACE),
    )
}

/// Testable core: enumerate `ready-state-*` overlays under `root` and act on each.
fn sweep_in(
    root: &Path,
    backend: &dyn SnapshotBackend,
    dry_run: bool,
    pid_alive: &dyn Fn(u32) -> bool,
    is_fresh: &dyn Fn(&Path) -> bool,
) -> SweepReport {
    let mut report = SweepReport {
        root: root.display().to_string(),
        ..Default::default()
    };
    let Ok(entries) = std::fs::read_dir(root) else {
        return report;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(OVERLAY_PREFIX) || !path.is_dir() {
            continue;
        }
        report.scanned += 1;

        let record = read_usable_record(&path);
        let alive = record.as_ref().map(|r| pid_alive(r.pid)).unwrap_or(false);
        // Freshness only matters for the no-usable-record branch; compute it
        // only then so a recorded overlay never depends on mtime.
        let fresh = record.is_none() && is_fresh(&path);
        let disposition = classify(record.as_ref(), alive, fresh);

        let (acted, note) = match &disposition {
            Disposition::Live { .. } => {
                report.live += 1;
                (false, None)
            }
            Disposition::Skip { .. } => {
                report.skipped += 1;
                (false, None)
            }
            Disposition::Reap { pid } => {
                if dry_run {
                    (false, Some("dry-run".to_string()))
                } else {
                    match reap(backend, &path, record.as_ref(), *pid) {
                        Ok(()) => {
                            report.reaped += 1;
                            (true, None)
                        }
                        Err(e) => {
                            report.failed += 1;
                            (false, Some(format!("reap failed: {e}")))
                        }
                    }
                }
            }
            Disposition::Quarantine { .. } => {
                if dry_run {
                    (false, Some("dry-run".to_string()))
                } else {
                    match quarantine(root, &path, &name) {
                        Ok(dest) => {
                            report.quarantined += 1;
                            (true, Some(format!("→ {dest}")))
                        }
                        Err(e) => {
                            report.failed += 1;
                            (false, Some(format!("quarantine failed: {e}")))
                        }
                    }
                }
            }
        };
        report.outcomes.push(OverlayOutcome {
            overlay: name,
            disposition,
            acted,
            note,
        });
    }
    report
}

/// Read a `.fc-session.json` record only if it is **usable** for record-driven
/// teardown: parseable, non-zero `pid`, and a non-empty `tap`. A missing,
/// malformed, partially-written, or tap-less record returns `None` (→ treated as
/// no usable record), so we never reap on incomplete information.
fn read_usable_record(overlay: &Path) -> Option<FcSessionRecord> {
    let raw = std::fs::read_to_string(overlay.join(SESSION_RECORD)).ok()?;
    let record: FcSessionRecord = serde_json::from_str(&raw).ok()?;
    if record.pid == 0 || record.tap.trim().is_empty() {
        return None;
    }
    Some(record)
}

/// Whether `overlay`'s mtime is younger than `grace`. Conservative: if the mtime
/// cannot be determined (or is in the future), treat the overlay as fresh so a
/// possibly-live restore is skipped rather than quarantined.
fn overlay_is_fresh(overlay: &Path, grace: Duration) -> bool {
    let Ok(meta) = std::fs::metadata(overlay) else {
        return true;
    };
    let Ok(mtime) = meta.modified() else {
        return true;
    };
    match mtime.elapsed() {
        Ok(age) => age < grace,
        Err(_) => true, // mtime in the future → treat as fresh.
    }
}

/// Reap via the backend's record-driven `stop` (the same teardown `ato stop`
/// uses): kill the recorded pid, delete the recorded tap, remove the lockfile,
/// and remove the overlay. We do **not** re-implement that sequence here. Only
/// reached for a usable record (pid + tap), so `backend.stop` reads the same
/// record and deletes the recorded tap.
fn reap(
    backend: &dyn SnapshotBackend,
    overlay: &Path,
    record: Option<&FcSessionRecord>,
    pid: u32,
) -> Result<()> {
    let session = RestoredSession {
        session_id: record
            .and_then(|r| r.session_id.clone())
            .unwrap_or_else(|| format!("orphan-{pid}")),
        backend_id: backend.id().to_string(),
        guest_port: None,
        overlay_root: overlay.to_path_buf(),
        restored_bytes: 0,
        vmm_pid: Some(pid as i32),
    };
    backend
        .stop(session)
        .map(|_| ())
        .map_err(anyhow::Error::new)
}

/// Move an unrecorded overlay aside (never blind-delete — it may hold state an
/// operator needs to inspect).
fn quarantine(root: &Path, overlay: &Path, name: &str) -> Result<String> {
    let qdir = root.join(QUARANTINE_DIR);
    std::fs::create_dir_all(&qdir)?;
    let mut dest = qdir.join(name);
    if dest.exists() {
        // Never clobber a prior quarantine; suffix with this pid.
        dest = qdir.join(format!("{name}.{}", std::process::id()));
    }
    std::fs::rename(overlay, &dest)?;
    Ok(dest.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapshot::FakeSnapshotBackend;
    use std::fs;
    use std::path::PathBuf;

    /// An overlay with a usable record (pid + tap).
    fn overlay_with_record(root: &Path, name: &str, pid: u32) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(SESSION_RECORD),
            format!(r#"{{"pid":{pid},"tap":"tap-test","session_id":"fc-abc-{pid}"}}"#),
        )
        .unwrap();
        dir
    }

    /// An overlay whose record has a pid but no tap (not usable).
    fn overlay_with_pid_no_tap(root: &Path, name: &str, pid: u32) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(SESSION_RECORD), format!(r#"{{"pid":{pid}}}"#)).unwrap();
        dir
    }

    /// An overlay with no record at all (e.g. a just-created restore dir).
    fn overlay_without_record(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("some-state"), b"x").unwrap();
        dir
    }

    const ALWAYS_OLD: &dyn Fn(&Path) -> bool = &|_| false;
    const ALWAYS_FRESH: &dyn Fn(&Path) -> bool = &|_| true;

    #[test]
    fn classify_covers_live_reap_skip_quarantine() {
        let rec = FcSessionRecord {
            pid: 42,
            tap: "tap0".into(),
            session_id: Some("s".into()),
        };
        assert_eq!(
            classify(Some(&rec), true, false),
            Disposition::Live { pid: 42 }
        );
        assert_eq!(
            classify(Some(&rec), false, false),
            Disposition::Reap { pid: 42 }
        );
        assert!(matches!(
            classify(None, false, true),
            Disposition::Skip { .. }
        ));
        assert!(matches!(
            classify(None, false, false),
            Disposition::Quarantine { .. }
        ));
    }

    #[test]
    fn read_usable_record_requires_pid_and_tap() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_usable_record(&overlay_with_record(tmp.path(), "ready-state-1", 1)).is_some());
        assert!(
            read_usable_record(&overlay_with_pid_no_tap(tmp.path(), "ready-state-2", 2)).is_none()
        );
        // Empty tap and pid=0 are both unusable.
        let d = tmp.path().join("ready-state-3");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(SESSION_RECORD), r#"{"pid":3,"tap":"  "}"#).unwrap();
        assert!(read_usable_record(&d).is_none());
        let d = tmp.path().join("ready-state-4");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(SESSION_RECORD), r#"{"pid":0,"tap":"tap0"}"#).unwrap();
        assert!(read_usable_record(&d).is_none());
        // Malformed / partially-written JSON → unusable, not a panic.
        let d = tmp.path().join("ready-state-5");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(SESSION_RECORD), r#"{"pid":5,"tap":"tap"#).unwrap();
        assert!(read_usable_record(&d).is_none());
    }

    #[test]
    fn live_session_is_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_with_record(tmp.path(), "ready-state-111", 111);
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(tmp.path(), &backend, false, &|_| true, ALWAYS_OLD);
        assert_eq!(report.live, 1);
        assert_eq!(report.reaped, 0);
        assert!(dir.exists(), "a live session's overlay must not be removed");
    }

    #[test]
    fn dead_session_with_pid_and_tap_is_reaped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_with_record(tmp.path(), "ready-state-222", 222);
        let backend = FakeSnapshotBackend::new();
        // pid dead → Reap → backend.stop removes the overlay.
        let report = sweep_in(tmp.path(), &backend, false, &|_| false, ALWAYS_OLD);
        assert_eq!(report.reaped, 1);
        assert_eq!(report.failed, 0);
        assert!(
            !dir.exists(),
            "orphan overlay with pid+tap should be reaped"
        );
    }

    #[test]
    fn record_with_pid_but_no_tap_is_quarantined_not_reaped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_with_pid_no_tap(tmp.path(), "ready-state-333", 333);
        let backend = FakeSnapshotBackend::new();
        // Even with a dead pid, a tap-less record must NOT be reaped.
        let report = sweep_in(tmp.path(), &backend, false, &|_| false, ALWAYS_OLD);
        assert_eq!(report.reaped, 0);
        assert_eq!(report.quarantined, 1);
        assert!(!dir.exists());
        assert!(
            tmp.path()
                .join(QUARANTINE_DIR)
                .join("ready-state-333")
                .exists()
        );
    }

    #[test]
    fn fresh_no_record_overlay_is_skipped_not_quarantined() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_without_record(tmp.path(), "ready-state-444");
        let backend = FakeSnapshotBackend::new();
        // Fresh (younger than grace) → Skip; a concurrent restore may own it.
        let report = sweep_in(tmp.path(), &backend, false, &|_| false, ALWAYS_FRESH);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.quarantined, 0);
        assert!(
            dir.exists(),
            "a fresh in-progress overlay must not be touched"
        );
    }

    #[test]
    fn old_no_record_overlay_is_quarantined_preserving_state() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_without_record(tmp.path(), "ready-state-555");
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(tmp.path(), &backend, false, &|_| false, ALWAYS_OLD);
        assert_eq!(report.quarantined, 1);
        assert!(!dir.exists(), "original overlay moved");
        let q = tmp.path().join(QUARANTINE_DIR).join("ready-state-555");
        assert!(
            q.exists() && q.join("some-state").exists(),
            "state preserved"
        );
    }

    #[test]
    fn dry_run_acts_on_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let reapable = overlay_with_record(tmp.path(), "ready-state-666", 666);
        let unrecorded = overlay_without_record(tmp.path(), "ready-state-777");
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(tmp.path(), &backend, true, &|_| false, ALWAYS_OLD);
        assert_eq!(report.scanned, 2);
        assert_eq!(report.reaped, 0);
        assert_eq!(report.quarantined, 0);
        assert!(report.outcomes.iter().all(|o| !o.acted));
        assert!(
            reapable.exists() && unrecorded.exists(),
            "dry-run touches nothing"
        );
    }

    #[test]
    fn non_overlay_entries_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("engine-logs")).unwrap();
        fs::write(tmp.path().join("ready-state-not-a-dir"), b"file").unwrap();
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(tmp.path(), &backend, false, &|_| false, ALWAYS_OLD);
        assert_eq!(report.scanned, 0, "only ready-state-* dirs are scanned");
    }

    #[test]
    fn missing_root_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(
            &tmp.path().join("does-not-exist"),
            &backend,
            false,
            &|_| false,
            ALWAYS_OLD,
        );
        assert_eq!(report.scanned, 0);
    }

    #[test]
    fn overlay_is_fresh_uses_mtime_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_without_record(tmp.path(), "ready-state-888");
        // Just-created → fresh under a 30s grace, not fresh under a 0s grace.
        assert!(overlay_is_fresh(&dir, Duration::from_secs(30)));
        assert!(!overlay_is_fresh(&dir, Duration::from_secs(0)));
    }
}
