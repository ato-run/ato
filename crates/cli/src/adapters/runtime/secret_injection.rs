//! Inject SecretStore-backed launch-condition grants into the runtime env during
//! installed relaunch (#508).
//!
//! #545 created real secret grants from `secret.*=prompt` (value → secure store →
//! `secret_grant_ref` → input rewritten to `grant:<id>`). This module consumes an
//! admitted `grant:<id>` at runtime: it reads the value back from the secure store
//! and projects it into the launched process env. #549 extends the same channel to
//! a sensitive `env.<name>=grant:<id>` launch condition — an env-var-shaped grant is
//! injected via the identical SecretStore-backed, receipt-excluded path (only the
//! registry condition namespace differs: `env.K` vs `secret.K`). `env.K=prompt`
//! creation is a follow-up; only `grant:` projects here.
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
use capsule::installed_state::{
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
/// value to inject. Carries **no** secret value. `namespace` is the registry
/// condition namespace the grant was selected under (`secret` for `secret.K`,
/// `env` for a sensitive `env.K`), used only for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecretEnvVar {
    pub name: String,
    pub grant_id: String,
    pub namespace: &'static str,
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
/// Both `secret.<name>=grant:<id>` and a sensitive `env.<name>=grant:<id>` (#549)
/// project this slice — a sensitive env launch condition is injected via the same
/// SecretStore-backed, receipt-excluded channel as `secret.*`. Each must match a
/// ledger claim of the matching kind (`Secret` keyed `secret.K`, or `Env` keyed
/// `env.K`; bare `K` matches either) or it is a typed unknown-condition error. The
/// env var name is the input key. A `=prompt` input that was never rewritten to a
/// grant, and any non-`grant:` input, are ignored (no value to inject). `env.K=prompt`
/// is out of scope (a follow-up); only `grant:` projects here.
pub(crate) fn plan_secret_injection(
    claims: &[LaunchConditionClaim],
    inputs: &[LaunchConditionInput],
) -> Result<SecretInjectionPlan> {
    let mut env = Vec::new();
    for input in inputs {
        // The registry condition key namespace differs by kind: `secret.K` for a
        // Secret input, `env.K` for a sensitive Env input. Both inject via the
        // same SecretStore-backed channel.
        let (claim_kind, namespace) = match input.kind {
            LaunchConditionInputKind::Secret => (LaunchConditionKind::Secret, "secret"),
            LaunchConditionInputKind::Env => (LaunchConditionKind::Env, "env"),
            _ => continue,
        };
        let LaunchConditionInputValue::Grant(grant_id) = &input.value else {
            // `prompt` (not yet rewritten), `required`, literal, etc. carry no grant.
            continue;
        };
        let matched = claims
            .iter()
            .any(|c| c.kind == claim_kind && matches_key(&c.condition_key, namespace, &input.key));
        if !matched {
            return Err(inject_err(format!(
                "launch input references unknown condition '{namespace}.{}'",
                input.key
            )));
        }
        env.push(SecretEnvVar {
            name: input.key.clone(),
            grant_id: grant_id.clone(),
            namespace,
        });
    }
    Ok(SecretInjectionPlan { env })
}

/// Resolve admitted secret grants into concrete `(env name, value)` pairs by
/// reading the secure store, after relaunch preflight admission and before spawn.
///
/// For each planned secret grant: confirm via the registry that the grant belongs
/// to this install profile, is `granted`, AND was minted for THIS exact condition
/// (`secret.K` / `env.K`) — never inject another app's grant, and never let a grant
/// issued for one condition (a different name, or the other namespace) satisfy a
/// different one. Then read its value. A grant that exists but has no stored value
/// blocks the launch with a typed error (grant selection is not value availability).
/// No value ever enters an error message.
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
        // The grant's recorded `condition_key` (`secret.K` for a Secret input,
        // `env.K` for a sensitive Env input — that namespaced form is what
        // `record_secret_grant_ref` validates and stores). A grant is the proof for
        // THIS condition only, so a grant minted for a different env/secret name —
        // or for the *other* namespace at the same name — must not satisfy it.
        let expected_condition_key = format!("{}.{}", entry.namespace, entry.name);
        // Ownership + status + condition: the grant must belong to THIS install
        // profile, be granted, AND match this exact condition. Defence-in-depth —
        // never inject a grant from another app, or a grant for another condition.
        match db.read_secret_grant_ref(&entry.grant_id)? {
            Some(rec)
                if rec.status == "granted"
                    && rec.install_profile_key == install_profile_key
                    && grant_matches_condition(
                        &rec.condition_key,
                        &expected_condition_key,
                        &entry.name,
                    ) => {}
            Some(rec) if rec.install_profile_key != install_profile_key => {
                bail!(
                    "{code}: secret grant for '{expected}' belongs to a different installed \
                     app; refusing to inject",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED,
                    expected = expected_condition_key,
                );
            }
            Some(rec)
                if rec.status == "granted"
                    && !grant_matches_condition(
                        &rec.condition_key,
                        &expected_condition_key,
                        &entry.name,
                    ) =>
            {
                // A grant exists and is owned by this app, but was minted for a
                // different launch condition. The grant is not proof for THIS one.
                bail!(
                    "{code}: the grant for '{expected}' was issued for a different launch \
                     condition; create the grant for '{expected}', then relaunch",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED,
                    expected = expected_condition_key,
                );
            }
            _ => {
                bail!(
                    "{code}: no granted secret for '{expected}'; create the grant, then relaunch",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_VALUE_MISSING,
                    expected = expected_condition_key,
                );
            }
        }

        let value = value_store
            .get_secret(install_profile_key, &entry.name)?
            .ok_or_else(|| {
                inject_err(format!(
                    "{code}: a grant exists for '{expected}' but no value is stored; recreate the \
                     grant to set it",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_VALUE_MISSING,
                    expected = expected_condition_key,
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
    // A sensitive `env.K=grant:<id>` (#549) is injected via the same channel as
    // `secret.K=grant:<id>`, so both kinds trigger resolution.
    matches!(
        input.kind,
        LaunchConditionInputKind::Secret | LaunchConditionInputKind::Env
    ) && matches!(input.value, LaunchConditionInputValue::Grant(_))
}

/// Match a ledger claim key against `<namespace>.<input_key>` (bare or namespaced).
fn matches_key(claim_key: &str, namespace: &str, input_key: &str) -> bool {
    claim_key == input_key || claim_key == format!("{namespace}.{input_key}")
}

/// Does a recorded grant's `condition_key` satisfy the condition we're injecting?
///
/// The grant is the proof for THIS condition only. The canonical recorded form is
/// the namespaced `secret.K` / `env.K` (that is what `record_secret_grant_ref`
/// validates and stores), so the namespaced match is the one that matters: it
/// rejects a grant minted for a different name OR for the other namespace at the
/// same name (`env.MY_TOKEN` must not satisfy `secret.MY_TOKEN`, and vice versa).
/// A bare `K` is accepted for backward compatibility with any pre-namespaced row,
/// but only against the same name — a bare key is namespace-agnostic, so it is the
/// one form that cannot distinguish secret-vs-env; we lean on the namespaced form.
fn grant_matches_condition(
    recorded_condition_key: &str,
    expected_condition_key: &str,
    name: &str,
) -> bool {
    recorded_condition_key == expected_condition_key || recorded_condition_key == name
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::installed_state::{LaunchConditionSource, LaunchConditionStatus};

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

    // #549: a sensitive `env.<name>=grant:<id>` launch condition.
    fn env_claim(ipk: &str, rev: &str, condition_key: &str) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: ipk.to_string(),
            install_revision_id: Some(rev.to_string()),
            provider_id: None,
            kind: LaunchConditionKind::Env,
            condition_key: condition_key.to_string(),
            status: LaunchConditionStatus::Satisfied,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: r#"{"source":"manifest.required_env"}"#.to_string(),
            redacted: true,
        }
    }

    fn env_grant_input(key: &str, grant_id: &str) -> LaunchConditionInput {
        LaunchConditionInput {
            kind: LaunchConditionInputKind::Env,
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
                namespace: "secret",
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

    // ── env-via-grant (#549) ─────────────────────────────────────────────────

    #[test]
    fn env_grant_plan_maps_env_grant_to_env_name() {
        let claims = vec![env_claim("ipk", "rev1", "MY_TOKEN")];
        let plan = plan_secret_injection(&claims, &[env_grant_input("MY_TOKEN", "g1")]).unwrap();
        assert_eq!(
            plan.env,
            vec![SecretEnvVar {
                name: "MY_TOKEN".to_string(),
                grant_id: "g1".to_string(),
                namespace: "env",
            }]
        );
    }

    #[test]
    fn env_grant_plan_matches_namespaced_env_ledger_key() {
        let claims = vec![env_claim("ipk", "rev1", "env.MY_TOKEN")];
        let plan = plan_secret_injection(&claims, &[env_grant_input("MY_TOKEN", "g1")]).unwrap();
        assert_eq!(plan.env.len(), 1);
        assert_eq!(plan.env[0].name, "MY_TOKEN");
        assert_eq!(plan.env[0].namespace, "env");
    }

    #[test]
    fn env_grant_plan_rejects_unknown_env_condition() {
        // An env grant must match an Env claim; a same-named Secret claim is not it.
        let claims = vec![secret_claim("ipk", "rev1", "MY_TOKEN")];
        let err = plan_secret_injection(&claims, &[env_grant_input("MY_TOKEN", "g1")]).unwrap_err();
        assert!(err.to_string().contains("unknown condition 'env.MY_TOKEN'"));
    }

    #[test]
    fn env_grant_injects_secret_env() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&env_claim("ipk_app", "rev1", "MY_TOKEN"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "env.MY_TOKEN", "g1")
            .unwrap();
        let resolved = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[env_grant_input("MY_TOKEN", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "MY_TOKEN");
        assert_eq!(resolved[0].value.expose(), SECRET);
    }

    #[test]
    fn env_grant_uses_receipt_excluded_channel() {
        use crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext;
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&env_claim("ipk_app", "rev1", "MY_TOKEN"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "env.MY_TOKEN", "g1")
            .unwrap();
        let resolved = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[env_grant_input("MY_TOKEN", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap();
        // The resolved env-grant value travels on the dedicated secret_env channel,
        // which is excluded from the receipt/session merged_env observation.
        let ctx = RuntimeLaunchContext::empty().with_secret_env(resolved);
        assert!(
            !ctx.merged_env().contains_key("MY_TOKEN"),
            "env-grant value must not appear in receipt/session merged_env"
        );
        assert!(
            ctx.secret_env().iter().any(|s| s.name == "MY_TOKEN"),
            "env-grant value must travel on the secret_env channel"
        );
    }

    #[test]
    fn env_grant_missing_store_value_blocks() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&env_claim("ipk_app", "rev1", "MY_TOKEN"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "env.MY_TOKEN", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[env_grant_input("MY_TOKEN", "g1")],
            &FakeValueStore { value: None },
        )
        .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains(ATO_ERR_LAUNCH_CONDITION_SECRET_VALUE_MISSING));
        // Error names the env condition and never leaks a value.
        assert!(rendered.contains("env.MY_TOKEN"));
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn env_grant_other_app_grant_blocks() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&env_claim("ipk_app", "rev1", "MY_TOKEN"))
            .unwrap();
        // Grant recorded under a DIFFERENT install profile key.
        db.record_secret_grant_ref("ipk_other", None, "env.MY_TOKEN", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[env_grant_input("MY_TOKEN", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains(ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED));
        assert!(!rendered.contains(SECRET));
    }

    // ── cross-condition grant rejection ───────────────────────────────────────
    // A grant is the proof for ONE launch condition. An owned, `granted` grant whose
    // recorded `condition_key` is for a different name — or for the other namespace
    // at the same name — must NOT satisfy the condition being injected.

    #[test]
    fn secret_grant_for_different_secret_condition_blocks() {
        let (_d, db) = temp_db();
        // The launch input/claim is for OPENAI_API_KEY, but the grant was minted for
        // a DIFFERENT secret condition (secret.OTHER).
        db.record_launch_condition_claim(&secret_claim("ipk_app", "rev1", "OPENAI_API_KEY"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "secret.OTHER", "g1")
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
        let rendered = format!("{err:#}");
        assert!(rendered.contains(ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED));
        assert!(rendered.contains("secret.OPENAI_API_KEY"));
        // The error names the condition we tried to satisfy, never the foreign one
        // and never the value.
        assert!(!rendered.contains("secret.OTHER"));
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn env_grant_for_different_env_condition_blocks() {
        let (_d, db) = temp_db();
        // The launch input/claim is for MY_TOKEN, but the grant was minted for a
        // DIFFERENT env condition (env.OTHER).
        db.record_launch_condition_claim(&env_claim("ipk_app", "rev1", "MY_TOKEN"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "env.OTHER", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[env_grant_input("MY_TOKEN", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains(ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED));
        assert!(rendered.contains("env.MY_TOKEN"));
        assert!(!rendered.contains("env.OTHER"));
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn env_grant_does_not_accept_secret_condition_ref() {
        let (_d, db) = temp_db();
        // The env claim is for MY_TOKEN; the grant carries a same-named SECRET
        // condition (secret.MY_TOKEN). A secret grant is not proof for an env one.
        db.record_launch_condition_claim(&env_claim("ipk_app", "rev1", "MY_TOKEN"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "secret.MY_TOKEN", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[env_grant_input("MY_TOKEN", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains(ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED));
        assert!(rendered.contains("env.MY_TOKEN"));
        assert!(!rendered.contains(SECRET));
    }

    #[test]
    fn secret_grant_does_not_accept_env_condition_ref() {
        let (_d, db) = temp_db();
        // The secret claim is for MY_TOKEN; the grant carries a same-named ENV
        // condition (env.MY_TOKEN). An env grant is not proof for a secret one.
        db.record_launch_condition_claim(&secret_claim("ipk_app", "rev1", "MY_TOKEN"))
            .unwrap();
        db.record_secret_grant_ref("ipk_app", None, "env.MY_TOKEN", "g1")
            .unwrap();
        let err = resolve_secret_injection(
            &db,
            "ipk_app",
            Some("rev1"),
            &[grant_input("MY_TOKEN", "g1")],
            &FakeValueStore {
                value: Some(SECRET.to_string()),
            },
        )
        .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains(ATO_ERR_LAUNCH_CONDITION_SECRET_INJECTION_FAILED));
        assert!(rendered.contains("secret.MY_TOKEN"));
        assert!(!rendered.contains(SECRET));
    }
}
