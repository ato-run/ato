//! Logging filter glue for `ato-desktop`.
//!
//! Two env vars drive the active filter, in this precedence order:
//!
//! 1. **`RUST_LOG`** — raw `tracing-subscriber` directives. When set,
//!    everything below is ignored. Reach for this when you need
//!    per-module fine control.
//! 2. **`ATO_DESKTOP_LOG`** — comma-separated feature names with a
//!    friendlier vocabulary than the directive grammar:
//!    - `favicon` — surface INFO output from icon / favicon plumbing
//!      (fetch dispatch, HTML link parsing, ICO/SVG normalization).
//!    - `all` — turn on every per-feature target plus app DEBUG.
//!    Unknown tokens are warned about on stderr and otherwise ignored.
//! 3. **Default** — `ato_desktop=info` baseline plus `<feature>=warn`
//!    for every per-feature target. Errors from the gated targets
//!    still surface; routine INFO chatter stays silent until opted in.

use tracing_subscriber::EnvFilter;

/// `target:` value for icon / favicon plumbing — fetch dispatch, HTML
/// link parsing, ICO/SVG normalization, and the share-icon resolver.
pub const TARGET_FAVICON: &str = "favicon";

/// Targets that `ATO_DESKTOP_LOG=<name>` knows by name. Adding a new
/// per-feature flag is one line here plus tagging the relevant
/// `tracing::*!` calls with `target: <YOUR_CONST>`.
const FEATURE_TARGETS: &[&str] = &[TARGET_FAVICON];

pub fn build_env_filter() -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    EnvFilter::new(build_directives(std::env::var("ATO_DESKTOP_LOG").ok().as_deref()))
}

fn build_directives(ato_desktop_log: Option<&str>) -> String {
    let baseline_level = "info";
    let feature_default = "warn";
    let feature_enabled = "info";

    let mut directives: Vec<String> = std::iter::once(format!("ato_desktop={baseline_level}"))
        .chain(FEATURE_TARGETS.iter().map(|t| format!("{t}={feature_default}")))
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
    fn default_silences_favicon_info_but_keeps_app_info() {
        let directives = build_directives(None);
        assert!(directives.contains("ato_desktop=info"));
        assert!(directives.contains("favicon=warn"));
    }

    #[test]
    fn favicon_token_promotes_only_favicon_to_info() {
        let directives = build_directives(Some("favicon"));
        assert!(directives.contains("ato_desktop=info"));
        assert!(directives.contains("favicon=info"));
        assert!(!directives.contains("favicon=warn"));
    }

    #[test]
    fn all_token_promotes_app_to_debug_and_features_to_debug() {
        let directives = build_directives(Some("all"));
        assert!(directives.contains("ato_desktop=debug"));
        assert!(directives.contains("favicon=debug"));
        assert!(!directives.contains("ato_desktop=info"));
    }

    #[test]
    fn comma_separated_tokens_compose() {
        // Currently only `favicon` exists, but the parser must still
        // tolerate trailing/leading whitespace and ignore unknown
        // tokens without dropping known ones.
        let directives = build_directives(Some(" favicon , bogus "));
        assert!(directives.contains("favicon=info"));
        assert!(directives.contains("ato_desktop=info"));
    }

    #[test]
    fn empty_value_falls_back_to_default() {
        let directives = build_directives(Some(""));
        assert!(directives.contains("favicon=warn"));
    }
}
