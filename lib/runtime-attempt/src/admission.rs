//! The hard gates a candidate must pass before an attempt is spent on it —
//! one set, used by every entry that executes a Derivation.
//!
//! Capability match is not success: it only decides whether executing is
//! worth anything. Everything here is a fact about the Runtime, the
//! Derivation or the request's authorization, never a guess about the
//! outcome, and every refusal happens before anything of the candidate runs.

use ato_formation::authoring::EffectClass;
use ato_formation::request::{AttemptFailure, RuntimeProfile};

use crate::browser_verify::BrowserVerification;
use crate::build_sandbox::{NetworkPolicy, TOOLCHAIN_ROOT};
use crate::plan::PlannedCandidate;

/// Effect classes a Runtime executes without a confirmed authorization: an
/// attempt of one of these can fail and be retried without anything leaking
/// out of it.
pub fn is_disposable(effects: EffectClass) -> bool {
    matches!(
        effects,
        EffectClass::Pure | EffectClass::Idempotent | EffectClass::RecordSubstitutable
    )
}

pub fn effects_name(effects: EffectClass) -> String {
    serde_json::to_value(effects)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{effects:?}"))
}

fn refused(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: "admission".to_owned(),
        message: message.into(),
    })
}

/// `None` when the candidate may be attempted here.
pub fn admit(
    profile: &RuntimeProfile,
    planned: &PlannedCandidate,
    network: NetworkPolicy,
    browser: Option<&BrowserVerification>,
) -> Option<AttemptFailure> {
    // A declared effect is a request, not an authorization. No entry carries
    // a confirmed authorization yet, so a route that may leave an effect
    // behind is not executed by any of them — local included.
    if !is_disposable(planned.derivation.effects) {
        return refused(
            "effect_policy",
            format!(
                "the route's effect class is {}; this request carries no confirmed \
                 authorization for it, so only pure, idempotent or record-substitutable \
                 routes are executed",
                effects_name(planned.derivation.effects)
            ),
        );
    }
    // The route's own platform statement is part of D, so it binds every
    // Runtime that is asked to run it — whatever a scheduler believed.
    let platforms = &planned.derivation.platforms;
    if !platforms.is_empty()
        && !platforms.iter().any(|platform| {
            platform.os == std::env::consts::OS && platform.arch == std::env::consts::ARCH
        })
    {
        return refused(
            "platform_unsupported",
            format!(
                "this route runs on {}; this Runtime is {}/{}; it was not attempted",
                platforms
                    .iter()
                    .map(|platform| format!("{}/{}", platform.os, platform.arch))
                    .collect::<Vec<_>>()
                    .join(", "),
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        );
    }
    if browser.is_some() && !planned.intent.lane.is_process() {
        // Browser verification has not moved onto the common observation
        // path for every lane; a request it cannot decide is refused rather
        // than decided some other way.
        return refused(
            "browser_contract_needs_realization",
            "a browser Contract is verified against a running process candidate; this \
             candidate's lane is not browser-verified on this Runtime",
        );
    }
    if let Some(browser) = browser {
        match &browser.verifier {
            None => {
                return refused(
                    "browser_verifier_unavailable",
                    "the request carries a browser Contract and this Runtime has no browser \
                     verifier; it was not attempted",
                );
            }
            // Never verified outside the verifier sandbox unless a developer
            // chose an uncontained verifier explicitly.
            Some(verifier) if !verifier.usable() => {
                return refused(
                    "browser_verifier_containment_unavailable",
                    "the request carries a browser Contract and this Runtime cannot run its \
                     browser verifier contained (bubblewrap); it was not attempted",
                );
            }
            Some(_) => {}
        }
    }
    if !planned.plan.steps.is_empty()
        && profile.get("formation.containment") != Some("bwrap+landlock")
    {
        return refused(
            "runtime_cannot_contain_build",
            "this candidate needs build steps and this Runtime cannot contain one (no bwrap); \
             it was not attempted",
        );
    }
    if !planned.plan.steps.is_empty() && profile.get("formation.toolchain_root").is_none() {
        return refused(
            "runtime_has_no_toolchain_root",
            format!(
                "this candidate's build provisions toolchains into {TOOLCHAIN_ROOT}, which this \
                 Runtime does not have; it was not attempted"
            ),
        );
    }
    if planned.intent.lane.is_process()
        && profile.get("formation.containment") != Some("bwrap+landlock")
    {
        return refused(
            "runtime_cannot_contain_candidate",
            "verifying this candidate means running it, and this Runtime cannot contain a \
             process (no bwrap); it was not attempted",
        );
    }
    if planned.plan.steps.iter().any(|step| step.needs_network) && network == NetworkPolicy::Denied
    {
        return refused(
            "network_denied",
            "this candidate's build resolves dependencies from the network and the request \
             denies it; it was not attempted",
        );
    }
    None
}
