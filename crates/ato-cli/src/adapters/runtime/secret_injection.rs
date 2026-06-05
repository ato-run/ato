//! Inject SecretStore-backed launch-condition grants into the runtime env during
//! installed relaunch (#508).
//!
//! #545 created real secret grants from `secret.*=prompt` (value → secure store →
//! `secret_grant_ref` → input rewritten to `grant:<id>`). This module consumes an
//! admitted `grant:<id>` at runtime: it reads the value back from the secure store
//! and projects it into the launched process env.
//!
//! Two security properties hold here:
//! - **Grant existence is not value availability.** A `secret_grant_ref` proves a
//!   grant was selected; the value must still exist in the secure store. A missing
//!   value blocks the launch with a typed error — it is never silently skipped.
//! - **Raw secret values are runtime-only and redacted.** [`SecretValue`] has a
//!   redacted `Debug`, no `Display`, and exposes its inner string only through a
//!   crate-private accessor used at the final env-construction point. Resolved
//!   secrets travel on a dedicated [`RuntimeLaunchContext`] channel that is
//!   excluded from the receipt/session env observation, so a value never reaches
//!   the execution receipt, session record, logs, or an error message.
//!
//! Planning is pure and value-free ([`plan_secret_injection`]); value retrieval is
//! a separate seam ([`SecretGrantValueStore`]). Nothing here reads `capsule.toml`,
//! the manifest, or a lockfile — the installed-state ledger is the source of truth.
//!
//! [`RuntimeLaunchContext`]: crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext

use anyhow::{Context, Result, bail};
use capsule_core::installed_state::{
    InstalledStateDb, LaunchConditionClaim, LaunchConditionInput, LaunchConditionInputKind,
    LaunchConditionInputValue, LaunchConditionKind,
};

use crate::application::secrets::SecretStore;
use crate::utils::error::{
    ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED, ATO_ERR_LAUNCH_CONDITION_SECRET_STORE_LOCKED,
    ATO_ERR_LAUNCH_CONDITION_SECRET_VALUE_MISSING,
};

/// A resolved secret value bound for the runtime env. Opaque on purpose: its
/// `Debug` is redacted, it has no `Display`, and the inner string is reachable
/// only via the crate-private [`SecretValue::expose`] at the env-insertion point.
#[derive(Clone)]
pub(crate) struct SecretValue {
    value: String,
}

impl SecretValue {
    pub(crate) fn new(value: String) -> Self {
        Self { value }
    }

    /// Reveal the raw value. Call only at the final point where the env map for
    /// the spawned process is assembled — never log or serialize the result.
    pub(crate) fn expose(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(***redacted***)")
    }
}

/// A resolved `(env name, secret value)` pair to inject. The name is not
/// sensitive; the value is redacted.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeSecretEnv {
    pub name: String,
    pub value: SecretValue,
}

/// A single planned secret env injection — env var name + the grant id whose
/// value to inject. Carries **no** secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretEnvVar {
    pub name: String,
    pub grant_id: String,
}

/// The pure plan: which secret grants project to which env var names. Value-free.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SecretInjectionPlan {
    pub env: Vec<SecretEnvVar>,
}

/// Seam for reading a secret value out of secure storage. Production uses
/// [`SecretStore`]; tests inject a fake. Errors must never include the value.
pub(crate) trait SecretGrantValueStore {
    fn get_secret(&self, namespace: &str, key: &str) -> Result<Option<SecretValue>>;
}

/// Production value store: reads from the age-encrypted [`SecretStore`] under the
/// per-install-profile namespace #545 wrote to. Distinguishes a locked store
/// (typed error) from a genuinely absent value (`Ok(None)`).
pub(crate) struct SecretStoreValueStore;

impl SecretGrantValueStore for SecretStoreValueStore {
    fn get_secret(&self, namespace: &str, key: &str) -> Result<Option<SecretValue>> {
        let store = SecretStore::open().context("open secret store")?;
        if let Some(value) = store.get_in_namespace(key, namespace)? {
            return Ok(Some(SecretValue::new(value)));
        }
        // No value found. If the age store is locked, the prompt-created secret is
        // simply unreadable — surface that as locked rather than "missing".
        if store.age().is_none() {
            bail!(
                "{code}: the secret store is locked — run `ato secrets init` to create an \
                 identity, or `ato session start` to unlock it, then relaunch",
                code = ATO_ERR_LAUNCH_CONDITION_SECRET_STORE_LOCKED,
            );
        }
        Ok(None)
    }
}

fn inject_err(msg: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(msg.into())
}

/// Plan secret env injections from admitted launch inputs (pure, value-free).
///
/// Only `secret.<name>=grant:<id>` inputs project this slice (env-via-grant and
/// non-secret kinds are follow-ups). Each must match a `Secret` ledger claim
/// (bare `K` or namespaced `secret.K`) or it is a typed unknown-condition error.
/// The env var name is the secret name (the input key). A `=prompt` input that was
/// never rewritten to a grant, and any non-`grant:` input, are ignored (no value
/// to inject).
pub(crate) fn plan_secret_injection(
    claims: &[LaunchConditionClaim],
    inputs: &[LaunchConditionInput],
) -> Result<SecretInjectionPlan> {
    let mut env = Vec::new();
    for input in inputs {
        if input.kind != LaunchConditionInputKind::Secret {
            continue;
        }
        let LaunchConditionInputValue::Grant(grant_id) = &input.value else {
            // `prompt` (not yet rewritten), `required`, etc. carry no grant.
            continue;
        };
        let matched = claims.iter().any(|c| {
            c.kind == LaunchConditionKind::Secret
                && matches_key(&c.condition_key, "secret", &input.key)
        });
        if !matched {
            return Err(inject_err(format!(
                "launch input references unknown condition 'secret.{}'",
                input.key
            )));
        }
        env.push(SecretEnvVar {
            name: input.key.clone(),
            grant_id: grant_id.clone(),
        });
    }
    Ok(SecretInjectionPlan { env })
}

/// Resolve admitted secret grants into concrete `(env name, value)` pairs by
/// reading the secure store, after relaunch preflight admission and before spawn.
///
/// For each planned secret grant: confirm via the registry that the grant belongs
/// to this install profile and is `granted` (never inject another app's grant),
/// then read its value. A grant that exists but has no stored value blocks the
/// launch with a typed error (grant selection is not value availability). No value
/// ever enters an error message.
///
/// Short-circuits with no secret-grant input (no ledger read). Reads only the
/// installed-state ledger and the secure store — never the manifest/lockfile.
pub(crate) fn resolve_secret_injection(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    inputs: &[LaunchConditionInput],
    value_store: &dyn SecretGrantValueStore,
) -> Result<Vec<RuntimeSecretEnv>> {
    if !inputs.iter().any(is_secret_grant_input) {
        return Ok(Vec::new());
    }

    let claims = db
        .load_relaunch_admission_input(install_profile_key, install_revision_id, None)
        .context("load installed-state ledger for secret injection")?
        .claims;
    let plan = plan_secret_injection(&claims, inputs)?;

    let mut resolved = Vec::with_capacity(plan.env.len());
    for entry in &plan.env {
        // Ownership + status: the grant must belong to THIS install profile and be
        // granted. Defence-in-depth — never inject a grant from another app.
        match db.read_secret_grant_ref(&entry.grant_id)? {
            Some(rec)
                if rec.status == "granted" && rec.install_profile_key == install_profile_key => {}
            Some(rec) if rec.install_profile_key != install_profile_key => {
                bail!(
                    "{code}: secret grant for 'secret.{name}' belongs to a different installed \
                     app; refusing to inject",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED,
                    name = entry.name,
                );
            }
            _ => {
                bail!(
                    "{code}: no granted secret for 'secret.{name}'; relaunch with \
                     secret.{name}=prompt to create it",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_VALUE_MISSING,
                    name = entry.name,
                );
            }
        }

        let value = value_store
            .get_secret(install_profile_key, &entry.name)?
            .ok_or_else(|| {
                inject_err(format!(
                    "{code}: a grant exists for 'secret.{name}' but no value is stored; relaunch \
                     with secret.{name}=prompt to set it",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_VALUE_MISSING,
                    name = entry.name,
                ))
            })?;
        resolved.push(RuntimeSecretEnv {
            name: entry.name.clone(),
            value,
        });
    }
    Ok(resolved)
}

fn is_secret_grant_input(input: &LaunchConditionInput) -> bool {
    input.kind == LaunchConditionInputKind::Secret
        && matches!(input.value, LaunchConditionInputValue::Grant(_))
}

/// Match a ledger claim key against `<namespace>.<input_key>` (bare or namespaced).
fn matches_key(claim_key: &str, namespace: &str, input_key: &str) -> bool {
    claim_key == input_key || claim_key == format!("{namespace}.{input_key}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::installed_state::{LaunchConditionSource, LaunchConditionStatus};
    use std::cell::RefCell;

    const SECRET: &str = "sk-super-secret-value-1234567890";

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = InstalledStateDb::open(dir.path().join("state")).unwrap();
        (dir, db)
    }

    fn secret_claim(ipk: &str, rev: &str, condition_key: &str) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: ipk.to_string(),
            install_revision_id: Some(rev.to_string()),
            provider_id: None,
            kind: LaunchConditionKind::Secret,
            condition_key: condition_key.to_string(),
            status: LaunchConditionStatus::Satisfied,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: r#"{"projection":"env"}"#.to_string(),
            redacted: true,
        }
    }

    fn grant_input(key: &str, grant_id: &str) -> LaunchConditionInput {
        LaunchConditionInput {
            kind: LaunchConditionInputKind::Secret,
            key: key.to_string(),
            value: LaunchConditionInputValue::Grant(grant_id.to_string()),
        }
    }

    struct FakeValueStore {
        value: Option<String>,
    }
    impl SecretGrantValueStore for FakeValueStore {
        fn get_secret(&self, _namespace: &str, _key: &str) -> Result<Option<SecretValue>> {
            Ok(self.value.clone().map(SecretValue::new))
        }
    }

    // ── pure planner ────────────────────────────────────────────────────────

    #[test]
    fn secret_injection_plan_maps_secret_grant_to_env_name() {
        let claims = vec![secret_claim("ipk", "rev1", "OPENAI_API_KEY")];
        let plan = plan_secret_injection(&claims, &[grant_input("OPENAI_API_KEY", "g1")]).unwrap();
        assert_eq!(
            plan.env,
            vec![SecretEnvVar {
                name: "OPENAI_API_KEY".to_string(),
                grant_id: "g1".to_string(),
            }]
        );
    }

    #[test]
    fn secret_injection_plan_matches_namespaced_ledger_key() {
        let claims = vec![secret_claim("ipk", "rev1", "secret.OPENAI_API_KEY")];
        let plan = plan_secret_injection(&claims, &[grant_input("OPENAI_API_KEY", "g1")]).unwrap();
        assert_eq!(plan.env.len(), 1);
        assert_eq!(plan.env[0].name, "OPENAI_API_KEY");
    }

    #[test]
    fn secret_injection_plan_ignores_prompt_and_non_grant_inputs() {
        let claims = vec![secret_claim("ipk", "rev1", "OPENAI_API_KEY")];
        let inputs = vec![
            LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: "OPENAI_API_KEY".to_string(),
                value: LaunchConditionInputValue::Prompt,
            },
            LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: "OPENAI_API_KEY".to_string(),
                value: LaunchConditionInputValue::Required,
            },
        ];
        assert!(
            plan_secret_injection(&claims, &inputs)
                .unwrap()
                .env
                .is_empty()
        );
    }

    #[test]
    fn secret_injection_plan_rejects_unknown_secret_condition() {
        let claims = vec![secret_claim("ipk", "rev1", "OTHER")];
        let err =
            plan_secret_injection(&claims, &[grant_input("OPENAI_API_KEY", "g1")]).unwrap_err();
        assert!(err.to_string().contains("unknown condition"));
    }

    #[test]
    fn secret_injection_plan_does_not_include_raw_secret_value() {
        // The plan carries grant ids, never values (structural + format check).
        let claims = vec![secret_claim("ipk", "rev1", "OPENAI_API_KEY")];
        let plan = plan_secret_injection(&claims, &[grant_input("OPENAI_API_KEY", "g1")]).unwrap();
        assert!(!format!("{plan:?}").contains(SECRET));
    }

    // ── resolve (value retrieval) ─────────────────────────────────────────────

    #[test]
    fn resolve_injects_value_for_granted_secret() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&secret_claim("ipk_app", "rev1", "OPENAI_API_KEY"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "secret.OPENAI_API_KEY", "g1")
            .unwrap();
        let resolved = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[grant_input("OPENAI_API_KEY", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "OPENAI_API_KEY");
        assert_eq!(resolved[0].value.expose(), SECRET);
    }

    #[test]
    fn resolve_blocks_when_store_value_missing() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&secret_claim("ipk_app", "rev1", "OPENAI_API_KEY"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "secret.OPENAI_API_KEY", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[grant_input("OPENAI_API_KEY", "g1")],
            &FakeValueStore { value: None },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains(ATO_ERR_LAUNCH_CONDITION_SECRET_VALUE_MISSING));
    }

    #[test]
    fn resolve_blocks_when_grant_belongs_to_other_app() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&secret_claim("ipk_app", "rev1", "OPENAI_API_KEY"))
            .unwrap();
        // Grant recorded under a DIFFERENT install profile key.
        db.record_secret_grant_ref("ipk_other", None, "secret.OPENAI_API_KEY", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[grant_input("OPENAI_API_KEY", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains(ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED));
    }

    #[test]
    fn resolve_error_never_includes_raw_value() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&secret_claim("ipk_app", "rev1", "OPENAI_API_KEY"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "secret.OPENAI_API_KEY", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[grant_input("OPENAI_API_KEY", "g1")],
            &FakeValueStore { value: None },
        )
        .unwrap_err();
        assert!(!format!("{err:#}").contains(SECRET));
    }

    #[test]
    fn resolve_no_secret_grant_input_is_noop() {
        let (_d, db) = temp_db();
        // No claims, no grant input → empty, no ledger dependency.
        let resolved = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[LaunchConditionInput {
                kind: LaunchConditionInputKind::State,
                key: "data".to_string(),
                value: LaunchConditionInputValue::Binding("user-data".to_string()),
            }],
            &FakeValueStore { value: None },
        )
        .unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn secret_value_debug_is_redacted() {
        let v = SecretValue::new(SECRET.to_string());
        let rendered = format!("{v:?}");
        assert!(!rendered.contains(SECRET));
        assert!(rendered.contains("redacted"));
        // RuntimeSecretEnv shows the name but redacts the value.
        let env = RuntimeSecretEnv {
            name: "OPENAI_API_KEY".to_string(),
            value: v,
        };
        let rendered = format!("{env:?}");
        assert!(rendered.contains("OPENAI_API_KEY"));
        assert!(!rendered.contains(SECRET));
    }
}
