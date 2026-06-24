//! Logging setup for `ato-desktop`.
//!
//! ## Output
//!
//! Every run writes to two sinks in parallel:
//! - **stderr** — human-readable, ANSI colours, no target prefix.
//! - **`~/.ato/logs/ato-desktop.YYYY-MM-DD.log`** — plain text, includes
//!   target prefix and thread ID for post-mortem analysis. Rotated daily.
//!   Old files are pruned at startup: anything older than
//!   [`LOG_RETENTION_MAX_AGE`] is deleted, and the total on-disk size is
//!   capped at [`LOG_RETENTION_MAX_TOTAL_BYTES`] (oldest files removed first).
//!
//! ## File-sink level cap
//!
//! The stderr and file sinks have **independent** filters. `RUST_LOG=trace`
//! (or `ATO_DESKTOP_LOG=all`) makes stderr as verbose as requested, but the
//! persistent file layer is always capped at [`FILE_SINK_MAX_LEVEL`] so a
//! debugging session can never write unbounded `TRACE` volume to disk.
//!
//! ## Filter precedence
//!
//! 1. **`RUST_LOG`** — raw `tracing-subscriber` directives. When set,
//!    everything below is ignored. Reach for this when you need
//!    per-module fine control.
//! 2. **`ATO_DESKTOP_LOG`** — comma-separated feature names:
//!    - `favicon` — icon / favicon fetch, HTML parsing, ICO/SVG normalization.
//!    - `bridge` — guest<->host IPC message flow (requests, responses, denials).
//!    - `webview` — WebView lifecycle: mount, unmount, navigation, script eval.
//!    - `orchestrator` — capsule session lifecycle: spawn, stop, exit codes.
//!    - `all` — promotes everything to DEBUG.
//!    Unknown tokens are warned about on stderr and otherwise ignored.
//! 3. **Default** — `desktop=info`, all feature targets at `warn`.
//!    Errors from gated targets still surface; routine chatter is silent.

use std::path::Path;
use std::time::{Duration, SystemTime};

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::FilterExt;
use tracing_subscriber::{EnvFilter, Registry, fmt, prelude::*};

use capsule::common::paths::ato_path_or_workspace_tmp;

/// Filename prefix of the rolling desktop log files (`ato-desktop.YYYY-MM-DD.log`).
const LOG_FILE_PREFIX: &str = "ato-desktop.log";

/// Files older than this are pruned at startup regardless of total size.
const LOG_RETENTION_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Total on-disk size cap for the rolling log files. Once age-pruning is done,
/// the oldest remaining files are deleted until the total is under this cap.
const LOG_RETENTION_MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;

/// Hard ceiling on the level routed to the **persistent file sink**, applied
/// on top of whatever directive filter is active. The file is capped at DEBUG
/// even when stderr is asked for TRACE, so a verbose session cannot write
/// unbounded volume to disk.
const FILE_SINK_MAX_LEVEL: LevelFilter = LevelFilter::DEBUG;

/// `target:` value for icon / favicon plumbing.
pub const TARGET_FAVICON: &str = "favicon";
/// `target:` value for guest<->host IPC messages in `bridge.rs`.
pub const TARGET_BRIDGE: &str = "bridge";
/// `target:` value for WebView lifecycle events in `webview.rs`.
pub const TARGET_WEBVIEW: &str = "webview";
/// `target:` value for capsule session lifecycle in `orchestrator.rs`.
pub const TARGET_ORCHESTRATOR: &str = "orchestrator";

/// All targets that `ATO_DESKTOP_LOG=<name>` recognises.
const FEATURE_TARGETS: &[&str] = &[
    TARGET_FAVICON,
    TARGET_BRIDGE,
    TARGET_WEBVIEW,
    TARGET_ORCHESTRATOR,
];

/// Initialise the global tracing subscriber.
///
/// Returns a [`WorkerGuard`] that **must be kept alive** until the process
/// exits. Dropping it early stops the background log-writer thread and may
/// lose buffered log lines.
///
/// Falls back to stderr-only logging when the log directory cannot be created.
pub fn init_tracing() -> Option<WorkerGuard> {
    let log_dir = ato_path_or_workspace_tmp("logs");

    if std::fs::create_dir_all(&log_dir).is_ok() {
        // Best-effort retention sweep before opening the day's appender. A
        // prune failure must never block logging init, so we only warn.
        if let Err(err) = prune_log_dir(
            &log_dir,
            SystemTime::now(),
            LOG_RETENTION_MAX_AGE,
            LOG_RETENTION_MAX_TOTAL_BYTES,
        ) {
            eprintln!(
                "ato-desktop: log retention sweep failed for {}: {err}",
                log_dir.display()
            );
        }

        let file_appender = tracing_appender::rolling::daily(&log_dir, LOG_FILE_PREFIX);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        Registry::default()
            .with(
                fmt::layer()
                    .with_target(false)
                    .with_writer(std::io::stderr)
                    .with_filter(stderr_filter()),
            )
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_ansi(false)
                    .with_writer(non_blocking)
                    .with_filter(file_filter().and(FILE_SINK_MAX_LEVEL)),
            )
            .init();

        return Some(guard);
    }

    // Fallback: stderr only.
    tracing_subscriber::fmt()
        .with_env_filter(stderr_filter())
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    None
}

/// Filter for the stderr sink — honours `RUST_LOG` / `ATO_DESKTOP_LOG` verbatim.
fn stderr_filter() -> EnvFilter {
    build_env_filter()
}

/// Per-layer directive filter for the persistent file sink. Built from the same
/// directive source as stderr (`EnvFilter` is not `Clone`, so we rebuild it
/// rather than sharing one instance). At the call site this is AND-combined with
/// [`FILE_SINK_MAX_LEVEL`] via [`FilterExt::and`], giving a hard level ceiling
/// that applies to *every* directive — including per-target ones like
/// `desktop=trace` — so `RUST_LOG=trace` can never write TRACE volume to disk.
fn file_filter() -> EnvFilter {
    build_env_filter()
}

fn build_env_filter() -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    EnvFilter::new(build_directives(
        std::env::var("ATO_DESKTOP_LOG").ok().as_deref(),
    ))
}

/// Prune rolling desktop log files under `log_dir`.
///
/// Files named `ato-desktop.log.*` (the rolling-appender daily files share the
/// [`LOG_FILE_PREFIX`] stem) are pruned in two passes:
///
/// 1. **Age** — any file whose modified time is older than `max_age` relative
///    to `now` is deleted.
/// 2. **Size cap** — if the surviving files still exceed `max_total_bytes`,
///    the oldest are deleted first until the total is under the cap.
///
/// Factored out of [`init_tracing`] (taking an explicit `now`, `max_age`, and
/// `max_total_bytes`) so the retention policy is unit-testable against a temp
/// directory. Best-effort: individual file errors (stat/remove) are tolerated
/// and the sweep continues; only a failure to list the directory is returned.
fn prune_log_dir(
    log_dir: &Path,
    now: SystemTime,
    max_age: Duration,
    max_total_bytes: u64,
) -> std::io::Result<()> {
    let mut survivors: Vec<(SystemTime, u64, std::path::PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(log_dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();

        // Only touch our own rolling log files. The daily appender writes
        // `ato-desktop.log.YYYY-MM-DD`, so match on the prefix stem.
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(LOG_FILE_PREFIX) {
            continue;
        }

        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        let size = meta.len();

        // Pass 1: age cut.
        let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
        if age > max_age {
            let _ = std::fs::remove_file(&path);
            continue;
        }

        survivors.push((modified, size, path));
    }

    // Pass 2: size cap. Delete oldest-first until under the cap.
    let mut total: u64 = survivors.iter().map(|(_, size, _)| *size).sum();
    if total > max_total_bytes {
        // Oldest first (ascending modified time).
        survivors.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, size, path) in &survivors {
            if total <= max_total_bytes {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(*size);
            }
        }
    }

    Ok(())
}

fn build_directives(ato_desktop_log: Option<&str>) -> String {
    let baseline_level = "info";
    let feature_default = "warn";
    let feature_enabled = "info";

    let mut directives: Vec<String> = std::iter::once(format!("desktop={baseline_level}"))
        .chain(
            FEATURE_TARGETS
                .iter()
                .map(|t| format!("{t}={feature_default}")),
        )
        .collect();

    let Some(raw) = ato_desktop_log else {
        return directives.join(",");
    };

    for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match token {
            "all" => {
                directives = std::iter::once("desktop=debug".to_string())
                    .chain(FEATURE_TARGETS.iter().map(|t| format!("{t}=debug")))
                    .collect();
            }
            feature if FEATURE_TARGETS.contains(&feature) => {
                directives.retain(|d| !d.starts_with(&format!("{feature}=")));
                directives.push(format!("{feature}={feature_enabled}"));
            }
            other => {
                eprintln!(
                    "ato-desktop: ignoring unknown ATO_DESKTOP_LOG token `{other}` \
                     (known: all, {})",
                    FEATURE_TARGETS.join(", ")
                );
            }
        }
    }

    directives.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    /// Write a file in `dir` and back-date its mtime to `now - age`.
    fn write_aged_file(dir: &Path, name: &str, len: usize, now: SystemTime, age: Duration) {
        let path = dir.join(name);
        fs::write(&path, vec![b'x'; len]).unwrap();
        let mtime = now - age;
        let ft = filetime::FileTime::from_system_time(mtime);
        filetime::set_file_mtime(&path, ft).unwrap();
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn prune_deletes_files_older_than_max_age() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let max_age = Duration::from_secs(30 * 24 * 60 * 60);

        write_aged_file(dir.path(), "ato-desktop.log.2026-01-01", 10, now, max_age * 2);
        write_aged_file(
            dir.path(),
            "ato-desktop.log.2026-06-01",
            10,
            now,
            Duration::from_secs(60),
        );

        prune_log_dir(dir.path(), now, max_age, u64::MAX).unwrap();

        assert_eq!(names_in(dir.path()), vec!["ato-desktop.log.2026-06-01"]);
    }

    #[test]
    fn prune_enforces_size_cap_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let max_age = Duration::from_secs(30 * 24 * 60 * 60);

        // Three 100-byte files, none age-expired, ascending age (oldest last
        // written name is the smallest date). Cap at 250 bytes => the single
        // oldest must be deleted, leaving two (200 bytes).
        write_aged_file(
            dir.path(),
            "ato-desktop.log.day1",
            100,
            now,
            Duration::from_secs(3 * 86400),
        );
        write_aged_file(
            dir.path(),
            "ato-desktop.log.day2",
            100,
            now,
            Duration::from_secs(2 * 86400),
        );
        write_aged_file(
            dir.path(),
            "ato-desktop.log.day3",
            100,
            now,
            Duration::from_secs(86400),
        );

        prune_log_dir(dir.path(), now, max_age, 250).unwrap();

        // day1 (oldest) removed; the two newest survive.
        assert_eq!(
            names_in(dir.path()),
            vec!["ato-desktop.log.day2", "ato-desktop.log.day3"]
        );
    }

    #[test]
    fn prune_keeps_everything_under_both_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let max_age = Duration::from_secs(30 * 24 * 60 * 60);

        write_aged_file(
            dir.path(),
            "ato-desktop.log.recent",
            50,
            now,
            Duration::from_secs(86400),
        );

        prune_log_dir(dir.path(), now, max_age, 1024).unwrap();

        assert_eq!(names_in(dir.path()), vec!["ato-desktop.log.recent"]);
    }

    #[test]
    fn prune_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let max_age = Duration::from_secs(30 * 24 * 60 * 60);

        // An old, large foreign file must never be touched by our sweep.
        write_aged_file(dir.path(), "other-app.log", 10_000, now, max_age * 5);
        write_aged_file(
            dir.path(),
            "ato-desktop.log.old",
            10,
            now,
            max_age * 2,
        );

        prune_log_dir(dir.path(), now, max_age, 0).unwrap();

        // ato-desktop.log.old is age-pruned; foreign file untouched.
        assert_eq!(names_in(dir.path()), vec!["other-app.log"]);
    }

    #[test]
    fn prune_missing_dir_is_err_not_panic() {
        let now = SystemTime::now();
        let missing = std::path::Path::new("/nonexistent/ato/logs/path/xyz");
        assert!(prune_log_dir(missing, now, Duration::from_secs(1), 1).is_err());
    }

    #[test]
    fn file_filter_caps_trace_to_debug() {
        // Simulate a verbose `RUST_LOG=trace` directive feeding the file sink.
        // The file layer AND-combines the env filter with FILE_SINK_MAX_LEVEL,
        // so the effective ceiling must drop to DEBUG, never TRACE.
        let verbose = EnvFilter::new("trace");
        let capped = verbose.and(FILE_SINK_MAX_LEVEL);

        let max = <_ as tracing_subscriber::layer::Filter<Registry>>::max_level_hint(&capped);
        assert_eq!(max, Some(FILE_SINK_MAX_LEVEL));
        assert_eq!(FILE_SINK_MAX_LEVEL, LevelFilter::DEBUG);
        assert!(FILE_SINK_MAX_LEVEL < LevelFilter::TRACE);
    }

    #[test]
    fn file_filter_does_not_raise_a_quiet_env() {
        // The cap is a ceiling, not a floor: an INFO env stays INFO.
        let quiet = EnvFilter::new("info");
        let capped = quiet.and(FILE_SINK_MAX_LEVEL);
        let max = <_ as tracing_subscriber::layer::Filter<Registry>>::max_level_hint(&capped);
        assert_eq!(max, Some(LevelFilter::INFO));
    }

    #[test]
    fn default_silences_feature_info_but_keeps_app_info() {
        let directives = build_directives(None);
        assert!(directives.contains("desktop=info"));
        assert!(directives.contains("favicon=warn"));
        assert!(directives.contains("bridge=warn"));
        assert!(directives.contains("webview=warn"));
        assert!(directives.contains("orchestrator=warn"));
    }

    #[test]
    fn favicon_token_promotes_only_favicon_to_info() {
        let directives = build_directives(Some("favicon"));
        assert!(directives.contains("desktop=info"));
        assert!(directives.contains("favicon=info"));
        assert!(!directives.contains("favicon=warn"));
        assert!(directives.contains("bridge=warn"));
    }

    #[test]
    fn bridge_token_promotes_only_bridge_to_info() {
        let directives = build_directives(Some("bridge"));
        assert!(directives.contains("bridge=info"));
        assert!(!directives.contains("bridge=warn"));
        assert!(directives.contains("favicon=warn"));
    }

    #[test]
    fn all_token_promotes_app_to_debug_and_features_to_debug() {
        let directives = build_directives(Some("all"));
        assert!(directives.contains("desktop=debug"));
        assert!(directives.contains("favicon=debug"));
        assert!(directives.contains("bridge=debug"));
        assert!(directives.contains("webview=debug"));
        assert!(directives.contains("orchestrator=debug"));
        assert!(!directives.contains("desktop=info"));
    }

    #[test]
    fn comma_separated_tokens_compose() {
        let directives = build_directives(Some(" favicon , bridge , bogus "));
        assert!(directives.contains("favicon=info"));
        assert!(directives.contains("bridge=info"));
        assert!(directives.contains("desktop=info"));
        assert!(!directives.contains("favicon=warn"));
        assert!(!directives.contains("bridge=warn"));
    }

    #[test]
    fn empty_value_falls_back_to_default() {
        let directives = build_directives(Some(""));
        assert!(directives.contains("favicon=warn"));
    }
}
