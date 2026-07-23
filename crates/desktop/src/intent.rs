//! `ato://` intent bridge — classification + origin policy (the P0 trust
//! boundary for the embedded PWA Home).
//!
//! Ato-owned web surfaces (the embedded `ato-pwa` Home, the in-product dock,
//! the sign-in page) can only *emit* intents by navigating to an `ato://` /
//! `capsule://` URL; the desktop decides what — if anything — to do. This module
//! is the pure validation layer the WebView navigation handler calls before
//! acting on any intercepted custom-scheme navigation. It answers two
//! questions:
//!
//!   1. **Origin** — did the intent come from a trusted Ato origin? An arbitrary
//!      external site loaded in a WebView pane must NOT be able to drive local
//!      execution, so every verb is gated on [`is_trusted_intent_origin`].
//!   2. **Verb + schema** — is this a known verb with a well-formed payload?
//!      Unknown verbs and malformed payloads are rejected (and logged), never
//!      forwarded.
//!
//! Privileged verbs (run, runner control) are accepted only from a trusted,
//! on-origin pane; their per-verb handling (and confirmation model) lives in the
//! dispatcher (`crate::webview::dispatch_privileged_intent`), not here. This
//! module's job is to classify and gate; it never executes.
//!
//! PR-D1 note: `crate::webview::WebViewManager` (the pane type this module was
//! written for) has no live construction site in the current Focus-mode
//! Desktop. The embedded Home now runs through
//! `window::web_app_view::WebAppView` + `app.rs`'s `NavigateToUrl` action
//! instead, which does not carry a per-navigation pane origin.
//! `parse_run_query` below is factored out so both the origin-gated path here
//! and that origin-agnostic live path share one parser. The live `ato://run`
//! consent gate itself lives in `system_capsule::ato_start::dispatch_run_intent`
//! and `window::launch_window::open_consent_window_for_run_agent`, not in this
//! module's `classify` / `dispatch_privileged_intent` pair.

// The `ato://` intent verb vocabulary (IntentDecision / PrivilegedIntent) is
// single-sourced in protocol::intent so both this GPUI shell and the Tauri
// shell speak one set of verbs. Re-exported so every `crate::intent::…`
// reference path is unchanged. The url-based classifier, the trusted-origin
// allowlist, and the navigation rules below stay here — they are shell policy
// (Phase 1 Step 8: intent verb split).
pub use protocol::intent::{IntentDecision, PrivilegedIntent};

/// Whether `host` is loopback (local dev). Only consulted in debug builds.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
        || host.ends_with(".localhost")
}

/// Canonical web origin (`scheme://host[:port]`) of `url_str`, or `None` for a
/// non-http(s) / opaque-origin URL. This is the unit the trust boundary operates
/// on — a web origin is scheme + host + port, never host alone.
pub fn web_origin(url_str: &str) -> Option<String> {
    let parsed = url::Url::parse(url_str).ok()?;
    let origin = parsed.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

/// Whether `origin` is an `http` loopback origin (localhost / 127.0.0.1 / ::1,
/// any port).
fn is_loopback_http_origin(origin: &str) -> bool {
    match url::Url::parse(origin) {
        Ok(url) => url.scheme() == "http" && url.host_str().is_some_and(is_loopback_host),
        Err(_) => false,
    }
}

/// Whether `origin` (a canonical `scheme://host[:port]`) is a trusted Ato origin
/// allowed to emit intents.
///
/// A **fixed allowlist of full web origins** — the production marketing site
/// (`https://ato.run`, for the dock + sign-in callbacks) and the production +
/// staging PWA Home origins, all over `https`. Debug builds additionally trust
/// `http` loopback (any port) so a local `vite` dev server works. A downgraded
/// scheme (`http://app.ato.run`), a non-default port, or an arbitrary
/// `ATO_APP_BASE_URL` host are all untrusted in release builds: only known Ato
/// origins may drive privileged local behaviour.
pub fn is_trusted_intent_origin(origin: &str) -> bool {
    if matches!(
        origin,
        "https://ato.run" | "https://app.ato.run" | "https://stg-app.ato.run"
    ) {
        return true;
    }
    cfg!(debug_assertions) && is_loopback_http_origin(origin)
}

/// Whether navigating to `target_uri` leaves `pane_origin` (a canonical
/// `scheme://host[:port]`). Compares **full web origins** (scheme + host +
/// port), so `https → http`, a different host, or a different port all count as
/// leaving. Non-http(s) targets (e.g. the `ato://` intents themselves) never
/// count. Used to invalidate intent-trust for a Home pane that has navigated off
/// its trusted origin (defense-in-depth behind the navigation block).
pub fn is_cross_origin_navigation(pane_origin: &str, target_uri: &str) -> bool {
    if !(target_uri.starts_with("http://") || target_uri.starts_with("https://")) {
        return false;
    }
    match web_origin(target_uri) {
        Some(target_origin) => target_origin != pane_origin,
        // Opaque / unparseable http(s) → treat as leaving (fail safe).
        None => true,
    }
}

/// How a top-level http(s) navigation in a WebView pane should be handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopLevelNavOutcome {
    /// Block the navigation inside the WebView (the caller hands it to the
    /// system browser instead).
    pub block: bool,
    /// New value for the pane's `navigated_off_origin` defense-in-depth flag.
    pub navigated_off: bool,
}

/// Decide how to treat a top-level http(s) navigation to `target_uri` from a
/// pane whose canonical origin is `pane_origin`.
///
/// - `is_home_pane`: the pane is the embedded PWA Home (pinned to its origin).
/// - `auth_flow`: a sign-in pane (exempt from pinning — OAuth round-trips).
///
/// The Home pane blocks cross-origin top-level navigation (→ system browser).
/// Crucially, a **blocked** navigation does NOT downgrade trust: the WebView
/// never leaves the Home origin, so a single external-link click must not poison
/// later `ato://` intents from the still-loaded Home page. Only navigations that
/// are actually allowed to proceed record the off-origin state.
pub fn classify_top_level_navigation(
    pane_origin: &str,
    target_uri: &str,
    is_home_pane: bool,
    auth_flow: bool,
) -> TopLevelNavOutcome {
    let cross = is_cross_origin_navigation(pane_origin, target_uri);
    if cross && is_home_pane && !auth_flow {
        // Blocked → handed to the system browser; the WebView stays on-origin.
        TopLevelNavOutcome {
            block: true,
            navigated_off: false,
        }
    } else {
        // Allowed to continue → record whether it left the origin.
        TopLevelNavOutcome {
            block: false,
            navigated_off: cross,
        }
    }
}

/// Parse the `source` (required) and `run_id` (optional) query parameters off
/// an `ato://run?source=<capsule-ref>[&run_id=<id>]` URI. Returns `None` when
/// `source` is absent or blank so no caller ever proceeds without an explicit
/// launch target.
///
/// Shared by [`classify`] (the origin-gated embedded-pane path) and the
/// live Focus-mode `NavigateToUrl` router in `app.rs` (PR-D1): `WebAppView`'s
/// navigation intercept does not carry a per-navigation pane origin the way
/// the legacy `WebViewManager` pane did, so that router calls this parser
/// directly rather than going through the origin check here. Both callers
/// must agree on the query-string shape, so it is extracted once instead of
/// duplicated.
pub fn parse_run_query(uri: &str) -> Option<(String, Option<String>)> {
    let parsed = url::Url::parse(uri).ok()?;
    let source = parsed
        .query_pairs()
        .find(|(k, _)| k == "source")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.trim().is_empty())?;
    let run_id = parsed
        .query_pairs()
        .find(|(k, _)| k == "run_id")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.trim().is_empty());
    Some((source, run_id))
}

/// Classify an intercepted custom-scheme navigation emitted by the pane at
/// `origin`. Pure — all policy is expressed here so it is unit-testable
/// without GPUI / Wry.
pub fn classify(origin: &str, uri: &str) -> IntentDecision {
    let trusted = is_trusted_intent_origin(origin);

    // `capsule://<host>/<publisher>/<slug>` — open a capsule by deep link.
    if uri.starts_with("capsule://") {
        return if trusted {
            IntentDecision::HostRoute(uri.to_string())
        } else {
            IntentDecision::Reject(format!(
                "capsule deep link from untrusted origin '{origin}'"
            ))
        };
    }

    if !uri.starts_with("ato://") {
        return IntentDecision::Reject(format!("unsupported scheme: {uri}"));
    }

    let parsed = match url::Url::parse(uri) {
        Ok(parsed) => parsed,
        Err(err) => return IntentDecision::Reject(format!("unparseable intent '{uri}': {err}")),
    };

    // `ato://<namespace>/...` — the namespace is the URL host component.
    let namespace = parsed.host_str().unwrap_or_default();
    match namespace {
        // Verbs the existing host-route drain already handles. Forward verbatim,
        // but only from a trusted origin.
        "auth" | "open" | "cli" => {
            if trusted {
                IntentDecision::HostRoute(uri.to_string())
            } else {
                IntentDecision::Reject(format!(
                    "'{namespace}' intent from untrusted origin '{origin}'"
                ))
            }
        }
        "run" => {
            if !trusted {
                return IntentDecision::Reject(format!(
                    "run intent from untrusted origin '{origin}'"
                ));
            }
            match parse_run_query(uri) {
                Some((source, run_id)) => IntentDecision::Privileged(PrivilegedIntent::Run {
                    source,
                    run_id,
                    origin: origin.to_string(),
                }),
                None => IntentDecision::Reject("run intent missing 'source' parameter".to_string()),
            }
        }
        "runner" => {
            if !trusted {
                return IntentDecision::Reject(format!(
                    "runner intent from untrusted origin '{origin}'"
                ));
            }
            let action = parsed
                .path_segments()
                .and_then(|mut segments| segments.next())
                .unwrap_or_default();
            match action {
                "register" => IntentDecision::Privileged(PrivilegedIntent::RunnerRegister),
                "start" => IntentDecision::Privileged(PrivilegedIntent::RunnerStart),
                "stop" => IntentDecision::Privileged(PrivilegedIntent::RunnerStop),
                other => IntentDecision::Reject(format!("unknown runner action: '{other}'")),
            }
        }
        other => IntentDecision::Reject(format!("unknown intent namespace: '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUSTED: &str = "https://app.ato.run";
    const UNTRUSTED: &str = "https://evil.example";

    #[test]
    fn trusted_origins_are_a_fixed_full_origin_allowlist() {
        assert!(is_trusted_intent_origin("https://app.ato.run"));
        assert!(is_trusted_intent_origin("https://stg-app.ato.run"));
        assert!(is_trusted_intent_origin("https://ato.run"));
        // A web origin is scheme + host + port: a downgraded scheme or a
        // non-default port is a DIFFERENT origin and is not trusted.
        assert!(!is_trusted_intent_origin("http://app.ato.run"));
        assert!(!is_trusted_intent_origin("https://app.ato.run:8443"));
        // Look-alike / arbitrary origins are never trusted.
        assert!(!is_trusted_intent_origin("https://notapp.ato.run"));
        assert!(!is_trusted_intent_origin("https://app.evil.example"));
        assert!(!is_trusted_intent_origin("https://custom.example"));
        assert!(!is_trusted_intent_origin(""));
    }

    #[test]
    fn loopback_is_trusted_only_in_debug_builds() {
        // Tests run in debug builds, so http loopback is trusted here; the
        // production guard is `cfg!(debug_assertions)`.
        assert_eq!(
            is_trusted_intent_origin("http://localhost:5173"),
            cfg!(debug_assertions)
        );
        assert_eq!(
            is_trusted_intent_origin("http://127.0.0.1:9999"),
            cfg!(debug_assertions)
        );
        // https loopback / non-loopback http are not in the debug exception.
        assert!(!is_trusted_intent_origin("https://localhost:5173"));
    }

    #[test]
    fn cross_origin_navigation_compares_full_origin() {
        assert!(!is_cross_origin_navigation(
            "https://app.ato.run",
            "https://app.ato.run/#route=/runners"
        ));
        // scheme, host, or port differing all count as leaving.
        assert!(is_cross_origin_navigation(
            "https://app.ato.run",
            "http://app.ato.run/"
        ));
        assert!(is_cross_origin_navigation(
            "https://app.ato.run",
            "https://app.ato.run:8443/"
        ));
        assert!(is_cross_origin_navigation(
            "https://app.ato.run",
            "https://evil.example/"
        ));
        // Non-http(s) (the intents themselves) never count as leaving.
        assert!(!is_cross_origin_navigation(
            "https://app.ato.run",
            "ato://runner/start"
        ));
        assert!(!is_cross_origin_navigation(
            "https://app.ato.run",
            "capsule://ato.run/a/b"
        ));
    }

    #[test]
    fn home_blocked_cross_origin_navigation_does_not_poison_intent_trust() {
        // Home pane clicks an external link: blocked + handed to the system
        // browser, and trust is NOT downgraded (the WebView stays on-origin),
        // so a subsequent ato:// intent from the still-loaded Home page is
        // still accepted.
        let out = classify_top_level_navigation(
            "https://app.ato.run",
            "https://evil.example/",
            true,
            false,
        );
        assert!(out.block);
        assert!(!out.navigated_off);

        // Same-origin navigation: not blocked, not off-origin.
        let out = classify_top_level_navigation(
            "https://app.ato.run",
            "https://app.ato.run/#route=/runners",
            true,
            false,
        );
        assert!(!out.block);
        assert!(!out.navigated_off);

        // A non-Home pane (e.g. the dock) is not pinned: cross-origin is allowed
        // to proceed and DOES record off-origin (defense-in-depth).
        let out = classify_top_level_navigation(
            "https://ato.run",
            "https://evil.example/",
            false,
            false,
        );
        assert!(!out.block);
        assert!(out.navigated_off);

        // Sign-in pane OAuth round-trip: allowed, records off-origin.
        let out = classify_top_level_navigation(
            "https://app.ato.run",
            "https://accounts.google.com/",
            true,
            true,
        );
        assert!(!out.block);
        assert!(out.navigated_off);
    }

    #[test]
    fn untrusted_origin_is_rejected_for_every_verb() {
        for uri in [
            "ato://run?source=community/hello",
            "ato://runner/start",
            "ato://open?handle=capsule%3A%2F%2Fato.run%2Fa%2Fb",
            "ato://cli",
            "ato://auth/callback/dock",
            "capsule://ato.run/a/b",
        ] {
            assert!(
                matches!(classify(UNTRUSTED, uri), IntentDecision::Reject(_)),
                "expected reject for {uri}"
            );
        }
    }

    #[test]
    fn host_route_verbs_pass_through_from_trusted_origin() {
        assert_eq!(
            classify(TRUSTED, "ato://auth/callback/dock"),
            IntentDecision::HostRoute("ato://auth/callback/dock".to_string())
        );
        assert_eq!(
            classify(TRUSTED, "ato://open?handle=x"),
            IntentDecision::HostRoute("ato://open?handle=x".to_string())
        );
        assert_eq!(
            classify(TRUSTED, "capsule://ato.run/a/b"),
            IntentDecision::HostRoute("capsule://ato.run/a/b".to_string())
        );
    }

    #[test]
    fn run_intent_parses_source_and_optional_run_id() {
        assert_eq!(
            classify(TRUSTED, "ato://run?source=community%2Fhello-capsule"),
            IntentDecision::Privileged(PrivilegedIntent::Run {
                source: "community/hello-capsule".to_string(),
                run_id: None,
                origin: TRUSTED.to_string(),
            })
        );
        assert_eq!(
            classify(TRUSTED, "ato://run?source=acme%2Fchat&run_id=run_123"),
            IntentDecision::Privileged(PrivilegedIntent::Run {
                source: "acme/chat".to_string(),
                run_id: Some("run_123".to_string()),
                origin: TRUSTED.to_string(),
            })
        );
    }

    #[test]
    fn run_intent_captures_the_requesting_origin() {
        // PR-D1: the dispatcher needs the requesting origin to show in the
        // consent wizard, so `classify` must echo the (already-validated)
        // pane origin back on the `Run` variant rather than discarding it.
        const STAGING: &str = "https://stg-app.ato.run";
        assert_eq!(
            classify(STAGING, "ato://run?source=acme%2Fchat"),
            IntentDecision::Privileged(PrivilegedIntent::Run {
                source: "acme/chat".to_string(),
                run_id: None,
                origin: STAGING.to_string(),
            })
        );
    }

    #[test]
    fn parse_run_query_extracts_source_and_run_id() {
        assert_eq!(
            parse_run_query("ato://run?source=community%2Fhello-capsule"),
            Some(("community/hello-capsule".to_string(), None))
        );
        assert_eq!(
            parse_run_query("ato://run?source=acme%2Fchat&run_id=run_123"),
            Some(("acme/chat".to_string(), Some("run_123".to_string())))
        );
    }

    #[test]
    fn parse_run_query_rejects_missing_or_blank_source() {
        assert_eq!(parse_run_query("ato://run"), None);
        assert_eq!(parse_run_query("ato://run?source="), None);
        assert_eq!(parse_run_query("ato://run?run_id=run_1"), None);
        assert_eq!(parse_run_query("not a url"), None);
    }

    #[test]
    fn run_intent_without_source_is_rejected() {
        assert!(matches!(
            classify(TRUSTED, "ato://run"),
            IntentDecision::Reject(_)
        ));
        assert!(matches!(
            classify(TRUSTED, "ato://run?source="),
            IntentDecision::Reject(_)
        ));
    }

    #[test]
    fn runner_actions_classify() {
        assert_eq!(
            classify(TRUSTED, "ato://runner/register"),
            IntentDecision::Privileged(PrivilegedIntent::RunnerRegister)
        );
        assert_eq!(
            classify(TRUSTED, "ato://runner/start"),
            IntentDecision::Privileged(PrivilegedIntent::RunnerStart)
        );
        assert_eq!(
            classify(TRUSTED, "ato://runner/stop"),
            IntentDecision::Privileged(PrivilegedIntent::RunnerStop)
        );
        assert!(matches!(
            classify(TRUSTED, "ato://runner/explode"),
            IntentDecision::Reject(_)
        ));
        assert!(matches!(
            classify(TRUSTED, "ato://runner"),
            IntentDecision::Reject(_)
        ));
    }

    #[test]
    fn unknown_namespace_is_rejected() {
        assert!(matches!(
            classify(TRUSTED, "ato://wipe-disk"),
            IntentDecision::Reject(_)
        ));
    }
}
