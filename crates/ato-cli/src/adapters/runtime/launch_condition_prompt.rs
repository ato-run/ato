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
//! `state.<key>=prompt` is handled the same way against the manifest-free state
//! binding **target store** (#552, #547): it prompts for a local-private
//! directory, writes the target, and — only after that write succeeds — records a
//! `state_binding_ref` and rewrites the input to `binding:<id>` so the relaunch
//! resolver admits it against the same DB existence registry as any pre-existing
//! `binding:<id>`. The raw host path lives only in the local-private target store;
//! it never enters the URL, the ledger, the `state_binding_refs` row, an error, or
//! a log line. `ensure_registered_state_binding` (manifest-coupled) is NOT used.
//!
//! Security invariants enforced here:
//! - the raw secret value / state target path never enters the URL, the
//!   launch-condition ledger, the `secret_grant_refs` / `state_binding_refs`
//!   registry row, an error message, or a log line;
//! - the registry proof is recorded **only after** the secure value / target
//!   write succeeds (fail-closed; a `=prompt` alone never satisfies a condition);
//! - nothing reads `capsule.toml`, the manifest, or a lockfile.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use capsule_core::installed_state::{
    InstalledStateDb, LaunchConditionInput, LaunchConditionInputKind, LaunchConditionInputValue,
    plan_launch_condition_prompts,
};

use crate::application::secrets::SecretStore;
use crate::local_input::expand_local_path;
use crate::utils::error::{
    ATO_ERR_LAUNCH_CONDITION_PROMPT_REQUIRED_NONINTERACTIVE,
    ATO_ERR_LAUNCH_CONDITION_SECRET_CREATE_FAILED, ATO_ERR_LAUNCH_CONDITION_SECRET_STORE_LOCKED,
    ATO_ERR_LAUNCH_CONDITION_STATE_CREATE_FAILED,
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

/// Identity of a state launch condition that needs an interactive target.
/// Carries no value — only identity.
#[derive(Debug, Clone)]
pub(crate) struct StatePromptRequest {
    pub install_profile_key: String,
    pub install_revision_id: Option<String>,
    /// The matching ledger claim's condition key (bare or namespaced).
    pub condition_key: String,
    /// The state key (e.g. `data`).
    pub input_key: String,
}

/// A user-selected local state target path. Deliberately opaque: its `Debug` is
/// redacted and the inner path is only reachable via [`StatePromptValue::expose_path`]
/// within this module (at the target-store write point), so a raw host path
/// cannot be accidentally logged or formatted.
pub(crate) struct StatePromptValue {
    target_path: PathBuf,
}

impl StatePromptValue {
    pub(crate) fn new(target_path: PathBuf) -> Self {
        Self { target_path }
    }

    fn expose_path(&self) -> String {
        self.target_path.to_string_lossy().into_owned()
    }
}

impl std::fmt::Debug for StatePromptValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StatePromptValue(***redacted***)")
    }
}

/// Seam for obtaining a launch-condition value interactively. Production reads a
/// hidden line (secret) or a visible directory path (state); tests inject a fake.
/// The implementation is responsible for the non-interactive policy (it must
/// refuse to prompt and return a typed error).
pub(crate) trait LaunchConditionPromptProvider {
    fn prompt_secret(&self, request: &SecretPromptRequest) -> Result<SecretPromptValue>;

    /// Obtain a local-private state target directory interactively. The returned
    /// path is local-private and must never be logged.
    fn prompt_state_binding(&self, request: &StatePromptRequest) -> Result<StatePromptValue>;
}

/// Seam for persisting a secret value to durable secure storage. Kept separate
/// from the prompt seam so the create ordering (store write → registry proof) is
/// owned and testable at this layer. An `Err` here MUST prevent the registry
/// proof from being recorded.
pub(crate) trait SecretGrantStore {
    fn put_secret(&self, namespace: &str, key: &str, value: &str) -> Result<()>;
}

/// Seam for persisting a state binding **target** to the local-private target
/// store (#552). Kept separate from the prompt seam so the create ordering
/// (target write → registry proof) is owned and testable at this layer. An `Err`
/// here MUST prevent the `state_binding_ref` proof from being recorded.
pub(crate) trait StateBindingTargetStore {
    fn put_target(
        &self,
        binding_id: &str,
        install_profile_key: &str,
        target_path: &str,
    ) -> Result<()>;
}

/// Production prompt provider: hidden stdin input for secrets via `rpassword`, a
/// visible line for state target directories, refusing in non-interactive mode.
pub(crate) struct CliLaunchConditionPromptProvider {
    non_interactive: bool,
}

impl CliLaunchConditionPromptProvider {
    pub(crate) fn new(non_interactive: bool) -> Self {
        Self { non_interactive }
    }
}

impl LaunchConditionPromptProvider for CliLaunchConditionPromptProvider {
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

    fn prompt_state_binding(&self, request: &StatePromptRequest) -> Result<StatePromptValue> {
        if self.non_interactive {
            bail!(
                "{code}: launch condition 'state.{key}' requires an interactive state binding \
                 prompt, but this launch is non-interactive. Run interactively, or select an \
                 existing binding:\n  capsule://…?state.{key}=binding:<id>",
                code = ATO_ERR_LAUNCH_CONDITION_PROMPT_REQUIRED_NONINTERACTIVE,
                key = request.input_key,
            );
        }
        // Visible line (a directory path is not a secret). Prompt on stderr so it
        // never pollutes stdout/JSON. The path is local-private: it is read here
        // and never logged or echoed back in any error below.
        use std::io::Write;
        eprint!(
            "Enter a local directory for state 'state.{}': ",
            request.input_key
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map_err(|e| {
            anyhow::anyhow!(
                "{code}: failed to read the state directory for 'state.{key}': {e}",
                code = ATO_ERR_LAUNCH_CONDITION_STATE_CREATE_FAILED,
                key = request.input_key,
            )
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            bail!(
                "{code}: no directory entered for 'state.{key}'; nothing was bound",
                code = ATO_ERR_LAUNCH_CONDITION_STATE_CREATE_FAILED,
                key = request.input_key,
            );
        }
        // Expand `~` and make absolute lexically (no filesystem side effects; the
        // directory need not already exist — materialization is a later concern).
        let expanded = expand_local_path(trimmed);
        let absolute = std::path::absolute(&expanded).unwrap_or(expanded);
        Ok(StatePromptValue::new(absolute))
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

/// Production target store: records the binding target in the local-private
/// `state_binding_targets` table (#552). The raw path is passed as a bound SQL
/// parameter, so a DB error never echoes it; the wrapper error omits it too.
pub(crate) struct DbStateBindingTargetStore<'a> {
    pub db: &'a InstalledStateDb,
}

impl StateBindingTargetStore for DbStateBindingTargetStore<'_> {
    fn put_target(
        &self,
        binding_id: &str,
        install_profile_key: &str,
        target_path: &str,
    ) -> Result<()> {
        self.db
            .record_state_binding_target(binding_id, install_profile_key, target_path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "{code}: failed to record the state binding target: {e}",
                    code = ATO_ERR_LAUNCH_CONDITION_STATE_CREATE_FAILED,
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

/// Derive a stable, scoped state binding id from the install profile key and the
/// (namespaced) condition key. The id never contains the host path; it is
/// `binding_<16 hex>` = first 8 bytes of `SHA256(ipk \0 condition_key)`, which is
/// short, path/scheme/token-free, and accepted by `validate_locator_id`. Stable
/// across relaunch for the same app + state condition, so re-prompting upserts the
/// same binding and overwrites the same recorded target.
fn derive_state_binding_id(install_profile_key: &str, condition_key: &str) -> String {
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
    format!("binding_{hex}")
}

/// Resolve `=prompt` launch inputs into concrete `grant:<id>` / `binding:<id>`
/// inputs by creating real grants/bindings, to be called after the installed
/// identity is resolved and before `run_relaunch_preflight`.
///
/// For each `secret.*=prompt` whose ledger claim is unsatisfied: prompt → write
/// the value to the secure store → record the `secret_grant_ref` → rewrite to
/// `grant:<id>`. For each `state.*=prompt`: prompt for a local directory → write
/// the local-private target → record the `state_binding_ref` → rewrite to
/// `binding:<id>`. A `*=prompt` whose claim is already satisfied is dropped (no
/// prompt). Non-`prompt` inputs pass through unchanged, and with no `=prompt`
/// input at all this is a no-op that never opens the DB.
///
/// Reads only the installed-state ledger — never `capsule.toml` / manifest /
/// lockfile. A registry proof row is recorded only **after** the secure value /
/// target write succeeds (fail-closed: a `=prompt` alone never satisfies a
/// condition, and a raw secret value / host path never reaches the proof row,
/// the ledger, an error, or a log).
pub(crate) fn resolve_prompt_launch_inputs(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    capsule_location: Option<&str>,
    inputs: Vec<LaunchConditionInput>,
    provider: &dyn LaunchConditionPromptProvider,
    grant_store: &dyn SecretGrantStore,
    target_store: &dyn StateBindingTargetStore,
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

    // Create a real grant/binding per unsatisfied prompt: input_key → the value
    // the input is rewritten to (`grant:<id>` for secrets, `binding:<id>` for state).
    let mut created: HashMap<String, LaunchConditionInputValue> = HashMap::new();
    for req in &plan {
        match req.kind {
            LaunchConditionInputKind::Secret => {
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
                    .with_context(|| {
                        format!("store secret value for 'secret.{}'", req.input_key)
                    })?;
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
                created.insert(
                    req.input_key.clone(),
                    LaunchConditionInputValue::Grant(grant_id),
                );
            }
            LaunchConditionInputKind::State => {
                let target = provider.prompt_state_binding(&StatePromptRequest {
                    install_profile_key: install_profile_key.to_string(),
                    install_revision_id: install_revision_id.map(str::to_string),
                    condition_key: req.condition_key.clone(),
                    input_key: req.input_key.clone(),
                })?;

                // The registry validates the condition key as a reserved `state.*` key.
                let registry_condition_key = format!("state.{}", req.input_key);
                let binding_id =
                    derive_state_binding_id(install_profile_key, &registry_condition_key);

                // 1) Local-private target write FIRST. On failure, record no proof.
                //    The raw path lives only in the target store, never in an error.
                target_store
                    .put_target(&binding_id, install_profile_key, &target.expose_path())
                    .with_context(|| {
                        format!("record state binding target for 'state.{}'", req.input_key)
                    })?;
                // The path is no longer needed past the target store write.
                drop(target);

                // 2) Only now record the existence proof the resolver checks. The
                //    state_key is the bare input key; condition_key is `state.<key>`.
                db.record_state_binding_ref(
                    install_profile_key,
                    capsule_location,
                    &registry_condition_key,
                    &req.input_key,
                    &binding_id,
                )
                .map_err(|e| {
                    anyhow::anyhow!(
                        "{code}: recorded the state target but failed to record its binding proof \
                         for 'state.{key}'; relaunch to retry: {e}",
                        code = ATO_ERR_LAUNCH_CONDITION_STATE_CREATE_FAILED,
                        key = req.input_key,
                    )
                })?;
                created.insert(
                    req.input_key.clone(),
                    LaunchConditionInputValue::Binding(binding_id),
                );
            }
            // The parser only produces `=prompt` for secret/state; any other kind
            // is not prompt-creatable and is left to pass through inert.
            _ => continue,
        }
    }

    // Rewrite: a created prompt → its `grant:<id>` / `binding:<id>`; a prompt whose
    // condition was already satisfied (not in the plan/created map) is dropped as inert.
    let rewritten = inputs
        .into_iter()
        .filter_map(|input| {
            if input.value != LaunchConditionInputValue::Prompt {
                return Some(input);
            }
            created.get(&input.key).map(|value| LaunchConditionInput {
                kind: input.kind,
                key: input.key.clone(),
                value: value.clone(),
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
    const STATE_PATH: &str = "/Users/koh/.local/share/acme/app/data";

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

    /// Fake prompt provider: returns canned secret/state values, records calls,
    /// or errors (cancel). A `None` for the requested kind means "user cancelled".
    struct FakeProvider {
        secret: Option<String>,
        state_path: Option<String>,
        secret_calls: RefCell<Vec<String>>,
        state_calls: RefCell<Vec<String>>,
    }
    impl FakeProvider {
        fn returning(secret: &str) -> Self {
            Self {
                secret: Some(secret.to_string()),
                state_path: None,
                secret_calls: RefCell::new(vec![]),
                state_calls: RefCell::new(vec![]),
            }
        }
        fn returning_state(path: &str) -> Self {
            Self {
                secret: None,
                state_path: Some(path.to_string()),
                secret_calls: RefCell::new(vec![]),
                state_calls: RefCell::new(vec![]),
            }
        }
        fn cancelling() -> Self {
            Self {
                secret: None,
                state_path: None,
                secret_calls: RefCell::new(vec![]),
                state_calls: RefCell::new(vec![]),
            }
        }
    }
    impl LaunchConditionPromptProvider for FakeProvider {
        fn prompt_secret(&self, request: &SecretPromptRequest) -> Result<SecretPromptValue> {
            self.secret_calls
                .borrow_mut()
                .push(request.input_key.clone());
            match &self.secret {
                Some(v) => Ok(SecretPromptValue::new(v.clone())),
                None => bail!("user cancelled the secret prompt"),
            }
        }
        fn prompt_state_binding(&self, request: &StatePromptRequest) -> Result<StatePromptValue> {
            self.state_calls
                .borrow_mut()
                .push(request.input_key.clone());
            match &self.state_path {
                Some(p) => Ok(StatePromptValue::new(PathBuf::from(p))),
                None => bail!("user cancelled the state binding prompt"),
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

    /// Fake target store: records `(binding_id, ipk, target_path)` writes, or fails.
    struct FakeTargetStore {
        fail: bool,
        writes: RefCell<Vec<(String, String, String)>>,
    }
    impl FakeTargetStore {
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
    impl StateBindingTargetStore for FakeTargetStore {
        fn put_target(&self, binding_id: &str, ipk: &str, target_path: &str) -> Result<()> {
            if self.fail {
                bail!("state binding target write failed (simulated)");
            }
            self.writes.borrow_mut().push((
                binding_id.to_string(),
                ipk.to_string(),
                target_path.to_string(),
            ));
            Ok(())
        }
    }

    // ── secret prompt creation (#545) ─────────────────────────────────────────

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
            &FakeTargetStore::ok(),
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
            &FakeTargetStore::ok(),
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
            &FakeTargetStore::ok(),
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
            &FakeTargetStore::ok(),
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
            &FakeTargetStore::ok(),
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
            &FakeTargetStore::ok(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("cancel"));
        // Nothing was stored or recorded.
        let grant_id = derive_secret_grant_id("ipk_app", "secret.OPENAI_API_KEY");
        assert!(!db.secret_grant_ref_exists(&grant_id).unwrap());
    }

    #[test]
    fn secret_prompt_non_interactive_returns_typed_error() {
        let provider = CliLaunchConditionPromptProvider::new(true);
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
            &FakeTargetStore::ok(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unknown condition"));
    }

    // ── state prompt creation (#547) ──────────────────────────────────────────

    #[test]
    fn state_prompt_creates_target_then_ref_and_rewrites_input() {
        let (_d, db) = temp_db();
        seed_state_claim(&db, "ipk_app", "rev1", "data");
        let provider = FakeProvider::returning_state(STATE_PATH);
        // Real target store so the DB target/ref rows are actually written.
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            Some("ato.run/acme/app"),
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &provider,
            &FakeStore::failing(), // never touched: no secret prompt
            &DbStateBindingTargetStore { db: &db },
        )
        .unwrap();

        assert_eq!(out.len(), 1);
        let binding_id = match &out[0].value {
            LaunchConditionInputValue::Binding(id) => id.clone(),
            other => panic!("expected Binding, got {other:?}"),
        };
        assert!(binding_id.starts_with("binding_"));
        // Both the local-private target (value store) and the ref (proof) exist.
        assert!(db.state_binding_target_exists(&binding_id).unwrap());
        assert!(db.state_binding_ref_exists(&binding_id).unwrap());
        // The target store holds the real path.
        let rec = db.read_state_binding_target(&binding_id).unwrap().unwrap();
        assert_eq!(rec.target_path, STATE_PATH);
        // The provider was asked for the state binding exactly once.
        assert_eq!(
            provider.state_calls.borrow().as_slice(),
            &["data".to_string()]
        );
    }

    #[test]
    fn state_prompt_created_binding_then_relaunch_preflight_admits() {
        // After creation the rewritten input is `binding:<id>` and the binding ref
        // exists — exactly what the relaunch resolver checks to lift the state
        // condition to Satisfied (resolver→admit coverage lives in
        // relaunch_resolution tests).
        let (_d, db) = temp_db();
        seed_state_claim(&db, "ipk_app", "rev1", "state.data");
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &FakeProvider::returning_state(STATE_PATH),
            &FakeStore::ok(),
            &DbStateBindingTargetStore { db: &db },
        )
        .unwrap();
        let binding_id = match &out[0].value {
            LaunchConditionInputValue::Binding(id) => id.clone(),
            other => panic!("expected Binding, got {other:?}"),
        };
        assert!(db.state_binding_ref_exists(&binding_id).unwrap());
    }

    #[test]
    fn state_prompt_does_not_write_ref_when_target_write_fails() {
        let (_d, db) = temp_db();
        seed_state_claim(&db, "ipk_app", "rev1", "data");
        let err = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &FakeProvider::returning_state(STATE_PATH),
            &FakeStore::ok(),
            &FakeTargetStore::failing(),
        )
        .unwrap_err();
        // No binding proof recorded because the target write failed (fail-closed).
        let binding_id = derive_state_binding_id("ipk_app", "state.data");
        assert!(!db.state_binding_ref_exists(&binding_id).unwrap());
        assert!(!db.state_binding_target_exists(&binding_id).unwrap());
        // The raw host path is not in the error.
        assert!(!format!("{err:#}").contains(STATE_PATH));
    }

    #[test]
    fn state_prompt_raw_path_only_in_target_store_not_claims_or_input() {
        let (_d, db) = temp_db();
        seed_state_claim(&db, "ipk_app", "rev1", "data");
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &FakeProvider::returning_state(STATE_PATH),
            &FakeStore::ok(),
            &DbStateBindingTargetStore { db: &db },
        )
        .unwrap();
        // The rewritten input carries only the binding id, never the path.
        assert!(
            !format!("{out:?}").contains(STATE_PATH),
            "rewritten input must not contain the raw path"
        );
        // The reloaded ledger claims (the proof ledger) never carry the raw path.
        let claims = db
            .load_relaunch_admission_input("ipk_app", Some("rev1"), None)
            .unwrap()
            .claims;
        assert!(
            claims.iter().all(|c| !c.detail_json.contains(STATE_PATH)),
            "launch_condition_claims must not contain the raw path"
        );
        // The path lives only in the local-private target store.
        // (state_binding_refs has no path column by schema.)
        let binding_id = match &out[0].value {
            LaunchConditionInputValue::Binding(id) => id.clone(),
            other => panic!("expected Binding, got {other:?}"),
        };
        assert_eq!(
            db.read_state_binding_target(&binding_id)
                .unwrap()
                .unwrap()
                .target_path,
            STATE_PATH
        );
    }

    #[test]
    fn state_prompt_non_interactive_returns_typed_error() {
        let provider = CliLaunchConditionPromptProvider::new(true);
        let err = provider
            .prompt_state_binding(&StatePromptRequest {
                install_profile_key: "ipk_app".to_string(),
                install_revision_id: Some("rev1".to_string()),
                condition_key: "state.data".to_string(),
                input_key: "data".to_string(),
            })
            .unwrap_err();
        assert!(
            format!("{err:#}").contains(ATO_ERR_LAUNCH_CONDITION_PROMPT_REQUIRED_NONINTERACTIVE)
        );
    }

    #[test]
    fn state_prompt_cancel_returns_typed_error() {
        let (_d, db) = temp_db();
        seed_state_claim(&db, "ipk_app", "rev1", "data");
        let err = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &FakeProvider::cancelling(),
            &FakeStore::ok(),
            &FakeTargetStore::ok(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("cancel"));
        let binding_id = derive_state_binding_id("ipk_app", "state.data");
        assert!(!db.state_binding_ref_exists(&binding_id).unwrap());
    }

    #[test]
    fn state_prompt_unknown_condition_errors() {
        let (_d, db) = temp_db();
        seed_state_claim(&db, "ipk_app", "rev1", "other");
        let err = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &FakeProvider::returning_state(STATE_PATH),
            &FakeStore::ok(),
            &FakeTargetStore::ok(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unknown condition"));
    }

    #[test]
    fn state_prompt_already_satisfied_skips_creation() {
        let (_d, db) = temp_db();
        // A satisfied state claim is skipped by the planner: no prompt, no
        // creation, and the inert prompt input is dropped.
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
        let provider = FakeProvider::returning_state(STATE_PATH);
        let out = resolve_prompt_launch_inputs(
            &db,
            "ipk_app",
            Some("rev1"),
            None,
            vec![prompt_input(LaunchConditionInputKind::State, "data")],
            &provider,
            &FakeStore::ok(),
            &FakeTargetStore::ok(),
        )
        .unwrap();
        assert!(out.is_empty(), "satisfied prompt is dropped, not created");
        assert!(
            provider.state_calls.borrow().is_empty(),
            "a satisfied claim is never prompted"
        );
    }

    #[test]
    fn binding_id_does_not_include_raw_path() {
        // The id is derived from ipk + condition key only — never the host path.
        let id = derive_state_binding_id("ipk_app", "state.data");
        assert!(id.starts_with("binding_"));
        assert!(!id.contains('/'), "binding id must not contain a path");
        assert!(!id.contains(':'), "binding id must not look like a scheme");
        assert!(!id.contains(STATE_PATH));
    }

    // ── pass-through ──────────────────────────────────────────────────────────

    #[test]
    fn existing_grant_and_binding_inputs_pass_through_unchanged() {
        let (_d, db) = temp_db();
        // No claims seeded; a grant:/binding: input must pass through untouched
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
            &FakeTargetStore::failing(),
        )
        .unwrap();
        assert_eq!(out, inputs);
    }
}
