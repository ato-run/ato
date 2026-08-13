//! `ato doctor disk` — surface the on-disk footprint of the local `~/.ato`
//! caches that grow silently (logs and SQLite state) and warn before they
//! bloat. Read-only: it only stats files, never deletes.
//!
//! Path resolution reuses the canonical resolvers rather than hardcoding
//! `~/.ato/...` strings:
//! - desktop logs: same dir as [`capsule::common::paths::ato_path_or_workspace_tmp`]
//!   `("logs")` used by `apps/desktop/src/logging.rs`.
//! - session logs: [`capsule::state::session::store::session_root`].
//! - engine/run logs: `ato_path_or_workspace_tmp("run")`, where the run
//!   pipeline writes `engine-*.log` (see `pipeline/phases/run.rs`).
//! - SQLite DBs (+ `-wal`/`-shm` sidecars): `installed_state.sqlite3`
//!   (`ato_state_dir()`), CAS `index.sqlite3` and `registry.sqlite3`.
//! - CAS content store: the `chunks/` subdirectory of the root
//!   `LocalCasIndex::open_default` resolves (`$ATO_CAS_ROOT` else `~/.ato/cas`).
//!   We measure `cas_root()/chunks` — the content blobs — rather than the whole
//!   root so the figure is DISJOINT from the CAS `index.sqlite3` row (which also
//!   lives under `cas_root()`); otherwise the TOTAL would double-count the index
//!   DB and its `-wal`/`-shm` sidecars.
//!
//! The size-aggregation and threshold logic is factored into pure functions
//! ([`directory_size`], [`category_warning`]) so it is unit-testable against a
//! temp tree without touching the real home directory.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;
use walkdir::WalkDir;

use capsule::common::paths::{
    ato_path_or_workspace_tmp, ato_state_dir, nacelle_home_dir_or_workspace_tmp,
};
use capsule::state::session::store::session_root;

/// Desktop log dir grows by one rolling file per day; warn past this.
const DESKTOP_LOGS_WARN_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
/// A single guest/engine log this large almost always means a runaway capsule.
const SINGLE_LOG_WARN_BYTES: u64 = 50 * 1024 * 1024; // 50 MB
/// Soft budget for the content-addressed store before suggesting a GC.
const CAS_WARN_BYTES: u64 = 5 * 1024 * 1024 * 1024; // 5 GB
/// Engine/run log dir (aggregate) soft budget.
const RUN_LOGS_WARN_BYTES: u64 = 100 * 1024 * 1024; // 100 MB

/// Kind of category, so the human/JSON renderers and the warning logic can
/// reason about a category without string-matching its display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CategoryKind {
    DesktopLogs,
    SessionLogs,
    RunLogs,
    Sqlite,
    CasStore,
}

/// One measured category of `~/.ato` disk usage.
#[derive(Debug, Clone, Serialize)]
pub struct DiskCategory {
    /// Stable display name, e.g. `"desktop logs"`.
    pub name: String,
    pub kind: CategoryKind,
    /// Resolved path that was measured.
    pub path: String,
    /// Recursive total size in bytes.
    pub bytes: u64,
    /// Largest single regular file under `path`, in bytes (0 if none).
    pub largest_file_bytes: u64,
    /// Whether `path` exists on disk.
    pub exists: bool,
    /// A human warning + prune hint, when a soft threshold is exceeded.
    pub warning: Option<String>,
}

#[derive(Debug, Serialize)]
struct DiskReport {
    categories: Vec<DiskCategory>,
    total_bytes: u64,
    warnings: Vec<String>,
}

/// Run `ato doctor disk`: measure each category, print largest-first with a
/// TOTAL, and emit WARNING lines for any category over its soft threshold.
pub fn run(json: bool) -> Result<()> {
    let categories = measure_categories(&resolve_targets());
    let report = build_report(categories);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    Ok(())
}

/// A category target before measurement: a label, its kind, and a path.
struct Target {
    name: &'static str,
    kind: CategoryKind,
    path: PathBuf,
}

/// Resolve every measured path from the canonical resolvers. SQLite targets
/// expand to the DB file plus its `-wal`/`-shm` sidecars at measure time.
fn resolve_targets() -> Vec<Target> {
    let mut targets = vec![
        Target {
            name: "desktop logs",
            kind: CategoryKind::DesktopLogs,
            path: ato_path_or_workspace_tmp("logs"),
        },
        Target {
            name: "engine/run logs",
            kind: CategoryKind::RunLogs,
            path: ato_path_or_workspace_tmp("run"),
        },
    ];

    // Session logs: `session_root()` honors ATO_DESKTOP_SESSION_ROOT; fall back
    // to its default path string only if the env resolution fails.
    let session_path =
        session_root().unwrap_or_else(|_| ato_path_or_workspace_tmp("apps/ato-desktop/sessions"));
    targets.push(Target {
        name: "session logs",
        kind: CategoryKind::SessionLogs,
        path: session_path,
    });

    // SQLite databases. Each row sums the DB plus its WAL/SHM sidecars.
    let state_dir = ato_state_dir();
    targets.push(Target {
        name: "installed_state.sqlite3",
        kind: CategoryKind::Sqlite,
        path: state_dir.join("installed_state.sqlite3"),
    });
    targets.push(Target {
        name: "registry.sqlite3",
        kind: CategoryKind::Sqlite,
        path: state_dir.join("registry.sqlite3"),
    });
    targets.push(Target {
        name: "CAS index.sqlite3",
        kind: CategoryKind::Sqlite,
        path: cas_root().join("index.sqlite3"),
    });

    // CAS content store: only the `chunks/` blobs (where LocalCasIndex writes
    // every chunk, rel_path `chunks/<aa>/<hash>`). Measuring the whole
    // `cas_root()` would re-count `index.sqlite3` (+ sidecars), which already
    // has its own row above; `chunks/` keeps the two categories disjoint so the
    // TOTAL is correct.
    targets.push(Target {
        name: "CAS content store",
        kind: CategoryKind::CasStore,
        path: cas_root().join("chunks"),
    });

    targets
}

/// The CAS root, resolved exactly as `LocalCasIndex::open_default`: the
/// `ATO_CAS_ROOT` override, else `~/.ato/cas`.
fn cas_root() -> PathBuf {
    match std::env::var("ATO_CAS_ROOT") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw),
        _ => nacelle_home_dir_or_workspace_tmp().join("cas"),
    }
}

/// Measure each target into a [`DiskCategory`], applying the warning rules.
fn measure_categories(targets: &[Target]) -> Vec<DiskCategory> {
    targets
        .iter()
        .map(|t| {
            let measure = match t.kind {
                CategoryKind::Sqlite => sqlite_measure(&t.path),
                _ => path_measure(&t.path),
            };
            let warning =
                category_warning(t.kind, t.name, measure.bytes, measure.largest_file_bytes);
            DiskCategory {
                name: t.name.to_string(),
                kind: t.kind,
                path: t.path.display().to_string(),
                bytes: measure.bytes,
                largest_file_bytes: measure.largest_file_bytes,
                exists: measure.exists,
                warning,
            }
        })
        .collect()
}

/// Aggregate result of walking a path.
struct Measure {
    bytes: u64,
    largest_file_bytes: u64,
    exists: bool,
}

/// Recursively sum the size of `path` (a file or directory), also tracking the
/// largest single regular file found. Unreadable entries are skipped rather
/// than aborting the whole report.
fn directory_size(path: &Path) -> Measure {
    if !path.exists() {
        return Measure {
            bytes: 0,
            largest_file_bytes: 0,
            exists: false,
        };
    }

    let mut total: u64 = 0;
    let mut largest: u64 = 0;
    for entry in WalkDir::new(path).follow_links(false).into_iter().flatten() {
        // Use symlink_metadata so a symlink counts as its own (tiny) size and
        // we never double-count or follow a link out of the tree.
        let meta = match entry.path().symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_file() {
            let len = meta.len();
            total = total.saturating_add(len);
            if len > largest {
                largest = len;
            }
        }
    }
    Measure {
        bytes: total,
        largest_file_bytes: largest,
        exists: true,
    }
}

/// Measure a single path (file or dir).
fn path_measure(path: &Path) -> Measure {
    directory_size(path)
}

/// Measure a SQLite DB as the DB file plus its `-wal` and `-shm` sidecars.
fn sqlite_measure(db_path: &Path) -> Measure {
    let mut total: u64 = 0;
    let mut largest: u64 = 0;
    let mut exists = false;
    for p in sqlite_family(db_path) {
        if let Ok(meta) = p.symlink_metadata()
            && meta.is_file()
        {
            exists = true;
            let len = meta.len();
            total = total.saturating_add(len);
            if len > largest {
                largest = len;
            }
        }
    }
    Measure {
        bytes: total,
        largest_file_bytes: largest,
        exists,
    }
}

/// The DB file plus its WAL/SHM sidecars (`foo.sqlite3`, `foo.sqlite3-wal`,
/// `foo.sqlite3-shm`).
fn sqlite_family(db_path: &Path) -> Vec<PathBuf> {
    let mut out = vec![db_path.to_path_buf()];
    if let Some(name) = db_path.file_name().and_then(|n| n.to_str())
        && let Some(parent) = db_path.parent()
    {
        out.push(parent.join(format!("{name}-wal")));
        out.push(parent.join(format!("{name}-shm")));
    }
    out
}

/// Decide whether a category exceeds its soft threshold, returning a warning
/// line with the relevant prune hint. Pure: no IO, so it is unit-testable.
fn category_warning(
    kind: CategoryKind,
    name: &str,
    bytes: u64,
    largest_file_bytes: u64,
) -> Option<String> {
    match kind {
        CategoryKind::DesktopLogs => {
            if bytes > DESKTOP_LOGS_WARN_BYTES {
                Some(format!(
                    "{name} is {} (> {}) — these are trimmed automatically by desktop startup retention; or remove old files manually under ~/.ato/logs",
                    fmt_bytes(bytes),
                    fmt_bytes(DESKTOP_LOGS_WARN_BYTES),
                ))
            } else {
                None
            }
        }
        CategoryKind::RunLogs => {
            if bytes > RUN_LOGS_WARN_BYTES {
                Some(format!(
                    "{name} is {} (> {}) — these are trimmed automatically by the guest-log TTL/size sweep; or remove old files manually under ~/.ato/run",
                    fmt_bytes(bytes),
                    fmt_bytes(RUN_LOGS_WARN_BYTES),
                ))
            } else if largest_file_bytes > SINGLE_LOG_WARN_BYTES {
                Some(format!(
                    "{name} has a single log of {} (> {}) — a capsule is logging excessively; inspect and remove it under ~/.ato/run",
                    fmt_bytes(largest_file_bytes),
                    fmt_bytes(SINGLE_LOG_WARN_BYTES),
                ))
            } else {
                None
            }
        }
        CategoryKind::SessionLogs => {
            if largest_file_bytes > SINGLE_LOG_WARN_BYTES {
                Some(format!(
                    "{name} has a single record/log of {} (> {}) — inspect it; stale sessions are trimmed automatically by the log sweep, or remove old files manually under ~/.ato/apps/ato-desktop/sessions",
                    fmt_bytes(largest_file_bytes),
                    fmt_bytes(SINGLE_LOG_WARN_BYTES),
                ))
            } else {
                None
            }
        }
        CategoryKind::CasStore => {
            if bytes > CAS_WARN_BYTES {
                Some(format!(
                    "{name} is {} (> {}) — large content-addressed cache; remove unreferenced blobs manually under ~/.ato/cas/chunks",
                    fmt_bytes(bytes),
                    fmt_bytes(CAS_WARN_BYTES),
                ))
            } else {
                None
            }
        }
        CategoryKind::Sqlite => None,
    }
}

/// Assemble the report: sort categories largest-first, sum the total, and
/// collect the warning lines.
fn build_report(mut categories: Vec<DiskCategory>) -> DiskReport {
    categories.sort_by_key(|c| std::cmp::Reverse(c.bytes));
    let total_bytes = categories
        .iter()
        .map(|c| c.bytes)
        .fold(0u64, u64::saturating_add);
    let warnings = categories
        .iter()
        .filter_map(|c| c.warning.clone())
        .collect();
    DiskReport {
        categories,
        total_bytes,
        warnings,
    }
}

/// Human-readable, fixed-precision byte size (binary units).
fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.2} GB", b / GB)
    }
}

fn print_human(report: &DiskReport) {
    println!("ato doctor disk — ~/.ato log & sqlite usage");
    println!();
    let name_w = report
        .categories
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(0)
        .max("TOTAL".len());
    for c in &report.categories {
        let marker = if c.exists { ' ' } else { '·' };
        println!(
            "  {marker} {:<name_w$}  {:>10}  {}",
            c.name,
            fmt_bytes(c.bytes),
            c.path,
        );
    }
    println!(
        "    {:<name_w$}  {:>10}",
        "TOTAL",
        fmt_bytes(report.total_bytes)
    );
    println!();
    if report.warnings.is_empty() {
        println!("  ✓ No category is over its soft threshold.");
    } else {
        for w in &report.warnings {
            println!("  ⚠ WARNING: {w}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(path: &Path, len: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![0u8; len]).unwrap();
    }

    #[test]
    fn directory_size_sums_recursively_and_tracks_largest() {
        let dir = tempdir().unwrap();
        write_file(&dir.path().join("a.log"), 100);
        write_file(&dir.path().join("sub/b.log"), 250);
        write_file(&dir.path().join("sub/deep/c.log"), 50);

        let m = directory_size(dir.path());
        assert!(m.exists);
        assert_eq!(m.bytes, 400);
        assert_eq!(m.largest_file_bytes, 250);
    }

    #[test]
    fn directory_size_missing_path_is_zero_and_absent() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let m = directory_size(&missing);
        assert!(!m.exists);
        assert_eq!(m.bytes, 0);
        assert_eq!(m.largest_file_bytes, 0);
    }

    #[test]
    fn sqlite_measure_includes_wal_and_shm_sidecars() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("installed_state.sqlite3");
        write_file(&db, 1000);
        write_file(&dir.path().join("installed_state.sqlite3-wal"), 500);
        write_file(&dir.path().join("installed_state.sqlite3-shm"), 32);
        // An unrelated file in the same dir must NOT be counted.
        write_file(&dir.path().join("registry.sqlite3"), 9999);

        let m = sqlite_measure(&db);
        assert!(m.exists);
        assert_eq!(m.bytes, 1000 + 500 + 32);
        assert_eq!(m.largest_file_bytes, 1000);
    }

    #[test]
    fn sqlite_measure_missing_db_is_absent() {
        let dir = tempdir().unwrap();
        let m = sqlite_measure(&dir.path().join("nope.sqlite3"));
        assert!(!m.exists);
        assert_eq!(m.bytes, 0);
    }

    #[test]
    fn desktop_logs_warning_fires_above_threshold() {
        let under = category_warning(
            CategoryKind::DesktopLogs,
            "desktop logs",
            DESKTOP_LOGS_WARN_BYTES,
            0,
        );
        assert!(under.is_none(), "exactly at threshold must not warn");

        let over = category_warning(
            CategoryKind::DesktopLogs,
            "desktop logs",
            DESKTOP_LOGS_WARN_BYTES + 1,
            0,
        )
        .expect("over threshold must warn");
        assert!(over.contains("desktop logs"));
        // The hint must point at automatic retention + the real directory, and
        // must NOT invent a non-existent `ato gc --logs` flag.
        assert!(
            over.contains("automatically") && over.contains("~/.ato/logs"),
            "warning must point at automatic retention and the real directory"
        );
        assert!(
            !over.contains("ato gc"),
            "warning must not reference the bogus `ato gc --logs` flag"
        );
    }

    #[test]
    fn run_logs_warns_on_single_large_log_even_when_aggregate_is_small() {
        // Aggregate under the run-logs budget, but one file over the single-log
        // threshold must still warn.
        let w = category_warning(
            CategoryKind::RunLogs,
            "engine/run logs",
            SINGLE_LOG_WARN_BYTES + 10,
            SINGLE_LOG_WARN_BYTES + 5,
        )
        .expect("single large log must warn");
        assert!(w.contains("single log"));
        assert!(w.contains("~/.ato/run"));
    }

    #[test]
    fn run_logs_aggregate_warning_takes_precedence() {
        let w = category_warning(
            CategoryKind::RunLogs,
            "engine/run logs",
            RUN_LOGS_WARN_BYTES + 1,
            10,
        )
        .expect("aggregate over budget must warn");
        assert!(w.contains("automatically") && w.contains("~/.ato/run"));
        assert!(!w.contains("ato gc"), "no bogus gc flag for run logs");
    }

    #[test]
    fn session_logs_warns_on_single_large_record() {
        let none = category_warning(
            CategoryKind::SessionLogs,
            "session logs",
            SINGLE_LOG_WARN_BYTES * 100,
            SINGLE_LOG_WARN_BYTES,
        );
        assert!(none.is_none(), "aggregate alone must not warn for sessions");

        let w = category_warning(
            CategoryKind::SessionLogs,
            "session logs",
            0,
            SINGLE_LOG_WARN_BYTES + 1,
        )
        .expect("single large record must warn");
        // Point at the automatic sweep + a real directory, not `ato gc`.
        assert!(w.contains("automatically") && w.contains("sessions"));
        assert!(
            !w.contains("ato gc"),
            "session hint must not reference `ato gc` (it prunes install revisions, not sessions/logs)"
        );
    }

    #[test]
    fn cas_store_warns_over_budget() {
        assert!(
            category_warning(
                CategoryKind::CasStore,
                "CAS content store",
                CAS_WARN_BYTES,
                0
            )
            .is_none()
        );
        let w = category_warning(
            CategoryKind::CasStore,
            "CAS content store",
            CAS_WARN_BYTES + 1,
            0,
        )
        .expect("CAS over budget must warn");
        // `ato gc` collects install revisions, not CAS blobs — don't suggest it.
        assert!(w.contains("~/.ato/cas/chunks"));
        assert!(
            !w.contains("ato gc"),
            "no bogus gc hint for the CAS content store"
        );
    }

    #[test]
    fn cas_content_store_excludes_index_db_so_total_does_not_double_count() {
        // Build a realistic CAS root: an index.sqlite3 (+ WAL/SHM sidecars) at
        // the root, and the content blobs under `chunks/`. The two categories
        // we measure — the SQLite family for `index.sqlite3` and the content
        // store at `chunks/` — must be disjoint, so summing them never
        // re-counts the index DB.
        let cas_root = tempdir().unwrap();
        let index_db = cas_root.path().join("index.sqlite3");
        write_file(&index_db, 4096);
        write_file(&cas_root.path().join("index.sqlite3-wal"), 1024);
        write_file(&cas_root.path().join("index.sqlite3-shm"), 32);
        // Content blobs, laid out as LocalCasIndex writes them: chunks/<aa>/<hash>.
        write_file(&cas_root.path().join("chunks/ab/abcd"), 5000);
        write_file(&cas_root.path().join("chunks/cd/cdef"), 7000);

        // This mirrors resolve_targets(): the content store is cas_root/chunks,
        // measured recursively; the index DB is measured as its sqlite family.
        let content = path_measure(&cas_root.path().join("chunks"));
        let index = sqlite_measure(&index_db);

        // Content store = blobs only, NOT the index DB or its sidecars.
        assert_eq!(content.bytes, 5000 + 7000);
        assert_eq!(index.bytes, 4096 + 1024 + 32);

        // Summing the two disjoint categories equals the on-disk sum of exactly
        // those files — no overlap, so build_report's TOTAL cannot double-count.
        assert_eq!(content.bytes + index.bytes, 12000 + 5152);

        // Sanity: measuring the WHOLE cas_root (the old, buggy behavior) would
        // include the index DB, proving the categories would have overlapped.
        let whole_root = path_measure(cas_root.path());
        assert_eq!(whole_root.bytes, content.bytes + index.bytes);
        assert!(
            whole_root.bytes > content.bytes,
            "whole-root figure includes the index DB that the content store excludes"
        );
    }

    #[test]
    fn sqlite_category_never_warns() {
        assert!(
            category_warning(CategoryKind::Sqlite, "registry.sqlite3", u64::MAX, u64::MAX)
                .is_none()
        );
    }

    #[test]
    fn build_report_sorts_largest_first_and_totals() {
        let cats = vec![
            DiskCategory {
                name: "small".into(),
                kind: CategoryKind::RunLogs,
                path: "/x".into(),
                bytes: 10,
                largest_file_bytes: 10,
                exists: true,
                warning: None,
            },
            DiskCategory {
                name: "big".into(),
                kind: CategoryKind::CasStore,
                path: "/y".into(),
                bytes: 1000,
                largest_file_bytes: 500,
                exists: true,
                warning: Some("over budget".into()),
            },
        ];
        let report = build_report(cats);
        assert_eq!(report.categories[0].name, "big");
        assert_eq!(report.categories[1].name, "small");
        assert_eq!(report.total_bytes, 1010);
        assert_eq!(report.warnings, vec!["over budget".to_string()]);
    }

    #[test]
    fn fmt_bytes_uses_binary_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(fmt_bytes(2 * 1024 * 1024 * 1024), "2.00 GB");
    }
}
