//! The gates every attempt passes before anything of it runs, whoever
//! realizes the candidate: the effect authorization, the platform, and a
//! browser Contract's verifier. What a particular executor can contain
//! (bwrap, toolchains, a build network) is that executor's
//! [`crate::realize::CandidateRealizer::admit`].
//!
//! Capability match is not success: it only decides whether executing is
//! worth anything. Everything here is a fact about the Runtime, the
//! Derivation or the request's authorization, never a guess about the
//! outcome, and every refusal happens before anything of the candidate runs.

use ato_formation::authoring::EffectClass;
use ato_formation::request::AttemptFailure;

use crate::browser_verify::BrowserVerification;
use crate::spec::{AttemptSpec, CandidateShape};

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

/// Who authorized this attempt's effects, stated by the caller — never
/// inferred, and never a flag that widens what may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectAuthorization<'a> {
    /// Nobody is present: a Formation attempt, a Runtime Network ticket, a
    /// hosted job. Only disposable effect classes run.
    Unattended,
    /// A person started a Run of exactly this Derivation in the foreground
    /// (`ato run <capsule>`). It authorizes running that D; it is not a
    /// confirmation of effects the D declares beyond the disposable ones,
    /// which still need a confirmation this request does not carry.
    UserInvoked { derivation_ref: &'a str },
}

impl EffectAuthorization<'_> {
    /// How the start record names it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Unattended => "unattended",
            Self::UserInvoked { .. } => "user_invoked",
        }
    }
}

fn refused(code: &str, message: impl Into<String>) -> Option<AttemptFailure> {
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: "admission".to_owned(),
        message: message.into(),
    })
}

/// `None` when the attempt may proceed to the executor's own admission.
pub fn admit(
    spec: &AttemptSpec<'_>,
    authorization: EffectAuthorization<'_>,
    browser: Option<&BrowserVerification>,
) -> Option<AttemptFailure> {
    if let EffectAuthorization::UserInvoked { derivation_ref } = authorization
        && derivation_ref != spec.derivation_ref
    {
        return refused(
            "authorization_mismatch",
            format!(
                "the Run was started for {derivation_ref}; this attempt is {}",
                spec.derivation_ref
            ),
        );
    }
    // A declared effect is a request, not an authorization. Neither an
    // unattended attempt nor a started Run confirms an effect that may leave
    // something behind, so such a route is not executed by any entry.
    let effects = spec.derivation.effects;
    if !is_disposable(effects) {
        return refused(
            "effect_policy",
            format!(
                "the route's effect class is {}; this request carries no confirmed \
                 authorization for it ({}), so only pure, idempotent or record-substitutable \
                 routes are executed",
                effects_name(effects),
                authorization.name()
            ),
        );
    }
    // The route's own platform statement is part of D, so it binds every
    // Runtime that is asked to run it — whatever a scheduler believed.
    let platforms = &spec.derivation.platforms;
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
    if browser.is_some() && spec.shape != CandidateShape::Process {
        // Browser verification has not moved onto the common observation
        // path for every shape; a request it cannot decide is refused rather
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
    None
}

#[cfg(test)]
mod tests {
    use ato_formation::authoring::{BoundContract, BoundDerivation};

    use super::*;

    fn derivation(effects: EffectClass) -> BoundDerivation {
        let mut derivation: BoundDerivation = serde_json::from_value(serde_json::json!({
            "schema": "ato.derivation/1",
            "inputs": [], "runtimes": {}, "steps": [], "ports": [], "state": [],
            "effects": "pure"
        }))
        .expect("a minimal derivation");
        derivation.effects = effects;
        derivation
    }

    fn check(effects: EffectClass, authorization: EffectAuthorization<'_>) -> Option<String> {
        let contract = BoundContract {
            schema: "ato.contract/1".to_owned(),
            requirements: Vec::new(),
        };
        let derivation = derivation(effects);
        let spec = AttemptSpec {
            contract: &contract,
            contract_ref: "sha256:k",
            derivation: &derivation,
            derivation_ref: "sha256:d",
            shape: CandidateShape::Process,
            input_refs: Default::default(),
            instance_snapshot_ref: None,
        };
        admit(&spec, authorization, None).map(|failure| failure.code)
    }

    #[test]
    fn a_started_run_authorizes_running_its_derivation_and_nothing_more() {
        let user = EffectAuthorization::UserInvoked {
            derivation_ref: "sha256:d",
        };
        for effects in [
            EffectClass::Pure,
            EffectClass::Idempotent,
            EffectClass::RecordSubstitutable,
        ] {
            assert_eq!(check(effects, EffectAuthorization::Unattended), None);
            assert_eq!(check(effects, user), None);
        }
        // Starting a Run is not a confirmation of what it may leave behind.
        for effects in [
            EffectClass::RequiresConfirmation,
            EffectClass::NonRepeatable,
        ] {
            assert_eq!(
                check(effects, EffectAuthorization::Unattended).as_deref(),
                Some("effect_policy")
            );
            assert_eq!(check(effects, user).as_deref(), Some("effect_policy"));
        }
        // A Run started for another Derivation authorizes nothing here.
        let other = EffectAuthorization::UserInvoked {
            derivation_ref: "sha256:other",
        };
        assert_eq!(
            check(EffectClass::Pure, other).as_deref(),
            Some("authorization_mismatch")
        );
    }
}
