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
