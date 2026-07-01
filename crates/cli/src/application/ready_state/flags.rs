//! Feature-flag gating for the Ready-State path (E6).
//!
//! Ready-State build/run is entirely behind `ATO_READY_STATE_ENABLED` (default
//! off) plus an optional `ATO_SNAPSHOT_BACKEND` selector. When the flag is off,
//! every Ready-State decision returns "not eligible" and the legacy cold path
//! runs unchanged — the backward-compat firewall.

const ENABLE_VAR: &str = "ATO_READY_STATE_ENABLED";
const BACKEND_VAR: &str = "ATO_SNAPSHOT_BACKEND";
const FOREGROUND_VAR: &str = "ATO_READY_STATE_FOREGROUND";
const UFFD_DIAGNOSTICS_VAR: &str = "ATO_READY_STATE_UFFD_DIAGNOSTICS";
const UFFD_PREVIEW_VAR: &str = "ATO_READY_STATE_UFFD_PREVIEW";
const UFFD_AUTO_PREVIEW_VAR: &str = "ATO_READY_STATE_UFFD_AUTO_PREVIEW";

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

/// U10 (#877): opt-in Ready-State `mem_backend` selection **diagnostics**.
/// `ATO_READY_STATE_UFFD_DIAGNOSTICS=1` makes `ato run` compute + record which
/// `mem_backend` a selector WOULD choose (and why), then restore via **File exactly
/// as before** — a pure observation with no behavior change. Off by default.
pub(crate) fn uffd_diagnostics_enabled() -> bool {
    std::env::var(UFFD_DIAGNOSTICS_VAR)
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .unwrap_or(false)
}

/// U11 (#878): opt-in **local UFFD preview**. `ATO_READY_STATE_UFFD_PREVIEW=1` makes
/// `ato run` restore a no-binding capsule via the UFFD local-CAS demand path (instead
/// of the eager File rehydrate) on a supported host — and **fail closed** on an
/// unsupported host. Off by default → the File path is unchanged.
pub(crate) fn uffd_preview_enabled() -> bool {
    std::env::var(UFFD_PREVIEW_VAR)
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .unwrap_or(false)
}

/// U15 (#882): opt-in **auto-selection preview**. `ATO_READY_STATE_UFFD_AUTO_PREVIEW=1`
/// lets the pure selector ([`snapshot::mem_backend_selector`]) CHOOSE File vs UFFD
/// from the real facts (no-binding only, local CAS, remote off), instead of the U11
/// forced-on preview. On an unsupported host it gracefully falls back to File. Off by
/// default → the File path is unchanged.
pub(crate) fn uffd_auto_preview_enabled() -> bool {
    std::env::var(UFFD_AUTO_PREVIEW_VAR)
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

    #[test]
    fn uffd_diagnostics_off_by_default_on_when_truthy() {
        // SAFETY: single-threaded test body; var restored at the end.
        let prev = std::env::var(UFFD_DIAGNOSTICS_VAR).ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(UFFD_DIAGNOSTICS_VAR, v),
                None => std::env::remove_var(UFFD_DIAGNOSTICS_VAR),
            }
        };
        set(None);
        assert!(!uffd_diagnostics_enabled(), "off by default");
        set(Some("1"));
        assert!(uffd_diagnostics_enabled());
        set(Some("0"));
        assert!(!uffd_diagnostics_enabled());
        set(prev.as_deref());
    }

    #[test]
    fn uffd_preview_off_by_default_on_when_truthy() {
        // SAFETY: single-threaded test body; var restored at the end.
        let prev = std::env::var(UFFD_PREVIEW_VAR).ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(UFFD_PREVIEW_VAR, v),
                None => std::env::remove_var(UFFD_PREVIEW_VAR),
            }
        };
        set(None);
        assert!(!uffd_preview_enabled(), "off by default");
        set(Some("1"));
        assert!(uffd_preview_enabled());
        set(Some("0"));
        assert!(!uffd_preview_enabled());
        set(prev.as_deref());
    }

    #[test]
    fn uffd_auto_preview_off_by_default_on_when_truthy() {
        // SAFETY: single-threaded test body; var restored at the end.
        let prev = std::env::var(UFFD_AUTO_PREVIEW_VAR).ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(UFFD_AUTO_PREVIEW_VAR, v),
                None => std::env::remove_var(UFFD_AUTO_PREVIEW_VAR),
            }
        };
        set(None);
        assert!(!uffd_auto_preview_enabled(), "off by default");
        set(Some("1"));
        assert!(uffd_auto_preview_enabled());
        set(Some("0"));
        assert!(!uffd_auto_preview_enabled());
        set(prev.as_deref());
    }
}
