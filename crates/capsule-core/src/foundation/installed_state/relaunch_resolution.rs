//! Resolve installed-app launch conditions against current device/provider
//! facts, before relaunch admission (#508).
//!
//! The relaunch preflight (#531) reads the ledger and evaluates admission. This
//! module sits *between* the two: it takes the recorded conditions and tries to
//! resolve the ones that depend on local facts — `Unknown` host env, a
//! `UserGrantRequired` secret with a grant reference, a `UserGrantRequired`
//! state with a binding reference — into `Satisfied`, **without re-reading the
//! manifest / lockfile**.
//!
//! ## Discipline
//!
//! - Resolution never reads or stores a value: it checks *presence* (host env),
//!   a *redacted grant reference*, or a *logical binding reference* — never an
//!   env value, secret value, token, or raw host path.
//! - Only conditions that can be confirmed are lifted to `Satisfied`. A
//!   condition that can't be confirmed is **left as-is** (e.g. host env stays
//!   `Unknown`, never fabricated to `Missing`) — no fake satisfaction.
//! - `Port` is **not** resolved here: an install-time port declaration stays
//!   `Unknown` and the launch-time `PortClaim` admission (#523) owns the actual
//!   port. `Storage` / `Runtime` / `ProviderCapability` / `Network` / `Hardware`
//!   are also left untouched (dedicated resolvers are follow-ups).
//!
//! ## Persistence
//!
//! Env presence is *transient* (set this launch, maybe not the next), so an
//! env-presence resolution must never be persisted back to the ledger — that
//! would be fake satisfaction on a future launch. [`RelaunchResolution::
//! durable_persist_claims`] returns the claim set safe to write back: only
//! durable resolutions (secret grant / state binding), with env-presence
//! resolutions reverted to their original status. The full in-memory resolved
//! claims remain authoritative for the *current* launch's admission.

use serde_json::Value;

use super::launch_condition::{LaunchConditionClaim, LaunchConditionKind, LaunchConditionStatus};
use super::relaunch_admission::RelaunchAdmissionInput;

/// Input to the resolver: the installed app identity and the ledger conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaunchResolutionInput {
    pub install_profile_key: String,
    pub install_revision_id: Option<String>,
    pub provider_id: Option<String>,
    pub claims: Vec<LaunchConditionClaim>,
}

impl From<RelaunchAdmissionInput> for RelaunchResolutionInput {
    fn from(input: RelaunchAdmissionInput) -> Self {
        Self {
            install_profile_key: input.install_profile_key,
            install_revision_id: input.install_revision_id,
            provider_id: input.provider_id,
            claims: input.claims,
        }
    }
}

/// Local-fact probes used to resolve conditions. Each is a boolean existence /
/// presence check that returns **no value** — only "yes/no". Injectable so the
/// resolver is OS-free and deterministically testable.
pub struct RelaunchResolutionContext {
    /// Is the named host environment variable present? (Value never read.)
    pub env_present: Box<dyn Fn(&str) -> bool>,
    /// Does a secret grant exist for this redacted grant reference?
    pub secret_grant_exists: Box<dyn Fn(&str) -> bool>,
    /// Does a state binding exist for this logical binding reference?
    pub state_binding_exists: Box<dyn Fn(&str) -> bool>,
}

/// The provenance of a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaunchResolutionSource {
    HostEnvPresence,
    SecretGrantRef,
    StateBindingRef,
}

/// A status change applied by the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConditionUpdate {
    pub kind: LaunchConditionKind,
    pub condition_key: String,
    pub old_status: LaunchConditionStatus,
    pub new_status: LaunchConditionStatus,
    pub source: RelaunchResolutionSource,
}

/// A condition the resolver could not resolve (value-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelaunchResolutionWarning {
    EnvStillUnknown { condition_key: String },
    SecretGrantMissing { condition_key: String },
    StateBindingMissing { condition_key: String },
}

/// The result of resolution: the fully-resolved claims (authoritative for this
/// launch's admission), the updates applied, and warnings for the unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaunchResolution {
    pub install_profile_key: String,
    pub install_revision_id: Option<String>,
    pub provider_id: Option<String>,
    pub claims: Vec<LaunchConditionClaim>,
    pub updates: Vec<LaunchConditionUpdate>,
    pub warnings: Vec<RelaunchResolutionWarning>,
}

impl RelaunchResolution {
    /// Build the admission input from the fully-resolved (in-memory) claims.
    pub fn to_admission_input(&self) -> RelaunchAdmissionInput {
        RelaunchAdmissionInput {
            install_profile_key: self.install_profile_key.clone(),
            install_revision_id: self.install_revision_id.clone(),
            provider_id: self.provider_id.clone(),
            claims: self.claims.clone(),
        }
    }

    /// The claim set that is safe to **persist** back to the ledger, or `None`
    /// when nothing durable changed.
    ///
    /// Only durable resolutions (secret grant / state binding) are written back;
    /// host-env-presence resolutions are reverted to their original status,
    /// because env presence is transient and persisting it as `Satisfied` would
    /// be fake satisfaction on a future launch where the env is absent.
    pub fn durable_persist_claims(&self) -> Option<Vec<LaunchConditionClaim>> {
        let has_durable = self
            .updates
            .iter()
            .any(|u| u.source != RelaunchResolutionSource::HostEnvPresence);
        if !has_durable {
            return None;
        }
        let mut claims = self.claims.clone();
        for claim in &mut claims {
            // Revert any env-presence resolution before persisting.
            if let Some(update) = self.updates.iter().find(|u| {
                u.source == RelaunchResolutionSource::HostEnvPresence
                    && u.kind == claim.kind
                    && u.condition_key == claim.condition_key
            }) {
                claim.status = update.old_status;
            }
        }
        Some(claims)
    }
}

/// Resolve relaunch conditions against the local-fact probes. Pure (the probes
/// encapsulate any I/O). Conditions that can't be confirmed are left unchanged.
pub fn resolve_relaunch_conditions(
    input: RelaunchResolutionInput,
    ctx: &RelaunchResolutionContext,
) -> RelaunchResolution {
    let mut claims = input.claims;
    let mut updates = Vec::new();
    let mut warnings = Vec::new();

    for claim in &mut claims {
        match claim.kind {
            // Host env: presence-only. Lift Unknown → Satisfied when present;
            // otherwise leave Unknown (never Missing). Value is never read.
            LaunchConditionKind::Env if claim.status == LaunchConditionStatus::Unknown => {
                if (ctx.env_present)(&claim.condition_key) {
                    push_update(
                        &mut updates,
                        claim,
                        RelaunchResolutionSource::HostEnvPresence,
                    );
                } else {
                    warnings.push(RelaunchResolutionWarning::EnvStillUnknown {
                        condition_key: claim.condition_key.clone(),
                    });
                }
            }
            // Secret: resolvable only with a non-null redacted grant_ref that the
            // grant probe confirms. The secret value is never inspected.
            LaunchConditionKind::Secret
                if claim.status == LaunchConditionStatus::UserGrantRequired =>
            {
                match detail_string(claim, "grant_ref") {
                    Some(grant_ref) if (ctx.secret_grant_exists)(&grant_ref) => {
                        push_update(
                            &mut updates,
                            claim,
                            RelaunchResolutionSource::SecretGrantRef,
                        );
                    }
                    _ => warnings.push(RelaunchResolutionWarning::SecretGrantMissing {
                        condition_key: claim.condition_key.clone(),
                    }),
                }
            }
            // State: resolvable only with a logical binding_ref the binding probe
            // confirms. No raw host path is read or required.
            LaunchConditionKind::State
                if claim.status == LaunchConditionStatus::UserGrantRequired =>
            {
                match detail_string(claim, "binding_ref") {
                    Some(binding_ref) if (ctx.state_binding_exists)(&binding_ref) => {
                        push_update(
                            &mut updates,
                            claim,
                            RelaunchResolutionSource::StateBindingRef,
                        );
                    }
                    _ => warnings.push(RelaunchResolutionWarning::StateBindingMissing {
                        condition_key: claim.condition_key.clone(),
                    }),
                }
            }
            // Port (Unknown → #523's launch-time admission), Storage, Runtime,
            // RuntimeTool, ProviderCapability, Network, Hardware, Policy, and any
            // already-resolved condition are intentionally left unchanged.
            _ => {}
        }
    }

    RelaunchResolution {
        install_profile_key: input.install_profile_key,
        install_revision_id: input.install_revision_id,
        provider_id: input.provider_id,
        claims,
        updates,
        warnings,
    }
}

/// Apply a `→ Satisfied` resolution to a claim and record the update.
fn push_update(
    updates: &mut Vec<LaunchConditionUpdate>,
    claim: &mut LaunchConditionClaim,
    source: RelaunchResolutionSource,
) {
    let old_status = claim.status;
    claim.status = LaunchConditionStatus::Satisfied;
    updates.push(LaunchConditionUpdate {
        kind: claim.kind,
        condition_key: claim.condition_key.clone(),
        old_status,
        new_status: LaunchConditionStatus::Satisfied,
        source,
    });
}

/// Read a non-null string field from a claim's `detail_json` (e.g. a redacted
/// `grant_ref` / logical `binding_ref`). Returns `None` for null / missing /
/// non-string / unparseable — never the secret value.
fn detail_string(claim: &LaunchConditionClaim, key: &str) -> Option<String> {
    let detail: Value = serde_json::from_str(&claim.detail_json).ok()?;
    detail.get(key)?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::super::launch_condition::LaunchConditionSource;
    use super::*;

    fn ctx(env_present: bool, grant: bool, binding: bool) -> RelaunchResolutionContext {
        RelaunchResolutionContext {
            env_present: Box::new(move |_| env_present),
            secret_grant_exists: Box::new(move |_| grant),
            state_binding_exists: Box::new(move |_| binding),
        }
    }

    fn claim(
        kind: LaunchConditionKind,
        condition_key: &str,
        status: LaunchConditionStatus,
        detail_json: &str,
    ) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: "ipk_app".to_string(),
            install_revision_id: Some("rev1".to_string()),
            provider_id: None,
            kind,
            condition_key: condition_key.to_string(),
            status,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: detail_json.to_string(),
            redacted: true,
        }
    }

    fn input(claims: Vec<LaunchConditionClaim>) -> RelaunchResolutionInput {
        RelaunchResolutionInput {
            install_profile_key: "ipk_app".to_string(),
            install_revision_id: Some("rev1".to_string()),
            provider_id: None,
            claims,
        }
    }

    fn status_of(res: &RelaunchResolution, key: &str) -> LaunchConditionStatus {
        res.claims
            .iter()
            .find(|c| c.condition_key == key)
            .unwrap()
            .status
    }

    #[test]
    fn resolve_env_unknown_to_satisfied_when_host_env_present() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::Env,
                "DATABASE_URL",
                LaunchConditionStatus::Unknown,
                r#"{"source":"manifest.required_env"}"#,
            )]),
            &ctx(true, false, false),
        );
        assert_eq!(
            status_of(&res, "DATABASE_URL"),
            LaunchConditionStatus::Satisfied
        );
        assert_eq!(res.updates.len(), 1);
        assert_eq!(
            res.updates[0].source,
            RelaunchResolutionSource::HostEnvPresence
        );
    }

    #[test]
    fn resolve_env_unknown_remains_unknown_when_absent() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::Env,
                "DATABASE_URL",
                LaunchConditionStatus::Unknown,
                r#"{"source":"manifest.required_env"}"#,
            )]),
            &ctx(false, false, false),
        );
        assert_eq!(
            status_of(&res, "DATABASE_URL"),
            LaunchConditionStatus::Unknown,
            "absent host env must stay Unknown, never fabricated to Missing"
        );
        assert!(matches!(
            res.warnings[0],
            RelaunchResolutionWarning::EnvStillUnknown { .. }
        ));
    }

    #[test]
    fn resolve_secret_user_grant_required_to_satisfied_when_grant_ref_exists() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"projection":"env","grant_ref":"ato-secret://store/openai"}"#,
            )]),
            &ctx(false, true, false),
        );
        assert_eq!(
            status_of(&res, "OPENAI_API_KEY"),
            LaunchConditionStatus::Satisfied
        );
        assert_eq!(
            res.updates[0].source,
            RelaunchResolutionSource::SecretGrantRef
        );
    }

    #[test]
    fn resolve_secret_user_grant_required_stays_when_grant_ref_null() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"projection":"env","grant_ref":null}"#,
            )]),
            // Even with a grant probe that would say yes, a null grant_ref means
            // there's nothing to confirm → stays UserGrantRequired.
            &ctx(false, true, false),
        );
        assert_eq!(
            status_of(&res, "OPENAI_API_KEY"),
            LaunchConditionStatus::UserGrantRequired
        );
        assert!(matches!(
            res.warnings[0],
            RelaunchResolutionWarning::SecretGrantMissing { .. }
        ));
    }

    #[test]
    fn resolve_state_user_grant_required_to_satisfied_when_binding_ref_exists() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"binding_ref":"ato-state://app/data","durability":"persistent"}"#,
            )]),
            &ctx(false, false, true),
        );
        assert_eq!(status_of(&res, "data"), LaunchConditionStatus::Satisfied);
        assert_eq!(
            res.updates[0].source,
            RelaunchResolutionSource::StateBindingRef
        );
    }

    #[test]
    fn resolve_state_user_grant_required_stays_when_no_binding_ref() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"durability":"persistent"}"#,
            )]),
            &ctx(false, false, true),
        );
        assert_eq!(
            status_of(&res, "data"),
            LaunchConditionStatus::UserGrantRequired,
            "no binding_ref → cannot confirm, stays UserGrantRequired"
        );
        assert!(matches!(
            res.warnings[0],
            RelaunchResolutionWarning::StateBindingMissing { .. }
        ));
    }

    #[test]
    fn resolve_port_unknown_is_not_resolved_by_this_slice() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::Port,
                "ato://app/ipk_app/main.tcp",
                LaunchConditionStatus::Unknown,
                r#"{"logical_endpoint":"ato://app/ipk_app/main","protocol":"tcp"}"#,
            )]),
            &ctx(true, true, true),
        );
        assert_eq!(
            status_of(&res, "ato://app/ipk_app/main.tcp"),
            LaunchConditionStatus::Unknown,
            "port stays Unknown; #523 owns launch-time port admission"
        );
        assert!(res.updates.is_empty());
    }

    #[test]
    fn resolver_never_writes_secret_or_env_values() {
        // Resolution only flips status; it never copies detail values anywhere.
        let res = resolve_relaunch_conditions(
            input(vec![
                claim(
                    LaunchConditionKind::Secret,
                    "OPENAI_API_KEY",
                    LaunchConditionStatus::UserGrantRequired,
                    r#"{"projection":"env","grant_ref":"ato-secret://store/openai"}"#,
                ),
                claim(
                    LaunchConditionKind::Env,
                    "DATABASE_URL",
                    LaunchConditionStatus::Unknown,
                    r#"{"source":"manifest.required_env"}"#,
                ),
            ]),
            &ctx(true, true, false),
        );
        for update in &res.updates {
            // Updates carry kind + key + statuses only — no value fields.
            assert!(!format!("{update:?}").contains("sk-"));
        }
        // The grant_ref locator (already redacted) stays in detail; no raw secret.
        assert!(res.claims.iter().all(|c| !c.detail_json.contains("sk-")));
    }

    #[test]
    fn resolver_reports_updates() {
        let res = resolve_relaunch_conditions(
            input(vec![
                claim(
                    LaunchConditionKind::Env,
                    "DATABASE_URL",
                    LaunchConditionStatus::Unknown,
                    r#"{"source":"manifest.required_env"}"#,
                ),
                claim(
                    LaunchConditionKind::Secret,
                    "OPENAI_API_KEY",
                    LaunchConditionStatus::UserGrantRequired,
                    r#"{"grant_ref":"ato-secret://store/openai"}"#,
                ),
            ]),
            &ctx(true, true, false),
        );
        assert_eq!(res.updates.len(), 2);
        // Env-presence is transient → not persisted; secret grant is durable.
        let persist = res
            .durable_persist_claims()
            .expect("a durable secret resolution must be persistable");
        let env = persist
            .iter()
            .find(|c| c.condition_key == "DATABASE_URL")
            .unwrap();
        assert_eq!(
            env.status,
            LaunchConditionStatus::Unknown,
            "env-presence resolution must be reverted for persistence (transient)"
        );
        let secret = persist
            .iter()
            .find(|c| c.condition_key == "OPENAI_API_KEY")
            .unwrap();
        assert_eq!(
            secret.status,
            LaunchConditionStatus::Satisfied,
            "durable secret-grant resolution is persisted"
        );
    }

    #[test]
    fn durable_persist_claims_is_none_for_env_only_resolution() {
        let res = resolve_relaunch_conditions(
            input(vec![claim(
                LaunchConditionKind::Env,
                "DATABASE_URL",
                LaunchConditionStatus::Unknown,
                r#"{"source":"manifest.required_env"}"#,
            )]),
            &ctx(true, false, false),
        );
        assert!(
            res.durable_persist_claims().is_none(),
            "a transient env-only resolution must not be persisted"
        );
    }
}
