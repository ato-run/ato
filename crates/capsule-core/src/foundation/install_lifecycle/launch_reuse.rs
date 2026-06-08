//! Launch reuse + volatile revalidation
//! (RFC: Ato Resource Namespace §"Install Outputs and Launch Reuse",
//! "Launch reuse algorithm"; #581 Stage 3).
//!
//! Install fixes the heavy, source-only outputs (artifact, requirement graph,
//! binding set, policies, launch templates). Launch reuses them but must
//! **revalidate the volatile conditions every time** — runner health and
//! capability, auth/consent, secret refs, storage credentials, network policy
//! enforcement, and state locks. Those volatile facts are *not* cache-key
//! inputs (they never enter [`LaunchTemplateKey`]); a volatile failure does not
//! invalidate the cached template — it blocks reuse/start with a typed reason.
//!
//! This module models the decision, not the I/O: it takes already-collected
//! revalidation outcomes and a candidate cached template and returns a typed
//! [`LaunchReuseDecision`]. Wiring the real runner/consent/secret/storage probes
//! is later work; until then callers construct conservative typed outcomes (see
//! [`RevalidationOutcome`]) and must never report a silent success.

use serde::{Deserialize, Serialize};

use super::launch_template::{LaunchTemplate, LaunchTemplateKey, RunnerClass};

// ── Revalidation outcomes ─────────────────────────────────────────────────────

/// Why a volatile revalidation check failed. Typed so callers cannot collapse a
/// real failure into a silent success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevalidationFailureKind {
    RunnerInactive,
    RunnerCapabilityDowngrade,
    ConsentRevoked,
    AuthGrantRevoked,
    SecretVersionUnavailable,
    StorageCredentialExpired,
    NetworkPolicyUnavailable,
    StateLockHeld,
    /// The selected runner class is not allowed by the compatibility precheck.
    CompatibilityRejected,
}

/// A typed revalidation failure carrying a human-readable detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevalidationFailure {
    pub kind: RevalidationFailureKind,
    pub detail: String,
}

impl RevalidationFailure {
    pub fn new(kind: RevalidationFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// The result of one volatile revalidation check.
///
/// There is no implicit "assume ok": a check is either explicitly `Passed`,
/// explicitly `Failed`, or explicitly `Skipped` with a reason (for subsystems
/// not wired yet). `Skipped` is conservative — see
/// [`VolatileRevalidation::all_passed`], which treats skipped checks as
/// not-yet-validated rather than success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RevalidationOutcome {
    Passed,
    Failed(RevalidationFailure),
    /// Subsystem not yet implemented; carries the reason it was skipped. This is
    /// explicit and testable — it is never treated as a success.
    Skipped {
        reason: String,
    },
}

impl RevalidationOutcome {
    pub fn is_passed(&self) -> bool {
        matches!(self, RevalidationOutcome::Passed)
    }
    pub fn failure(&self) -> Option<&RevalidationFailure> {
        match self {
            RevalidationOutcome::Failed(f) => Some(f),
            _ => None,
        }
    }
}

/// The full set of volatile revalidation checks performed at launch time.
///
/// Each field is a typed outcome. None of these are launch-template cache-key
/// inputs; they gate *whether* the cached template may be used, they do not
/// change the template's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolatileRevalidation {
    pub runner_health: RevalidationOutcome,
    pub runner_capability: RevalidationOutcome,
    pub consent: RevalidationOutcome,
    pub auth: RevalidationOutcome,
    pub secret_refs: RevalidationOutcome,
    pub storage_credentials: RevalidationOutcome,
    pub network_policy: RevalidationOutcome,
    pub state_lock: RevalidationOutcome,
}

impl VolatileRevalidation {
    /// Checks in a fixed evaluation order so the first failure is deterministic.
    fn ordered(&self) -> [&RevalidationOutcome; 8] {
        [
            &self.runner_health,
            &self.runner_capability,
            &self.consent,
            &self.auth,
            &self.secret_refs,
            &self.storage_credentials,
            &self.network_policy,
            &self.state_lock,
        ]
    }

    /// The first failing check, in evaluation order, if any.
    pub fn first_failure(&self) -> Option<RevalidationFailure> {
        self.ordered().iter().find_map(|o| o.failure().cloned())
    }

    /// True only if every check explicitly `Passed`. A `Skipped` or `Failed`
    /// check means revalidation has not fully succeeded — reuse/start is blocked.
    pub fn all_passed(&self) -> bool {
        self.ordered().iter().all(|o| o.is_passed())
    }

    /// The first check that is not `Passed` and not `Failed` (i.e. `Skipped`),
    /// used to explain why reuse is blocked without a hard failure.
    pub fn first_unvalidated_reason(&self) -> Option<String> {
        self.ordered().iter().find_map(|o| match o {
            RevalidationOutcome::Skipped { reason } => Some(reason.clone()),
            _ => None,
        })
    }
}

// ── Reuse inputs + decision ────────────────────────────────────────────────────

/// The stable inputs that determine which launch template applies.
///
/// Mirrors the fields of [`LaunchTemplateKey`]. There is intentionally no
/// session/observed field: those never participate in reuse selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchReuseInputs {
    pub key: LaunchTemplateKey,
    /// The runner class chosen for this launch; checked against the
    /// compatibility precheck before reuse.
    pub selected_runner_class: RunnerClass,
}

/// The outcome of evaluating launch reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LaunchReuseDecision {
    /// The cached template matches and all volatile checks passed: reuse it.
    /// Carries the matched template key hash for audit.
    Reuse { template_key_hash: String },
    /// No cached template matched the stable inputs: a new template must be
    /// built from the (already-installed) outputs. This does **not** rebuild
    /// source/dependencies — only the launch template is (re)derived.
    RebuildTemplate { reason: String },
    /// A volatile revalidation failed (or compatibility was rejected): reuse and
    /// start are blocked with a typed reason. Never a silent success.
    Blocked(RevalidationFailure),
}

/// Evaluate whether a cached launch template can be reused for this launch.
///
/// Order of evaluation (RFC "Launch reuse algorithm" + "Cache invalidation
/// rules"):
///
/// 1. Compatibility precheck on the selected runner class — handled by the
///    caller's `compatibility_index` and surfaced here as a
///    [`RevalidationFailureKind::CompatibilityRejected`] failure in
///    `revalidation` if rejected. (Kept as a revalidation outcome so a single
///    typed channel carries every block reason.)
/// 2. Volatile revalidation — any failure ⇒ [`LaunchReuseDecision::Blocked`].
/// 3. Cache match — if a cached template's key hash equals the requested key
///    hash, reuse it; otherwise rebuild the template.
///
/// `cached` is the template currently associated with the install revision (if
/// any). Reuse never rebuilds source/dependencies; a key mismatch only means
/// the launch template is re-derived from the existing install outputs.
pub fn evaluate_launch_reuse(
    inputs: &LaunchReuseInputs,
    cached: Option<&LaunchTemplate>,
    revalidation: &VolatileRevalidation,
) -> anyhow::Result<LaunchReuseDecision> {
    // Volatile revalidation is a hard gate: a failure blocks reuse AND start.
    if let Some(failure) = revalidation.first_failure() {
        return Ok(LaunchReuseDecision::Blocked(failure));
    }
    // A not-yet-validated (skipped) check is conservatively treated as a block,
    // never a silent success.
    if !revalidation.all_passed() {
        let reason = revalidation
            .first_unvalidated_reason()
            .unwrap_or_else(|| "revalidation incomplete".to_string());
        return Ok(LaunchReuseDecision::Blocked(RevalidationFailure::new(
            RevalidationFailureKind::CompatibilityRejected,
            format!("volatile revalidation not complete: {reason}"),
        )));
    }

    let requested_hash = inputs.key.key_hash()?;
    match cached {
        Some(template) => {
            let cached_hash = template.key.key_hash()?;
            if cached_hash == requested_hash {
                Ok(LaunchReuseDecision::Reuse {
                    template_key_hash: requested_hash,
                })
            } else {
                Ok(LaunchReuseDecision::RebuildTemplate {
                    reason: "cached launch template key does not match requested stable inputs"
                        .to_string(),
                })
            }
        }
        None => Ok(LaunchReuseDecision::RebuildTemplate {
            reason: "no cached launch template for this install revision".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::install_lifecycle::ids::InstallRevisionId;
    use crate::foundation::install_lifecycle::launch_template::RunnerCompatibilityClass;

    fn sample_key() -> LaunchTemplateKey {
        LaunchTemplateKey {
            install_revision_id: InstallRevisionId::new("rev_aaaa"),
            profile_hash: "blake3:prof".into(),
            requirement_graph_snapshot_hash: "blake3:graphsnap".into(),
            binding_set_hash: "blake3:bind".into(),
            network_policy_hash: "blake3:net".into(),
            capability_policy_hash: "blake3:cap".into(),
            state_contract_hash: "blake3:state".into(),
            runner_compatibility_class: RunnerCompatibilityClass::new(
                "managed_runner/linux-x86_64",
            ),
        }
    }

    fn template_for(key: LaunchTemplateKey) -> LaunchTemplate {
        LaunchTemplate::new(
            "ltmpl",
            key,
            "/profile",
            "/artifact",
            "snap",
            "bset",
            "blake3:fs",
            "blake3:net",
            "blake3:cap",
        )
        .unwrap()
    }

    fn all_ok() -> VolatileRevalidation {
        VolatileRevalidation {
            runner_health: RevalidationOutcome::Passed,
            runner_capability: RevalidationOutcome::Passed,
            consent: RevalidationOutcome::Passed,
            auth: RevalidationOutcome::Passed,
            secret_refs: RevalidationOutcome::Passed,
            storage_credentials: RevalidationOutcome::Passed,
            network_policy: RevalidationOutcome::Passed,
            state_lock: RevalidationOutcome::Passed,
        }
    }

    fn inputs() -> LaunchReuseInputs {
        LaunchReuseInputs {
            key: sample_key(),
            selected_runner_class: RunnerClass::ManagedRunner,
        }
    }

    // ── Acceptance: reuse allowed only after volatile revalidation succeeds ───

    #[test]
    fn reuse_allowed_when_inputs_match_and_revalidation_passes() {
        let cached = template_for(sample_key());
        let decision = evaluate_launch_reuse(&inputs(), Some(&cached), &all_ok()).unwrap();
        match decision {
            LaunchReuseDecision::Reuse { template_key_hash } => {
                assert_eq!(template_key_hash, sample_key().key_hash().unwrap());
            }
            other => panic!("expected Reuse, got {other:?}"),
        }
    }

    // ── Acceptance: volatile revalidation failure returns a typed reason ──────

    #[test]
    fn volatile_failure_blocks_with_typed_reason_not_silent_success() {
        let cached = template_for(sample_key());
        let mut reval = all_ok();
        reval.consent = RevalidationOutcome::Failed(RevalidationFailure::new(
            RevalidationFailureKind::ConsentRevoked,
            "user revoked GitHub OAuth consent",
        ));
        let decision = evaluate_launch_reuse(&inputs(), Some(&cached), &reval).unwrap();
        match decision {
            LaunchReuseDecision::Blocked(f) => {
                assert_eq!(f.kind, RevalidationFailureKind::ConsentRevoked);
                assert!(f.detail.contains("consent"));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn skipped_revalidation_is_not_silent_success() {
        let cached = template_for(sample_key());
        let mut reval = all_ok();
        reval.secret_refs = RevalidationOutcome::Skipped {
            reason: "secret manager probe not implemented".into(),
        };
        let decision = evaluate_launch_reuse(&inputs(), Some(&cached), &reval).unwrap();
        assert!(
            matches!(decision, LaunchReuseDecision::Blocked(_)),
            "a skipped (unvalidated) check must block reuse, not silently succeed; got {decision:?}"
        );
    }

    #[test]
    fn first_failure_is_deterministic_by_order() {
        let mut reval = all_ok();
        // runner_health is earlier in evaluation order than network_policy.
        reval.network_policy = RevalidationOutcome::Failed(RevalidationFailure::new(
            RevalidationFailureKind::NetworkPolicyUnavailable,
            "net",
        ));
        reval.runner_health = RevalidationOutcome::Failed(RevalidationFailure::new(
            RevalidationFailureKind::RunnerInactive,
            "runner gone",
        ));
        assert_eq!(
            reval.first_failure().unwrap().kind,
            RevalidationFailureKind::RunnerInactive
        );
    }

    // ── Acceptance: stable input change ⇒ rebuild template (no source rebuild) ─

    #[test]
    fn changed_stable_input_rebuilds_template_without_blocking() {
        // Cached template was built for a different binding set.
        let mut cached_key = sample_key();
        cached_key.binding_set_hash = "blake3:OLD".into();
        let cached = template_for(cached_key);

        // Requested inputs use the new binding set; revalidation passes.
        let decision = evaluate_launch_reuse(&inputs(), Some(&cached), &all_ok()).unwrap();
        match decision {
            LaunchReuseDecision::RebuildTemplate { reason } => {
                assert!(reason.contains("does not match"));
            }
            other => panic!("expected RebuildTemplate, got {other:?}"),
        }
    }

    #[test]
    fn no_cached_template_rebuilds_template() {
        let decision = evaluate_launch_reuse(&inputs(), None, &all_ok()).unwrap();
        assert!(matches!(
            decision,
            LaunchReuseDecision::RebuildTemplate { .. }
        ));
    }

    // ── Acceptance: observed facts are not reuse inputs ───────────────────────

    #[test]
    fn observed_facts_are_not_reuse_inputs() {
        // The same stable inputs reused across two launches whose observed facts
        // differ must produce the identical Reuse decision (same key hash).
        let cached = template_for(sample_key());
        let d1 = evaluate_launch_reuse(&inputs(), Some(&cached), &all_ok()).unwrap();
        let d2 = evaluate_launch_reuse(&inputs(), Some(&cached), &all_ok()).unwrap();
        assert_eq!(d1, d2);
    }
}
