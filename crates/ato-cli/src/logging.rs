//! Logging filter glue for `ato-cli`.
//!
//! Mirrors `ato-desktop`'s logging story so users have one mental
//! model for both binaries. Two env vars drive the active filter, in
//! this precedence order:
//!
//! 1. **`RUST_LOG`** — raw `tracing-subscriber` directives. When set,
//!    everything below is ignored.
//! 2. **`ATO_CLI_LOG`** — comma-separated feature names with a
//!    friendlier vocabulary than the directive grammar:
//!    - `node-compat` — surface INFO output from the node-compat
//!      executor (assembled Deno command lines, etc.). Useful for
//!      diagnosing permission denials and stale-binary issues.
//!    - `all` — turn on every per-feature target plus app DEBUG.
//!    Unknown tokens are warned about on stderr and otherwise ignored.
//! 3. **Default** — `ato_cli=info` baseline plus `<feature>=warn` for
//!    every per-feature target. Errors stay visible; routine INFO
//!    chatter from the gated targets is silent until opted in.

use tracing_subscriber::EnvFilter;

/// `target:` value for node-compat (Deno-driven Node guest) plumbing —
/// command assembly, permission shaping, runtime invocation. Tagging
/// these traces lets `ATO_CLI_LOG=node-compat` surface them on demand
/// without flooding default output.
pub const TARGET_NODE_COMPAT: &str = "node-compat";

const FEATURE_TARGETS: &[&str] = &[TARGET_NODE_COMPAT];

pub fn build_env_filter() -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    EnvFilter::new(build_directives(
        std::env::var("ATO_CLI_LOG").ok().as_deref(),
    ))
}

fn build_directives(ato_cli_log: Option<&str>) -> String {
    let mut directives: Vec<String> = std::iter::once("ato_cli=info".to_string())
        .chain(FEATURE_TARGETS.iter().map(|t| format!("{t}=warn")))
        .collect();

    let Some(raw) = ato_cli_log else {
        return directives.join(",");
    };

    for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match token {
            "all" => {
                directives = std::iter::once("ato_cli=debug".to_string())
                    .chain(FEATURE_TARGETS.iter().map(|t| format!("{t}=debug")))
                    .collect();
            }
            feature if FEATURE_TARGETS.contains(&feature) => {
                directives.retain(|d| !d.starts_with(&format!("{feature}=")));
                directives.push(format!("{feature}=info"));
            }
            other => {
                eprintln!(
                    "ato-cli: ignoring unknown ATO_CLI_LOG token `{other}` \
                     (known: all, {})",
                    FEATURE_TARGETS.join(", ")
                );
            }
        }
    }

    directives.join(",")
}

/// Idempotent subscriber init. Safe to call multiple times — used both
/// in `main_entry` and in test helpers that need stderr breadcrumbs.
pub fn init_subscriber() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(build_env_filter())
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::build_directives;

    #[test]
    fn default_silences_per_feature_info_but_keeps_app_info() {
        let directives = build_directives(None);
        assert!(directives.contains("ato_cli=info"));
        assert!(directives.contains("node-compat=warn"));
    }

    #[test]
    fn node_compat_token_promotes_only_that_target_to_info() {
        let directives = build_directives(Some("node-compat"));
        assert!(directives.contains("node-compat=info"));
        assert!(!directives.contains("node-compat=warn"));
    }

    #[test]
    fn all_token_promotes_app_to_debug_and_features_to_debug() {
        let directives = build_directives(Some("all"));
        assert!(directives.contains("ato_cli=debug"));
        assert!(directives.contains("node-compat=debug"));
    }

    #[test]
    fn unknown_token_is_ignored_without_dropping_known_directives() {
        let directives = build_directives(Some("node-compat,bogus"));
        assert!(directives.contains("node-compat=info"));
        assert!(directives.contains("ato_cli=info"));
    }

    #[test]
    fn empty_value_falls_back_to_default() {
        let directives = build_directives(Some(""));
        assert!(directives.contains("node-compat=warn"));
    }
}
