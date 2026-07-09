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
const BINDINGS_PREVIEW_VAR: &str = "ATO_READY_STATE_BINDINGS_PREVIEW";

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

/// Phase 8a-RunGate (#912): opt-in **BindingLease run-gate preview**.
/// `ATO_READY_STATE_BINDINGS_PREVIEW=1` lets a binding-required Ready-State capsule
/// restore its secret-free snapshot, receive bindings over vsock, and expose traffic
/// **only after bound-ready** — instead of the #837 pre-restore fail-closed. Off by
/// default → binding-required capsules stay fail-closed exactly as today.
pub(crate) fn bindings_preview_enabled() -> bool {
    std::env::var(BINDINGS_PREVIEW_VAR)
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .unwrap_or(false)
}

/// v1.2 PR 3e: operator opt-in for SUPERVISOR (binding-required) snapshot restores
/// on this runner — symmetric with the builder's `ATO_BUILDER_SUPERVISOR`. Off by
/// default: the runner then neither advertises `restore_snapshot_with_bindings`
/// nor accepts a supervisor artifact (byte-identical v1 behavior).
pub(crate) fn runner_supervisor_enabled() -> bool {
    std::env::var("ATO_RUNNER_SUPERVISOR")
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .unwrap_or(false)
}

/// ato#1006 (UNIT C): operator opt-in for the Public Preview lane on this runner.
/// Off by default: the runner then neither advertises `restore_snapshot_preview`
/// nor accepts a preview lease (byte-identical behavior). Symmetric with
/// `ATO_RUNNER_SUPERVISOR` — a fixed KVM preview box sets `ATO_RUNNER_PREVIEW=1`.
pub(crate) fn runner_preview_enabled() -> bool {
    std::env::var("ATO_RUNNER_PREVIEW")
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .unwrap_or(false)
}

/// v1.2 PR 2 (L8): the binding-lease TTL, `ATO_READY_STATE_BINDING_TTL_MS`
/// (default 1h). The foreground serving loop renews well inside this window;
/// an un-renewed lease expiry-scrubs in the guest (lazy) and traffic gates.
/// Clamped to ≥ 10s so a typo cannot create an instantly-expiring lease; a
/// non-numeric value falls back to the default with a warning (never a crash
/// mid-run-gate).
pub(crate) fn binding_ttl_ms() -> u64 {
    const VAR: &str = "ATO_READY_STATE_BINDING_TTL_MS";
    const DEFAULT: u64 = 3_600_000;
    match std::env::var(VAR).ok().filter(|v| !v.trim().is_empty()) {
        None => DEFAULT,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) => ms.max(10_000),
            Err(_) => {
                tracing::warn!(target: "ato::ready_state", raw, "invalid {VAR}; using default");
                DEFAULT
            }
        },
    }
}

/// v1.2 PR 3e-4: total budget for the restore path's proxy readiness probe,
/// `ATO_PROXY_READY_TIMEOUT_MS` (default 5s). The supervisor guest restarts
/// its workload with the real env AFTER bind, so the first accept can lag
/// bring-up by 1-2s — a single probe is a guaranteed race. Clamped to ≥ 1s so
/// a typo cannot reintroduce that race; a non-numeric value falls back to the
/// default with a warning (never a crash mid-restore).
pub(crate) fn proxy_ready_timeout_ms() -> u64 {
    const VAR: &str = "ATO_PROXY_READY_TIMEOUT_MS";
    const DEFAULT: u64 = 5_000;
    match std::env::var(VAR).ok().filter(|v| !v.trim().is_empty()) {
        None => DEFAULT,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(ms) => ms.max(1_000),
            Err(_) => {
                tracing::warn!(target: "ato::ready_state", raw, "invalid {VAR}; using default");
                DEFAULT
            }
        },
    }
}

/// ato#1002 (UNIT C1): sanity cap for a fetched remote `r2://` artifact,
/// `ATO_ARTIFACT_FETCH_MAX_BYTES` (default 8 GiB). Applied to BOTH the downloaded
/// `artifact.tar.gz` byte count and the summed decompressed entry sizes during the
/// safe extraction — a decompression bomb trips it, never fills the disk. A zero or
/// non-numeric value falls back to the default with a warning (never a crash on the
/// restore path).
pub(crate) fn artifact_fetch_max_bytes() -> u64 {
    const VAR: &str = "ATO_ARTIFACT_FETCH_MAX_BYTES";
    const DEFAULT: u64 = 8 * 1024 * 1024 * 1024;
    match std::env::var(VAR).ok().filter(|v| !v.trim().is_empty()) {
        None => DEFAULT,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(target: "ato::ready_state", raw, "invalid {VAR}; using default");
                DEFAULT
            }
        },
    }
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

    #[test]
    fn artifact_fetch_max_bytes_defaults_and_overrides() {
        const VAR: &str = "ATO_ARTIFACT_FETCH_MAX_BYTES";
        // SAFETY: single-threaded test body; var restored at the end.
        let prev = std::env::var(VAR).ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(VAR, v),
                None => std::env::remove_var(VAR),
            }
        };
        set(None);
        assert_eq!(
            artifact_fetch_max_bytes(),
            8 * 1024 * 1024 * 1024,
            "default 8 GiB"
        );
        set(Some("1048576"));
        assert_eq!(artifact_fetch_max_bytes(), 1_048_576);
        set(Some("0"));
        assert_eq!(
            artifact_fetch_max_bytes(),
            8 * 1024 * 1024 * 1024,
            "zero falls back"
        );
        set(Some("not-a-number"));
        assert_eq!(
            artifact_fetch_max_bytes(),
            8 * 1024 * 1024 * 1024,
            "junk falls back"
        );
        set(prev.as_deref());
    }

    #[test]
    fn runner_preview_off_by_default_on_when_truthy() {
        // SAFETY: single-threaded test body; var restored at the end.
        let prev = std::env::var("ATO_RUNNER_PREVIEW").ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var("ATO_RUNNER_PREVIEW", v),
                None => std::env::remove_var("ATO_RUNNER_PREVIEW"),
            }
        };
        set(None);
        assert!(!runner_preview_enabled(), "off by default");
        set(Some("1"));
        assert!(runner_preview_enabled());
        set(Some("0"));
        assert!(!runner_preview_enabled());
        set(prev.as_deref());
    }

    #[test]
    fn bindings_preview_off_by_default_on_when_truthy() {
        // SAFETY: single-threaded test body; var restored at the end.
        let prev = std::env::var(BINDINGS_PREVIEW_VAR).ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(BINDINGS_PREVIEW_VAR, v),
                None => std::env::remove_var(BINDINGS_PREVIEW_VAR),
            }
        };
        set(None);
        assert!(!bindings_preview_enabled(), "off by default");
        set(Some("1"));
        assert!(bindings_preview_enabled());
        set(Some("0"));
        assert!(!bindings_preview_enabled());
        set(prev.as_deref());
    }
}
