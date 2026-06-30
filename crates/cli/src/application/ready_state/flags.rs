//! Feature-flag gating for the Ready-State path (E6).
//!
//! Ready-State build/run is entirely behind `ATO_READY_STATE_ENABLED` (default
//! off) plus an optional `ATO_SNAPSHOT_BACKEND` selector. When the flag is off,
//! every Ready-State decision returns "not eligible" and the legacy cold path
//! runs unchanged — the backward-compat firewall.

const ENABLE_VAR: &str = "ATO_READY_STATE_ENABLED";
const BACKEND_VAR: &str = "ATO_SNAPSHOT_BACKEND";
const FOREGROUND_VAR: &str = "ATO_READY_STATE_FOREGROUND";

/// Parse a bool env value with the same accept-set as the capsule crate's
/// `parse_bool_env` (`1/true/yes/on` → true, `0/false/no/off`/empty → false).
/// Unknown tokens are treated as `None` (caller decides; we default off).
pub(crate) fn parse_bool_env(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

/// Whether the Ready-State path is enabled. Off unless the env var is explicitly
/// truthy.
pub(crate) fn ready_state_enabled() -> bool {
    std::env::var(ENABLE_VAR)
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .unwrap_or(false)
}

/// Opt-in foreground serving (Phase 7.5b): `ATO_READY_STATE_FOREGROUND=1` makes
/// `ato run` BLOCK while the Ready-State microVM serves and tear it down on
/// Ctrl-C / guest exit, instead of the default #845 background register-and-return
/// (where `ato stop` reaps it later). Off by default → behavior unchanged.
pub(crate) fn foreground_serve_enabled() -> bool {
    std::env::var(FOREGROUND_VAR)
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .unwrap_or(false)
}

/// An explicitly selected snapshot backend id (`ATO_SNAPSHOT_BACKEND`), if any.
pub(crate) fn selected_backend_id() -> Option<String> {
    std::env::var(BACKEND_VAR)
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "none")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bool_env_matches_capsule_accept_set() {
        for t in ["1", "true", "yes", "on", "TRUE", " On "] {
            assert_eq!(parse_bool_env(t), Some(true), "{t:?}");
        }
        for f in ["0", "false", "no", "off", "", "  "] {
            assert_eq!(parse_bool_env(f), Some(false), "{f:?}");
        }
        assert_eq!(parse_bool_env("maybe"), None);
    }

    #[test]
    fn foreground_serve_off_by_default_on_when_truthy() {
        // SAFETY: single-threaded test body; var restored at the end.
        let prev = std::env::var(FOREGROUND_VAR).ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(FOREGROUND_VAR, v),
                None => std::env::remove_var(FOREGROUND_VAR),
            }
        };
        set(None);
        assert!(!foreground_serve_enabled(), "off by default");
        set(Some("1"));
        assert!(foreground_serve_enabled());
        set(Some("0"));
        assert!(!foreground_serve_enabled());
        set(prev.as_deref());
    }
}
