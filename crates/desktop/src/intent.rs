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

/// Outcome of classifying an intercepted `ato://` / `capsule://` navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentDecision {
    /// A callback / deep-link verb that the existing
    /// [`crate::state::AppState::handle_host_route`] drain already handles
    /// (auth callback, `ato://open`, `ato://cli`, `capsule://` deep link).
    /// The original URI is carried through unchanged. Only produced for trusted
    /// origins.
    HostRoute(String),
    /// A privileged action (local execution / runner control). Only produced
    /// for a trusted, on-origin pane; the dispatcher decides how to handle each
    /// verb (see `crate::webview::dispatch_privileged_intent`).
    Privileged(PrivilegedIntent),
    /// Rejected — untrusted origin, unknown verb, or malformed payload. Carries
    /// a human-readable reason for telemetry/logging. Never acted on.
    Reject(String),
}

/// A privileged intent: it touches local execution or the runner agent and so
/// requires a trusted origin (enforced here). Per-verb handling lives in the
/// dispatcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivilegedIntent {
    /// `ato://run?source=<capsule-ref>[&run_id=<id>]` — request to run a capsule
    /// on this device.
    Run {
        source: String,
        run_id: Option<String>,
    },
    /// `ato://runner/register` — register this device as a personal Connected
    /// Runner.
    RunnerRegister,
    /// `ato://runner/start` — start the local runner agent (`ato runner serve`).
    RunnerStart,
    /// `ato://runner/stop` — stop the local runner agent.
    RunnerStop,
}

/// Whether `host` is loopback (local dev). Only consulted in debug builds.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".localhost")
}

/// Whether `host` is a trusted Ato origin allowed to emit intents.
///
/// The set is a **fixed allowlist** — the production marketing site (`ato.run`,
/// for the dock + sign-in callbacks) and the production + staging PWA Home hosts.
/// Debug builds additionally trust loopback so a local `vite` dev server works.
/// An arbitrary `ATO_APP_BASE_URL` host is deliberately **not** trusted in
/// release builds: the Home may be pointed elsewhere, but only known Ato origins
/// may drive privileged local behaviour.
pub fn is_trusted_intent_origin(host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    if matches!(host, "ato.run" | "app.ato.run" | "stg-app.ato.run") {
        return true;
    }
    cfg!(debug_assertions) && is_loopback_host(host)
}

/// Whether navigating to `target_uri` leaves `pane_host` (a different-host
/// http(s) top-level navigation). Used to invalidate intent-trust for a Home
/// pane that has navigated off its trusted origin — so a page loaded *after*
/// such a navigation can no longer emit trusted intents. Non-http(s) targets
/// (e.g. the `ato://` intents themselves) never count as leaving.
pub fn is_cross_origin_navigation(pane_host: &str, target_uri: &str) -> bool {
    if !(target_uri.starts_with("http://") || target_uri.starts_with("https://")) {
        return false;
    }
    match url::Url::parse(target_uri) {
        Ok(target) => target.host_str().unwrap_or_default() != pane_host,
        // Unparseable http(s) → treat as leaving (fail safe).
        Err(_) => true,
    }
}

/// Classify an intercepted custom-scheme navigation emitted by the pane at
/// `origin_host`. Pure — all policy is expressed here so it is unit-testable
/// without GPUI / Wry.
pub fn classify(origin_host: &str, uri: &str) -> IntentDecision {
    let trusted = is_trusted_intent_origin(origin_host);

    // `capsule://<host>/<publisher>/<slug>` — open a capsule by deep link.
    if uri.starts_with("capsule://") {
        return if trusted {
            IntentDecision::HostRoute(uri.to_string())
        } else {
            IntentDecision::Reject(format!(
                "capsule deep link from untrusted origin '{origin_host}'"
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
                    "'{namespace}' intent from untrusted origin '{origin_host}'"
                ))
            }
        }
        "run" => {
            if !trusted {
                return IntentDecision::Reject(format!(
                    "run intent from untrusted origin '{origin_host}'"
                ));
            }
            let source = parsed
                .query_pairs()
                .find(|(k, _)| k == "source")
                .map(|(_, v)| v.into_owned())
                .filter(|v| !v.trim().is_empty());
            match source {
                Some(source) => IntentDecision::Privileged(PrivilegedIntent::Run {
                    source,
                    run_id: parsed
                        .query_pairs()
                        .find(|(k, _)| k == "run_id")
                        .map(|(_, v)| v.into_owned())
                        .filter(|v| !v.trim().is_empty()),
                }),
                None => IntentDecision::Reject("run intent missing 'source' parameter".to_string()),
            }
        }
        "runner" => {
            if !trusted {
                return IntentDecision::Reject(format!(
                    "runner intent from untrusted origin '{origin_host}'"
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

    const TRUSTED: &str = "app.ato.run";
    const UNTRUSTED: &str = "evil.example";

    #[test]
    fn trusted_origins_are_a_fixed_allowlist() {
        assert!(is_trusted_intent_origin("app.ato.run"));
        assert!(is_trusted_intent_origin("stg-app.ato.run"));
        assert!(is_trusted_intent_origin("ato.run"));
        assert!(!is_trusted_intent_origin("notapp.ato.run"));
        assert!(!is_trusted_intent_origin("app.evil.example"));
        assert!(!is_trusted_intent_origin(""));
        // An arbitrary host is never trusted — even if it were configured as the
        // Home base URL. Only the fixed allowlist (+ loopback in debug) qualifies.
        assert!(!is_trusted_intent_origin("custom.example"));
    }

    #[test]
    fn loopback_is_trusted_only_in_debug_builds() {
        // Tests run in debug builds, so loopback is trusted here; the production
        // guard is `cfg!(debug_assertions)` in `is_trusted_intent_origin`.
        assert_eq!(
            is_trusted_intent_origin("localhost"),
            cfg!(debug_assertions)
        );
        assert_eq!(
            is_trusted_intent_origin("127.0.0.1"),
            cfg!(debug_assertions)
        );
    }

    #[test]
    fn cross_origin_navigation_detected_by_host() {
        assert!(!is_cross_origin_navigation(
            "app.ato.run",
            "https://app.ato.run/#route=/runners"
        ));
        assert!(is_cross_origin_navigation(
            "app.ato.run",
            "https://evil.example/"
        ));
        // Non-http(s) (the intents themselves) never count as leaving.
        assert!(!is_cross_origin_navigation(
            "app.ato.run",
            "ato://runner/start"
        ));
        assert!(!is_cross_origin_navigation(
            "app.ato.run",
            "capsule://ato.run/a/b"
        ));
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
            })
        );
        assert_eq!(
            classify(TRUSTED, "ato://run?source=acme%2Fchat&run_id=run_123"),
            IntentDecision::Privileged(PrivilegedIntent::Run {
                source: "acme/chat".to_string(),
                run_id: Some("run_123".to_string()),
            })
        );
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
