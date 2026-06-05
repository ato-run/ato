//! Interactive creation of launch-condition grants/bindings from `capsule://`
//! `=prompt` query inputs (#508).
//!
//! `secret.<name>=prompt` / `state.<key>=prompt` are *requests to be prompted*,
//! not proofs (see #544). This module turns a secret prompt into a **real**
//! grant: it reads a hidden value, writes it to the secure secret store, and —
//! only after that write succeeds — records a `secret_grant_ref` and rewrites the
//! input to `grant:<id>` so the relaunch resolver can admit it against the same
//! DB existence registry as any pre-existing `grant:<id>`.
//!
//! State prompts are intentionally **not** implemented: the only state-binding
//! target store is manifest-coupled, which would violate the no-manifest-reread
//! SOT rule, and writing a bare `state_binding_ref` would forge a proof. A state
//! `=prompt` therefore returns a typed not-implemented error.
//!
//! Security invariants enforced here:
//! - the raw secret value never enters the URL, the launch-condition ledger, the
//!   `secret_grant_refs` registry row, an error message, or a log line;
//! - the registry proof is recorded **only after** the secure value write
//!   succeeds (fail-closed; a `=prompt` alone never satisfies a condition);
//! - nothing reads `capsule.toml`, the manifest, or a lockfile.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use capsule_core::installed_state::{
    InstalledStateDb, LaunchConditionInput, LaunchConditionInputKind, LaunchConditionInputValue,
    plan_launch_condition_prompts,
};

use crate::application::secrets::SecretStore;
use crate::utils::error::{
    ATO_ERR_LAUNCH_CONDITION_PROMPT_REQUIRED_NONINTERACTIVE,
    ATO_ERR_LAUNCH_CONDITION_SECRET_CREATE_FAILED, ATO_ERR_LAUNCH_CONDITION_SECRET_STORE_LOCKED,
    ATO_ERR_LAUNCH_CONDITION_STATE_PROMPT_UNIMPLEMENTED,
};

/// Identity of a secret launch condition that needs an interactive value.
/// Carries no value — only identity.
#[derive(Debug, Clone)]
pub(crate) struct SecretPromptRequest {
    pub install_profile_key: String,
    pub install_revision_id: Option<String>,
    /// The matching ledger claim's condition key (bare or namespaced).
    pub condition_key: String,
    /// The secret name (e.g. `OPENAI_API_KEY`).
    pub input_key: String,
}

/// A user-entered secret value. Deliberately opaque: its `Debug` is redacted and
/// the inner string is only reachable via [`SecretPromptValue::expose`] within
/// this module, so it cannot be accidentally logged or formatted.
pub(crate) struct SecretPromptValue {
    value: String,
}

impl SecretPromptValue {
    pub(crate) fn new(value: String) -> Self {
        Self { value }
    }

    fn expose(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for SecretPromptValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretPromptValue(***redacted***)")
    }
}

/// Seam for obtaining a secret value interactively. Production reads a hidden
/// line; tests inject a fake. The implementation is responsible for the
/// non-interactive policy (it must refuse to prompt and return a typed error).
pub(crate) trait LaunchConditionPromptProvider {
    fn prompt_secret(&self, request: &SecretPromptRequest) -> Result<SecretPromptValue>;
}

/// Seam for persisting a secret value to durable secure storage. Kept separate
/// from the prompt seam so the create ordering (store write → registry proof) is
/// owned and testable at this layer. An `Err` here MUST prevent the registry
/// proof from being recorded.
pub(crate) trait SecretGrantStore {
    fn put_secret(&self, namespace: &str, key: &str, value: &str) -> Result<()>;
}

/// Production prompt provider: hidden stdin input via `rpassword`, refusing in
/// non-interactive mode.
pub(crate) struct CliSecretPromptProvider {
    non_interactive: bool,
}

impl CliSecretPromptProvider {
    pub(crate) fn new(non_interactive: bool) -> Self {
        Self { non_interactive }
    }
}

impl LaunchConditionPromptProvider for CliSecretPromptProvider {
    fn prompt_secret(&self, request: &SecretPromptRequest) -> Result<SecretPromptValue> {
        if self.non_interactive {
            bail!(
                "{code}: launch condition 'secret.{key}' requires an interactive secret prompt, \
                 but this launch is non-interactive. Run interactively, or select an existing grant:\n  \
                 capsule://…?secret.{key}=grant:<id>",
                code = ATO_ERR_LAUNCH_CONDITION_PROMPT_REQUIRED_NONINTERACTIVE,
                key = request.input_key,
            );
        }
        let value = rpassword::prompt_password(format!(
            "Enter secret value for 'secret.{}' (input hidden): ",
            request.input_key
        ))
        .map_err(|e| {
            anyhow::anyhow!(
                "{code}: failed to read secret input for 'secret.{key}': {e}",
                code = ATO_ERR_LAUNCH_CONDITION_SECRET_CREATE_FAILED,
                key = request.input_key,
            )
        })?;
        if value.is_empty() {
            bail!(
                "{code}: no secret value entered for 'secret.{key}'; nothing was stored",
                code = ATO_ERR_LAUNCH_CONDITION_SECRET_CREATE_FAILED,
                key = request.input_key,
            );
        }
        Ok(SecretPromptValue::new(value))
    }
}

/// Production grant store: writes to the age-encrypted [`SecretStore`] under a
/// per-install-profile namespace, distinguishing a locked store from a write
/// failure so the caller surfaces an actionable code.
pub(crate) struct SecretStoreGrantStore;

impl SecretGrantStore for SecretStoreGrantStore {
    fn put_secret(&self, namespace: &str, key: &str, value: &str) -> Result<()> {
        let store = SecretStore::open().context("open secret store")?;
        if store.age().is_none() {
            bail!(
                "{code}: the secret store is locked — run `ato secrets init` to create an \
                 identity, or `ato session start` to unlock it, then relaunch",
                code = ATO_ERR_LAUNCH_CONDITION_SECRET_STORE_LOCKED,
            );
        }
        store
            .set_in_namespace(
                key,
                namespace,
                value,
                Some("created by `ato launch capsule://…?secret.*=prompt`"),
                None,
                None,
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "{code}: failed to write the secret value to the store: {e}",
                    code = ATO_ERR_LAUNCH_CONDITION_SECRET_CREATE_FAILED,
                )
            })
    }
}

/// Derive a stable, scoped secret grant id from the install profile key and the
/// (namespaced) condition key. The id never contains the secret value; it is
/// `grant_<16 hex>` = first 8 bytes of `SHA256(ipk \0 condition_key)`, which is
/// short, path/scheme/token-free, and accepted by `validate_locator_id`. Stable
/// across relaunch for the same app + condition, so re-prompting upserts the
/// same grant and overwrites the same stored value.
fn derive_secret_grant_id(install_profile_key: &str, condition_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(install_profile_key.as_bytes());
    hasher.update([0u8]);
    hasher.update(condition_key.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("grant_{hex}")
}

/// Resolve `=prompt` launch inputs into concrete `grant:<id>` inputs by creating
/// real secret grants, to be called after the installed identity is resolved and
/// before `run_relaunch_preflight`.
///
/// For each `secret.*=prompt` whose ledger claim is unsatisfied: prompt → write
/// the value to the secure store → record the `secret_grant_ref` → rewrite the
/// input to `grant:<id>`. A `secret.*=prompt` whose claim is already satisfied is
/// dropped (no prompt). A `state.*=prompt` returns a typed not-implemented error
/// *before* any secret is created (no partial effects). Non-`prompt` inputs pass
/// through unchanged, and with no `=prompt` input at all this is a no-op that
/// never opens the DB.
///
/// Reads only the installed-state ledger — never `capsule.toml` / manifest /
/// lockfile. Writes a registry row only after the secure value write succeeds.
pub(crate) fn resolve_prompt_launch_inputs(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    capsule_location: Option<&str>,
    inputs: Vec<LaunchConditionInput>,
    provider: &dyn LaunchConditionPromptProvider,
    grant_store: &dyn SecretGrantStore,
) -> Result<Vec<LaunchConditionInput>> {
    // No `=prompt` input → nothing to do (no ledger read, no prompt).
    if !inputs
        .iter()
        .any(|i| i.value == LaunchConditionInputValue::Prompt)
    {
        return Ok(inputs);
    }

    let claims = db
        .load_relaunch_admission_input(install_profile_key, install_revision_id, None)
        .context("load installed-state ledger for launch-condition prompts")?
        .claims;
    // Validates every prompt input (unknown condition → error) and skips claims
    // that are already satisfied.
    let plan = plan_launch_condition_prompts(&claims, &inputs)?;

    // State prompts cannot be honestly created yet — fail before creating any
    // secret so a mixed secret+state launch has no partial side effects.
    if let Some(state_req) = plan
        .iter()
        .find(|r| r.kind == LaunchConditionInputKind::State)
    {
        bail!(
            "{code}: launch condition 'state.{key}' requested a prompt, but state prompt \
             creation is not implemented yet (no manifest-free state-binding target store \
             exists). Select an existing binding instead:\n  \
             capsule://…?state.{key}=binding:<id>",
            code = ATO_ERR_LAUNCH_CONDITION_STATE_PROMPT_UNIMPLEMENTED,
            key = state_req.input_key,
        );
    }

    // Create a real grant per unsatisfied secret prompt: input_key → grant_id.
    let mut created: HashMap<String, String> = HashMap::new();
    for req in &plan {
        // Only Secret remains (State handled above).
        if req.kind != LaunchConditionInputKind::Secret {
            continue;
        }
        let value = provider.prompt_secret(&SecretPromptRequest {
            install_profile_key: install_profile_key.to_string(),
            install_revision_id: install_revision_id.map(str::to_string),
            condition_key: req.condition_key.clone(),
            input_key: req.input_key.clone(),
        })?;

        // The registry validates the condition key as a reserved `secret.*` key.
        let registry_condition_key = format!("secret.{}", req.input_key);
        let grant_id = derive_secret_grant_id(install_profile_key, &registry_condition_key);

        // 1) Secure value write FIRST. On failure, record no registry proof.
        grant_store
            .put_secret(install_profile_key, &req.input_key, value.expose())
            .with_context(|| format!("store secret value for 'secret.{}'", req.input_key))?;
        // The value is no longer needed past the store write.
        drop(value);

        // 2) Only now record the existence proof the resolver checks.
        db.record_secret_grant_ref(
            install_profile_key,
            capsule_location,
            &registry_condition_key,
            &grant_id,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "{code}: stored the secret value but failed to record its grant proof for \
                 'secret.{key}'; relaunch to retry (the stored value is overwritten on retry): {e}",
                code = ATO_ERR_LAUNCH_CONDITION_SECRET_CREATE_FAILED,
                key = req.input_key,
            )
        })?;
        created.insert(req.input_key.clone(), grant_id);
    }

    // Rewrite: a created secret prompt → grant:<id>; a prompt whose condition was
    // already satisfied (not in the plan/created map) is dropped as inert.
    let rewritten = inputs
        .into_iter()
        .filter_map(|input| {
            if input.value != LaunchConditionInputValue::Prompt {
                return Some(input);
            }
            created
                .get(&input.key)
                .map(|grant_id| LaunchConditionInput {
                    kind: input.kind,
                    key: input.key.clone(),
                    value: LaunchConditionInputValue::Grant(grant_id.clone()),
                })
        })
        .collect();
    Ok(rewritten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::installed_state::{
        LaunchConditionClaim, LaunchConditionKind, LaunchConditionSource, LaunchConditionStatus,
    };
    use std::cell::RefCell;

    const SECRET: &str = "sk-super-secret-value-1234567890";

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = InstalledStateDb::open(dir.path().join("state")).unwrap();
        (dir, db)
    }

    fn seed_secret_claim(db: &InstalledStateDb, ipk: &str, rev: &str, condition_key: &str) {
        db.record_launch_condition_claim(&LaunchConditionClaim {
            install_profile_key: ipk.to_string(),
            install_revision_id: Some(rev.to_string()),
            provider_id: None,
            kind: LaunchConditionKind::Secret,
            condition_key: condition_key.to_string(),
            status: LaunchConditionStatus::UserGrantRequired,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: "{}".to_string(),
            redacted: true,
        })
        .unwrap();
    }

    fn seed_state_claim(db: &InstalledStateDb, ipk: &str, rev: &str, condition_key: &str) {
        db.record_launch_condition_claim(&LaunchConditionClaim {
            install_profile_key: ipk.to_string(),
            install_revision_id: Some(rev.to_string()),
            provider_id: None,
            kind: LaunchConditionKind::State,
            condition_key: condition_key.to_string(),
            status: LaunchConditionStatus::UserGrantRequired,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: "{}".to_string(),
            redacted: true,
        })
        .unwrap();
    }

    fn prompt_input(kind: LaunchConditionInputKind, key: &str) -> LaunchConditionInput {
        LaunchConditionInput {
            kind,
            key: key.to_string(),
            value: LaunchConditionInputValue::Prompt,
        }
    }

    /// Fake prompt provider: returns a canned value, records calls, or errors.
    struct FakeProvider {
        value: Option<String>,
        calls: RefCell<Vec<String>>,
    }
    impl FakeProvider {
        fn returning(value: &str) -> Self {
            Self {
                value: Some(value.to_string()),
                calls: RefCell::new(vec![]),
            }
        }
        fn cancelling() -> Self {
            Self {
                value: None,
                calls: RefCell::new(vec![]),
            }
        }
    }
    impl LaunchConditionPromptProvider for FakeProvider {
        fn prompt_secret(&self, request: &SecretPromptRequest) -> Result<SecretPromptValue> {
            self.calls.borrow_mut().push(request.input_key.clone());
            match &self.value {
                Some(v) => Ok(SecretPromptValue::new(v.clone())),
                None => bail!("user cancelled the secret prompt"),
            }
        }
    }

    /// Fake grant store: records `(namespace, key, value)` writes, or fails.
    struct FakeStore {
        fail: bool,
        writes: RefCell<Vec<(String, String, String)>>,
    }
    impl FakeStore {
        fn ok() -> Self {
            Self {
                fail: false,
                writes: RefCell::new(vec![]),
            }
        }
        fn failing() -> Self {
            Self {
                fail: true,
                writes: RefCell::new(vec![]),
            }
        }
    }
    impl SecretGrantStore for FakeStore {
        fn put_secret(&self, namespace: &str, key: &str, value: &str) -> Result<()> {
            if self.fail {
                bail!("secret store write failed (simulated)");
            }
            self.writes.borrow_mut().push((
                namespace.to_string(),
                key.to_string(),
                value.to_string(),
            ));
            Ok(())
        }
    }

    #[test]
    fn secret_prompt_creates_grant_and_rewrites_input() {
        let (_d, db) = temp_db();
        seed_secret_claim(&db, "ipk_app", "rev1", "OPENAI_API_KEY");
        let provider = FakeProvider::returning(SECRET);
        let store = FakeStore::ok();
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            Some("ato.run/acme/app"),
            vec![prompt_input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
            )],
            &provider,
            &store,
        )
        .unwrap();

        assert_eq!(out.len(), 1);
        let grant_id = match &out[0].value {
            LaunchConditionInputValue::Grant(id) => id.clone(),
            other => panic!("expected Grant, got {other:?}"),
        };
        assert!(grant_id.starts_with("grant_"));
        // The created grant proof exists in the registry the resolver checks.
        assert!(db.secret_grant_ref_exists(&grant_id).unwrap());
        // The value was written to the store exactly once, under the ipk namespace.
        let writes = store.writes.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, "ipk_app");
        assert_eq!(writes[0].1, "OPENAI_API_KEY");
        assert_eq!(writes[0].2, SECRET);
    }

    #[test]
    fn secret_prompt_created_grant_then_relaunch_preflight_admits() {
        // After creation, the rewritten input is `grant:<id>` and the grant exists
        // — exactly what the relaunch resolver checks to lift the secret to
        // Satisfied (resolver→admit coverage lives in relaunch_preflight tests).
        let (_d, db) = temp_db();
        seed_secret_claim(&db, "ipk_app", "rev1", "secret.OPENAI_API_KEY");
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
            )],
            &FakeProvider::returning(SECRET),
            &FakeStore::ok(),
        )
        .unwrap();
        let grant_id = match &out[0].value {
            LaunchConditionInputValue::Grant(id) => id.clone(),
            other => panic!("expected Grant, got {other:?}"),
        };
        assert!(db.secret_grant_ref_exists(&grant_id).unwrap());
    }

    #[test]
    fn secret_prompt_does_not_write_registry_when_secret_store_write_fails() {
        let (_d, db) = temp_db();
        seed_secret_claim(&db, "ipk_app", "rev1", "OPENAI_API_KEY");
        let err = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
            )],
            &FakeProvider::returning(SECRET),
            &FakeStore::failing(),
        )
        .unwrap_err();
        // No grant proof was recorded because the secure write failed.
        let grant_id = derive_secret_grant_id("ipk_app", "secret.OPENAI_API_KEY");
        assert!(!db.secret_grant_ref_exists(&grant_id).unwrap());
        // And the raw secret value is not in the error.
        assert!(!format!("{err:#}").contains(SECRET));
    }

    #[test]
    fn secret_prompt_writes_secret_store_before_registry_ref() {
        // If the registry write were attempted before the store write, a store
        // failure would still leave a registry row. It does not.
        let (_d, db) = temp_db();
        seed_secret_claim(&db, "ipk_app", "rev1", "OPENAI_API_KEY");
        let _ = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
            )],
            &FakeProvider::returning(SECRET),
            &FakeStore::failing(),
        );
        let grant_id = derive_secret_grant_id("ipk_app", "secret.OPENAI_API_KEY");
        assert!(
            !db.secret_grant_ref_exists(&grant_id).unwrap(),
            "registry must not be written when the store write fails"
        );
    }

    #[test]
    fn secret_prompt_value_never_appears_in_registry_or_rewritten_input() {
        let (_d, db) = temp_db();
        seed_secret_claim(&db, "ipk_app", "rev1", "OPENAI_API_KEY");
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
            )],
            &FakeProvider::returning(SECRET),
            &FakeStore::ok(),
        )
        .unwrap();
        // Rewritten input carries only the grant id, never the value.
        let rendered = format!("{:?}", out);
        assert!(!rendered.contains(SECRET));
        // The reloaded ledger claim never carries the raw value.
        let claims = db
            .load_relaunch_admission_input("ipk_app", Some("rev1"), None)
            .unwrap()
            .claims;
        assert!(claims.iter().all(|c| !c.detail_json.contains(SECRET)));
    }

    #[test]
    fn secret_prompt_cancel_returns_typed_error() {
        let (_d, db) = temp_db();
        seed_secret_claim(&db, "ipk_app", "rev1", "OPENAI_API_KEY");
        let err = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
            )],
            &FakeProvider::cancelling(),
            &FakeStore::ok(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("cancel"));
        // Nothing was stored or recorded.
        let grant_id = derive_secret_grant_id("ipk_app", "secret.OPENAI_API_KEY");
        assert!(!db.secret_grant_ref_exists(&grant_id).unwrap());
    }

    #[test]
    fn secret_prompt_non_interactive_returns_typed_error() {
        let provider = CliSecretPromptProvider::new(true);
        let err = provider
            .prompt_secret(&SecretPromptRequest {
                install_profile_key: "ipk_app".to_string(),
                install_revision_id: Some("rev1".to_string()),
                condition_key: "OPENAI_API_KEY".to_string(),
                input_key: "OPENAI_API_KEY".to_string(),
            })
            .unwrap_err();
        assert!(
            format!("{err:#}").contains(ATO_ERR_LAUNCH_CONDITION_PROMPT_REQUIRED_NONINTERACTIVE)
        );
    }

    #[test]
    fn secret_prompt_unknown_condition_errors() {
        let (_d, db) = temp_db();
        seed_secret_claim(&db, "ipk_app", "rev1", "OTHER");
        let err = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(
                LaunchConditionInputKind::Secret,
                "OPENAI_API_KEY",
            )],
            &FakeProvider::returning(SECRET),
            &FakeStore::ok(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unknown condition"));
    }

    #[test]
    fn state_prompt_returns_typed_not_implemented_and_writes_nothing() {
        let (_d, db) = temp_db();
        seed_state_claim(&db, "ipk_app", "rev1", "data");
        let store = FakeStore::ok();
        let err = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &FakeProvider::returning(SECRET),
            &store,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains(ATO_ERR_LAUNCH_CONDITION_STATE_PROMPT_UNIMPLEMENTED));
        // No state binding ref written, no secret store write.
        assert!(!db.state_binding_ref_exists("data").unwrap());
        assert!(store.writes.borrow().is_empty());
    }

    #[test]
    fn state_prompt_already_satisfied_skips_not_implemented() {
        let (_d, db) = temp_db();
        // A satisfied state claim is skipped by the planner, so no error and the
        // inert prompt input is dropped.
        db.record_launch_condition_claim(&LaunchConditionClaim {
            install_profile_key: "ipk_app".to_string(),
            install_revision_id: Some("rev1".to_string()),
            provider_id: None,
            kind: LaunchConditionKind::State,
            condition_key: "data".to_string(),
            status: LaunchConditionStatus::Satisfied,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: "{}".to_string(),
            redacted: true,
        })
        .unwrap();
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &FakeProvider::returning(SECRET),
            &FakeStore::ok(),
        )
        .unwrap();
        assert!(out.is_empty(), "satisfied prompt is dropped, not errored");
    }

    #[test]
    fn no_prompt_inputs_is_passthrough_no_op() {
        let (_d, db) = temp_db();
        // No claims seeded; a grant:/required input must pass through untouched
        // without even reading the ledger.
        let inputs = vec![
            LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: "OPENAI_API_KEY".to_string(),
                value: LaunchConditionInputValue::Grant("g1".to_string()),
            },
            LaunchConditionInput {
                kind: LaunchConditionInputKind::State,
                key: "data".to_string(),
                value: LaunchConditionInputValue::Binding("user-data".to_string()),
            },
        ];
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            inputs.clone(),
            &FakeProvider::cancelling(),
            &FakeStore::failing(),
        )
        .unwrap();
        assert_eq!(out, inputs);
    }
}
