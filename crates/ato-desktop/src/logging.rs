//! Logging setup for `ato-desktop`.
//!
//! ## Output
//!
//! Every run writes to two sinks in parallel:
//! - **stderr** — human-readable, ANSI colours, no target prefix.
//! - **`~/.ato/logs/ato-desktop.YYYY-MM-DD.log`** — plain text, includes
//!   target prefix and thread ID for post-mortem analysis. Rotated daily;
//!   old files are left in place (prune manually or via a cron job).
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
//! 3. **Default** — `ato_desktop=info`, all feature targets at `warn`.
//!    Errors from gated targets still surface; routine chatter is silent.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

use capsule_core::common::paths::ato_path_or_workspace_tmp;

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
    let filter = build_env_filter();

    let log_dir = ato_path_or_workspace_tmp("logs");

    if std::fs::create_dir_all(&log_dir).is_ok() {
        let file_appender = tracing_appender::rolling::daily(&log_dir, "ato-desktop.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        Registry::default()
            .with(filter)
            .with(fmt::layer().with_target(false).with_writer(std::io::stderr))
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_ansi(false)
                    .with_writer(non_blocking),
            )
            .init();

        return Some(guard);
    }

    // Fallback: stderr only.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    None
}

fn build_env_filter() -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    EnvFilter::new(build_directives(
        std::env::var("ATO_DESKTOP_LOG").ok().as_deref(),
    ))
}

fn build_directives(ato_desktop_log: Option<&str>) -> String {
    let baseline_level = "info";
    let feature_default = "warn";
    let feature_enabled = "info";

    let mut directives: Vec<String> = std::iter::once(format!("ato_desktop={baseline_level}"))
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
                directives = std::iter::once("ato_desktop=debug".to_string())
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
    use super::build_directives;

    #[test]
    fn default_silences_feature_info_but_keeps_app_info() {
        let directives = build_directives(None);
        assert!(directives.contains("ato_desktop=info"));
        assert!(directives.contains("favicon=warn"));
        assert!(directives.contains("bridge=warn"));
        assert!(directives.contains("webview=warn"));
        assert!(directives.contains("orchestrator=warn"));
    }

    #[test]
    fn favicon_token_promotes_only_favicon_to_info() {
        let directives = build_directives(Some("favicon"));
        assert!(directives.contains("ato_desktop=info"));
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
        assert!(directives.contains("ato_desktop=debug"));
        assert!(directives.contains("favicon=debug"));
        assert!(directives.contains("bridge=debug"));
        assert!(directives.contains("webview=debug"));
        assert!(directives.contains("orchestrator=debug"));
        assert!(!directives.contains("ato_desktop=info"));
    }

    #[test]
    fn comma_separated_tokens_compose() {
        let directives = build_directives(Some(" favicon , bridge , bogus "));
        assert!(directives.contains("favicon=info"));
        assert!(directives.contains("bridge=info"));
        assert!(directives.contains("ato_desktop=info"));
        assert!(!directives.contains("favicon=warn"));
        assert!(!directives.contains("bridge=warn"));
    }

    #[test]
    fn empty_value_falls_back_to_default() {
        let directives = build_directives(Some(""));
        assert!(directives.contains("favicon=warn"));
    }
}
