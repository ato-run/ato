//! Ready-State orphan overlay sweep (Phase 7.5).
//!
//! A long-lived Firecracker serving session leaves a writable overlay at
//! `<run-root>/ready-state-<pid>/` with a `.fc-session.json` record
//! (`{pid, tap, session_id}`). If the run process (or the host) crashes, the
//! overlay is orphaned. This sweep reclaims them, fail-closed:
//!
//! - **record present + VMM pid alive** → a live serving session; left untouched.
//! - **record present + VMM pid dead** → orphan; **reaped via the backend's
//!   record-driven `stop`** — the *same* path `ato stop` uses (kill recorded pid,
//!   delete recorded tap, remove the lockfile, remove the overlay), never a
//!   re-implementation.
//! - **no usable record** → it cannot be reaped safely (no pid/tap), so it is
//!   **quarantined** (moved aside, never blind-deleted) for operator inspection.
//!
//! Scope: Linux / Firecracker / Ready-State only (behind `ATO_READY_STATE_ENABLED`).
//! No Desktop Runner, no CRIU, no `BindingLease`. Reuses
//! [`pid_is_alive`](capsule::state::session::process::pid_is_alive),
//! [`RestoredSession`] + [`SnapshotBackend::stop`], and the run-root resolver, so
//! the sweep adds no new teardown logic. The classification is pure
//! ([`classify`]) and unit-tested KVM-free with the Fake backend.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use snapshot::{RestoredSession, SnapshotBackend};

/// A Ready-State overlay dir is named `ready-state-<pid>`.
const OVERLAY_PREFIX: &str = "ready-state-";
/// The on-disk session record `firecracker::restore` writes into the overlay.
const SESSION_RECORD: &str = ".fc-session.json";
/// Sub-dir of the run root that unrecorded overlays are moved into.
const QUARANTINE_DIR: &str = "quarantine";

/// Parsed `.fc-session.json` — the record the Firecracker restore stamps so a
/// cross-process reap has the authoritative pid + tap.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FcSessionRecord {
    pid: u32,
    #[serde(default)]
    tap: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

/// What the sweep decides for one overlay dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub(crate) enum Disposition {
    /// Record present + VMM pid alive → a live serving session; untouched.
    Live { pid: u32 },
    /// Record present + VMM pid dead → orphan; reaped via the backend.
    Reap { pid: u32 },
    /// No usable record → cannot reap safely; quarantined for inspection.
    Quarantine { reason: String },
}

/// Pure classification — the side-effecting pid probe is injected so this is
/// unit-tested without a live process.
fn classify(record: Option<&FcSessionRecord>, pid_alive: bool) -> Disposition {
    match record {
        Some(r) if pid_alive => Disposition::Live { pid: r.pid },
        Some(r) => Disposition::Reap { pid: r.pid },
        None => Disposition::Quarantine {
            reason: "no .fc-session.json record".to_string(),
        },
    }
}

/// One overlay's sweep outcome.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct OverlayOutcome {
    pub(crate) overlay: String,
    pub(crate) disposition: Disposition,
    /// Whether the reap/quarantine action was carried out (false for Live,
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
    pub(crate) quarantined: usize,
    pub(crate) failed: usize,
    pub(crate) outcomes: Vec<OverlayOutcome>,
}

/// Sweep orphan Ready-State overlays under the run root with the live host pid
/// probe. Best-effort: a per-overlay failure is recorded, never propagated.
pub(crate) fn sweep(backend: &dyn SnapshotBackend, dry_run: bool) -> SweepReport {
    let root = capsule::common::paths::ato_path_or_workspace_tmp("run");
    sweep_in(&root, backend, dry_run, &|pid| {
        capsule::state::session::process::pid_is_alive(pid)
    })
}

/// Testable core: enumerate `ready-state-*` overlays under `root` and act on each.
fn sweep_in(
    root: &Path,
    backend: &dyn SnapshotBackend,
    dry_run: bool,
    pid_alive: &dyn Fn(u32) -> bool,
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

        let record = read_record(&path);
        let alive = record.as_ref().map(|r| pid_alive(r.pid)).unwrap_or(false);
        let disposition = classify(record.as_ref(), alive);

        let (acted, note) = match &disposition {
            Disposition::Live { .. } => {
                report.live += 1;
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

fn read_record(overlay: &Path) -> Option<FcSessionRecord> {
    let raw = std::fs::read_to_string(overlay.join(SESSION_RECORD)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Reap via the backend's record-driven `stop` (the same teardown `ato stop`
/// uses): kill the recorded pid, delete the recorded tap, remove the lockfile,
/// and remove the overlay. We do **not** re-implement that sequence here.
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

    fn overlay_without_record(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("some-state"), b"x").unwrap();
        dir
    }

    #[test]
    fn classify_is_live_reap_quarantine() {
        let rec = FcSessionRecord {
            pid: 42,
            tap: Some("tap0".into()),
            session_id: Some("s".into()),
        };
        assert_eq!(classify(Some(&rec), true), Disposition::Live { pid: 42 });
        assert_eq!(classify(Some(&rec), false), Disposition::Reap { pid: 42 });
        assert!(matches!(
            classify(None, false),
            Disposition::Quarantine { .. }
        ));
    }

    #[test]
    fn live_session_is_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_with_record(tmp.path(), "ready-state-111", 111);
        let backend = FakeSnapshotBackend::new();
        // pid reported alive → Live.
        let report = sweep_in(tmp.path(), &backend, false, &|_| true);
        assert_eq!(report.scanned, 1);
        assert_eq!(report.live, 1);
        assert_eq!(report.reaped, 0);
        assert!(dir.exists(), "a live session's overlay must not be removed");
    }

    #[test]
    fn dead_session_overlay_is_reaped_via_backend() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_with_record(tmp.path(), "ready-state-222", 222);
        let backend = FakeSnapshotBackend::new();
        // pid reported dead → Reap → backend.stop removes the overlay.
        let report = sweep_in(tmp.path(), &backend, false, &|_| false);
        assert_eq!(report.reaped, 1);
        assert_eq!(report.failed, 0);
        assert!(!dir.exists(), "orphan overlay should be reaped");
    }

    #[test]
    fn unrecorded_overlay_is_quarantined_not_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = overlay_without_record(tmp.path(), "ready-state-333");
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(tmp.path(), &backend, false, &|_| false);
        assert_eq!(report.quarantined, 1);
        assert!(!dir.exists(), "original overlay moved");
        let quarantined = tmp.path().join(QUARANTINE_DIR).join("ready-state-333");
        assert!(quarantined.exists(), "overlay preserved under quarantine/");
        assert!(quarantined.join("some-state").exists(), "state preserved");
    }

    #[test]
    fn dry_run_acts_on_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let reapable = overlay_with_record(tmp.path(), "ready-state-444", 444);
        let unrecorded = overlay_without_record(tmp.path(), "ready-state-555");
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(tmp.path(), &backend, true, &|_| false);
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
        let report = sweep_in(tmp.path(), &backend, false, &|_| false);
        assert_eq!(report.scanned, 0, "only ready-state-* dirs are scanned");
    }

    #[test]
    fn missing_root_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = FakeSnapshotBackend::new();
        let report = sweep_in(&tmp.path().join("does-not-exist"), &backend, false, &|_| {
            false
        });
        assert_eq!(report.scanned, 0);
    }
}
