//! `ato://` intent **verb vocabulary** — the typed result of classifying an
//! intercepted custom-scheme navigation emitted by an Ato-owned web surface.
//!
//! This is the shared vocabulary, not the classifier: *parsing* an `ato://` /
//! `capsule://` URL into these types and *gating* it on origin trust is
//! host-specific policy (the desktop shell's `intent` module owns the `url`
//! parsing, the trusted-origin allowlist, and the navigation rules; a future
//! Tauri shell will own its own). Both shells produce this same vocabulary so
//! their dispatchers — and any shared handling — speak one set of verbs rather
//! than each re-declaring them.
//!
//! The verbs map onto supervision actions (`Run`, runner register/start/stop);
//! per-verb dispatch and the confirmation model live in the shell, never here.
//!
//! The types are `serde`-serializable so a shell can carry a classified verb
//! across its `invoke` boundary (the Tauri shell) or an IPC hop; the
//! representation is the default externally-tagged form.

use serde::{Deserialize, Serialize};

/// A privileged intent: it touches local execution or the runner agent and so
/// requires a trusted origin (enforced by the classifying shell). Per-verb
/// handling lives in the shell's dispatcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrivilegedIntent {
    /// `ato://run?source=<capsule-ref>[&run_id=<id>]` — request to run a capsule
    /// on this device.
    Run {
        source: String,
        run_id: Option<String>,
        /// The trusted pane origin the intent was emitted from (already
        /// validated by the classifying shell). Carried through so the
        /// dispatcher can show it in the consent wizard — this verb is the only
        /// one whose dispatcher needs it today.
        origin: String,
    },
    /// `ato://runner/register` — register this device as a personal Connected
    /// Runner.
    RunnerRegister,
    /// `ato://runner/start` — start the local runner agent (`ato runner serve`).
    RunnerStart,
    /// `ato://runner/stop` — stop the local runner agent.
    RunnerStop,
}

/// Outcome of classifying an intercepted `ato://` / `capsule://` navigation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentDecision {
    /// A callback / deep-link verb that the shell's existing host-route drain
    /// already handles (auth callback, `ato://open`, `ato://cli`, `capsule://`
    /// deep link). The original URI is carried through unchanged. Only produced
    /// for trusted origins.
    HostRoute(String),
    /// A privileged action (local execution / runner control). Only produced for
    /// a trusted, on-origin pane; the shell dispatcher decides how to handle
    /// each verb.
    Privileged(PrivilegedIntent),
    /// Rejected — untrusted origin, unknown verb, or malformed payload. Carries
    /// a human-readable reason for telemetry/logging. Never acted on.
    Reject(String),
}

/// Parse an `ato://runner/{register,start,stop}` URI into its runner-control
/// verb. Returns `None` for any other scheme, namespace, or action.
///
/// This is the shared, **trust-free** verb parser: it decides *which* runner
/// verb a URI names, never *whether* the caller may act on it. Origin trust is
/// host policy applied by the caller (the GPUI shell gates on its https origin
/// allowlist; the Tauri shell trusts the bundled main window's local asset).
/// It reuses the canonical [`crate::handle::parse_host_route`] splitter, so no
/// second `ato://` parser can drift from it, and needs no `url` dependency.
///
/// The query-bearing `run` verb and the `auth`/`open`/`cli` host-route verbs are
/// intentionally out of scope here — they are wired on separate, shell-specific
/// paths (the consent-gated launch; the host-route drain).
pub fn parse_runner_control_intent(uri: &str) -> Option<PrivilegedIntent> {
    let route = crate::handle::parse_host_route(uri).ok()?;
    if route.namespace != "runner" {
        return None;
    }
    match route.path_segments.first().map(String::as_str) {
        Some("register") => Some(PrivilegedIntent::RunnerRegister),
        Some("start") => Some(PrivilegedIntent::RunnerStart),
        Some("stop") => Some(PrivilegedIntent::RunnerStop),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_runner_control_verbs() {
        assert_eq!(
            parse_runner_control_intent("ato://runner/register"),
            Some(PrivilegedIntent::RunnerRegister)
        );
        assert_eq!(
            parse_runner_control_intent("ato://runner/start"),
            Some(PrivilegedIntent::RunnerStart)
        );
        assert_eq!(
            parse_runner_control_intent("ato://runner/stop"),
            Some(PrivilegedIntent::RunnerStop)
        );
    }

    #[test]
    fn rejects_non_runner_and_malformed_uris() {
        // Other namespaces are not runner-control intents.
        assert_eq!(parse_runner_control_intent("ato://run?source=x"), None);
        assert_eq!(parse_runner_control_intent("ato://open?handle=x"), None);
        assert_eq!(parse_runner_control_intent("capsule://ato.run/a/b"), None);
        // Unknown runner action / missing action.
        assert_eq!(parse_runner_control_intent("ato://runner/restart"), None);
        assert_eq!(parse_runner_control_intent("ato://runner"), None);
        // Not an ato:// URI at all.
        assert_eq!(parse_runner_control_intent("https://app.ato.run"), None);
    }
}
