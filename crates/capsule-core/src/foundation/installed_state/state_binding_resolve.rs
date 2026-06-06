//! Non-interactive resolution of a `state.<key>` launch condition from a
//! user-chosen host directory (#404 backend seam).
//!
//! This is the reusable, **non-UI** analogue of ato-cli's interactive
//! `resolve_prompt_launch_inputs` State arm (#547 / #555). A future Desktop
//! folder picker (a follow-up PR) — and the CLI alike — calls this once it has a
//! concrete host path the user selected, to turn a `state.<key>=prompt`-style
//! unresolved condition into a real `binding:<id>` the relaunch resolver admits.
//!
//! Both ato-cli and ato-desktop can call it: ato-desktop depends on
//! capsule-core but **not** on ato-cli, so the resolve seam must live here, not
//! in the CLI.
//!
//! ## Prompt/selection is not proof — write order is load-bearing
//!
//! The chosen path is written to the local-private **target store** FIRST
//! ([`InstalledStateDb::record_state_binding_target`]); only after that write
//! succeeds is the existence **proof** recorded
//! ([`InstalledStateDb::record_state_binding_ref`]). A failure between the two
//! leaves no proof, so a half-resolved binding can never satisfy a condition
//! (fail-closed). This mirrors the CLI create ordering exactly.
//!
//! ## The raw path never leaks
//!
//! `chosen_path` lives **only** in the local-private target store. It never
//! enters the `state_binding_refs` proof row, the launch-condition ledger, a
//! receipt, a log line, an error message, or the returned value — the caller
//! only ever receives the logical `binding_<…>` id. The binding id is derived
//! from the install profile + condition key (SHA256), never from the path.

use sha2::{Digest, Sha256};

use crate::error::{CapsuleError, Result};

use super::db::InstalledStateDb;
use super::launch_input::LaunchConditionInputKind;
use super::launch_input::condition_key_kind;

/// The outcome of resolving a `state.<key>` condition from a chosen host path:
/// the logical binding id and the re-submittable `state.<key>=binding:<id>`
/// launch input string. Neither carries the raw path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStateBinding {
    /// The stable logical binding id (`binding_<16 hex>`). Never a host path.
    pub binding_id: String,
    /// The launch input the caller re-submits to satisfy the condition,
    /// e.g. `state.data=binding:binding_0123456789abcdef`.
    pub launch_input: String,
}

/// Derive a stable, scoped state binding id from the install profile key and the
/// (namespaced) condition key. The id never contains the host path; it is
/// `binding_<16 hex>` = first 8 bytes of `SHA256(install_profile_key \0
/// condition_key)`, which is short, path/scheme/token-free, and accepted by
/// `validate_locator_id`.
///
/// Stable across relaunch for the same app + state condition, so re-resolving
/// the same condition upserts the same binding and overwrites the same recorded
/// target. This mirrors `derive_state_binding_id` in ato-cli's
/// `launch_condition_prompt` (the interactive path) byte-for-byte, so an
/// interactive prompt and a non-interactive resolve of the same condition
/// produce the same binding id.
fn derive_state_binding_id(install_profile_key: &str, condition_key: &str) -> String {
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

/// Resolve a `state.<key>` launch condition from a user-chosen host directory,
/// non-interactively.
///
/// Given the installed identity (`install_profile_key`), the matching ledger
/// claim's `condition_key` (bare `data` or namespaced `state.data`), the bare
/// `state_key`, and the host directory the user selected (`chosen_path`):
///
/// 1. derive a stable `binding_<id>` from `(install_profile_key, state.<key>)`;
/// 2. write the **target** FIRST to the local-private target store;
/// 3. only after that succeeds, record the existence **proof** ref;
/// 4. return the `binding_id` and the `state.<key>=binding:<id>` input string
///    the caller re-submits.
///
/// `capsule_location` is an optional capsule-location label recorded alongside
/// the proof (never a path). It is the same value the install/launch pipeline
/// already threads through; pass `None` when unknown.
///
/// ## Security / invariants
///
/// - The proof ref is recorded **only after** the target write succeeds
///   (prompt/selection is not proof; fail-closed).
/// - `chosen_path` lives only in the local-private target store; it never
///   appears in the proof row, the ledger, the return value, an error, or a log.
/// - `chosen_path` is validated as non-empty. It is intentionally *not* shape-
///   validated as a logical id (it IS a host path); the `binding_id` (a logical
///   id) is shape-validated at the DB boundary, so the key can never be a path.
/// - No manifest / lockfile / `capsule.toml` is read.
pub fn resolve_state_binding_from_path(
    db: &InstalledStateDb,
    install_profile_key: &str,
    condition_key: &str,
    state_key: &str,
    chosen_path: &str,
) -> Result<ResolvedStateBinding> {
    resolve_state_binding_from_path_with_location(
        db,
        install_profile_key,
        None,
        condition_key,
        state_key,
        chosen_path,
    )
}

/// [`resolve_state_binding_from_path`] with an explicit `capsule_location`
/// label recorded on the proof ref. The label is never a path.
pub fn resolve_state_binding_from_path_with_location(
    db: &InstalledStateDb,
    install_profile_key: &str,
    capsule_location: Option<&str>,
    condition_key: &str,
    state_key: &str,
    chosen_path: &str,
) -> Result<ResolvedStateBinding> {
    if install_profile_key.is_empty() {
        return Err(CapsuleError::Runtime(
            "install profile key must not be empty".to_string(),
        ));
    }
    if state_key.is_empty() {
        return Err(CapsuleError::Runtime(
            "state key must not be empty".to_string(),
        ));
    }
    // The chosen path must be present. We deliberately do NOT echo it in the
    // error (it is a local-private host path) — only that it was empty.
    if chosen_path.trim().is_empty() {
        return Err(CapsuleError::Runtime(format!(
            "no directory chosen for 'state.{state_key}'; nothing was bound"
        )));
    }

    // Accept the bare key (`data`) or the namespaced key (`state.data`), but
    // ONLY when it names exactly this `state_key`; a mismatched pair (e.g.
    // `condition_key = "state.other"`, `state_key = "data"`) is rejected so we
    // never silently bind a different state than the caller intends. The result
    // is the canonical `state.<state_key>` form the registry and resolver agree on.
    let registry_condition_key = normalize_state_condition_key(condition_key, state_key)?;

    let binding_id = derive_state_binding_id(install_profile_key, &registry_condition_key);

    // 1) Local-private target write FIRST. On failure, record no proof. The raw
    //    path is passed as a bound SQL parameter and is never surfaced in the
    //    error we wrap it in.
    db.record_state_binding_target(&binding_id, install_profile_key, chosen_path)
        .map_err(|e| {
            CapsuleError::Runtime(format!(
                "failed to record the state binding target for 'state.{state_key}': {e}"
            ))
        })?;

    // 2) Only now record the existence proof the relaunch resolver checks. The
    //    state_key is the bare key; condition_key is the reserved `state.<key>`.
    db.record_state_binding_ref(
        install_profile_key,
        capsule_location,
        &registry_condition_key,
        state_key,
        &binding_id,
    )
    .map_err(|e| {
        CapsuleError::Runtime(format!(
            "recorded the state target but failed to record its binding proof for \
             'state.{state_key}'; re-resolve to retry: {e}"
        ))
    })?;

    let launch_input = format!("{registry_condition_key}=binding:{binding_id}");
    Ok(ResolvedStateBinding {
        binding_id,
        launch_input,
    })
}

/// Normalize a caller-supplied condition key to the canonical `state.<state_key>`
/// form, enforcing that the key refers to *exactly* the given `state_key`.
///
/// The caller passes the install identity, the ledger claim's `condition_key`,
/// and the bare `state_key` independently; nothing upstream guarantees they
/// agree. If we trusted `condition_key` blindly, a mismatched pair (e.g.
/// `condition_key = "state.other"`, `state_key = "data"`) would bind
/// `state.other` while the caller believes it resolved `data` — a silent
/// wrong-binding correctness bug for the Desktop picker. So we require:
///
/// - **bare key** (`data`): must equal `state_key`; canonicalized to
///   `state.<state_key>`;
/// - **namespaced key** (`state.data`): must equal `state.<state_key>` exactly;
/// - anything else (a different state, a non-`state.*` namespace, a path/URI-
///   shaped key) is rejected via `condition_key_kind`'s validation plus the
///   exact-match check, so a malformed or mismatched key can never reach the
///   registry.
///
/// The returned value is always the canonical `state.<state_key>` — the single
/// form the registry, the binding-id derivation, and the relaunch resolver agree
/// on. Errors name only the logical condition/state keys, never the host path.
fn normalize_state_condition_key(condition_key: &str, state_key: &str) -> Result<String> {
    let canonical = format!("state.{state_key}");

    if condition_key.contains('.') {
        // A namespaced key must be a valid reserved `state.*` key (rejects
        // non-`state.*` namespaces, path/URI-shaped keys, forbidden namespaces)
        // AND name exactly this state: `state.<state_key>`.
        match condition_key_kind(condition_key)? {
            LaunchConditionInputKind::State => {}
            other => {
                return Err(CapsuleError::Runtime(format!(
                    "condition key must be a state.* key (got {other:?} kind '{condition_key}')"
                )));
            }
        }
        if condition_key != canonical {
            return Err(CapsuleError::Runtime(format!(
                "condition key '{condition_key}' does not match state key '{state_key}'; \
                 expected '{canonical}'"
            )));
        }
    } else {
        // A bare key must be exactly the state key it claims to resolve. We do
        // not silently accept e.g. `other` for `state.data`.
        if condition_key != state_key {
            return Err(CapsuleError::Runtime(format!(
                "bare condition key '{condition_key}' does not match state key '{state_key}'; \
                 expected '{state_key}' or '{canonical}'"
            )));
        }
    }

    // Validate the canonical form is itself a well-formed `state.*` key (this
    // also rejects an empty state_key reaching here), then return it — the
    // single form everything downstream agrees on.
    match condition_key_kind(&canonical)? {
        LaunchConditionInputKind::State => Ok(canonical),
        other => Err(CapsuleError::Runtime(format!(
            "state key '{state_key}' is not a valid state.* condition key \
             (got {other:?} kind '{canonical}')"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE_PATH: &str = "/Users/koh/.local/share/acme/app/data";

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = InstalledStateDb::open(dir.path().join("state")).unwrap();
        (dir, db)
    }

    #[test]
    fn resolve_state_binding_from_path_writes_target_then_ref() {
        let (_d, db) = temp_db();
        let resolved =
            resolve_state_binding_from_path(&db, "ipk_app", "state.data", "data", STATE_PATH)
                .unwrap();

        assert!(resolved.binding_id.starts_with("binding_"));
        // Both the local-private target (value store) and the ref (proof) exist.
        assert!(
            db.state_binding_target_exists(&resolved.binding_id)
                .unwrap()
        );
        assert!(db.state_binding_ref_exists(&resolved.binding_id).unwrap());
        // The target store holds the real path.
        let rec = db
            .read_state_binding_target(&resolved.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(rec.target_path, STATE_PATH);
        // The re-submittable input names the binding, never the path.
        assert_eq!(
            resolved.launch_input,
            format!("state.data=binding:{}", resolved.binding_id)
        );
        assert!(!resolved.launch_input.contains(STATE_PATH));
    }

    #[test]
    fn resolve_state_binding_from_path_accepts_bare_condition_key() {
        let (_d, db) = temp_db();
        // Caller passes the bare ledger condition key `data`; it is normalized to
        // the reserved `state.data`, producing the same binding id as the
        // namespaced form.
        let bare =
            resolve_state_binding_from_path(&db, "ipk_app", "data", "data", STATE_PATH).unwrap();
        let namespaced = derive_state_binding_id("ipk_app", "state.data");
        assert_eq!(bare.binding_id, namespaced);
        assert_eq!(
            bare.launch_input,
            format!("state.data=binding:{namespaced}")
        );
    }

    #[test]
    fn resolve_state_binding_from_path_is_idempotent_per_condition() {
        let (_d, db) = temp_db();
        // Same condition → same binding id; re-resolving upserts the target.
        let first =
            resolve_state_binding_from_path(&db, "ipk_app", "state.data", "data", STATE_PATH)
                .unwrap();
        let second_path = "/Users/koh/.local/share/acme/app/data-v2";
        let second =
            resolve_state_binding_from_path(&db, "ipk_app", "state.data", "data", second_path)
                .unwrap();
        assert_eq!(first.binding_id, second.binding_id, "binding id is stable");
        // The upsert overwrote the recorded target with the latest path.
        let rec = db
            .read_state_binding_target(&second.binding_id)
            .unwrap()
            .unwrap();
        assert_eq!(rec.target_path, second_path);
        // Still exactly one bound proof for that id.
        assert!(db.state_binding_ref_exists(&second.binding_id).unwrap());
    }

    #[test]
    fn resolve_state_binding_distinct_conditions_get_distinct_ids() {
        let (_d, db) = temp_db();
        let a = resolve_state_binding_from_path(&db, "ipk_app", "state.data", "data", STATE_PATH)
            .unwrap();
        let b = resolve_state_binding_from_path(&db, "ipk_app", "state.cache", "cache", STATE_PATH)
            .unwrap();
        assert_ne!(a.binding_id, b.binding_id);
    }

    #[test]
    fn resolve_state_binding_from_path_does_not_leak_path_in_error() {
        let (_d, db) = temp_db();
        // An empty path is rejected without ever touching the stores, and the
        // error names only the state key, never a path.
        let err = resolve_state_binding_from_path(&db, "ipk_app", "state.data", "data", "   ")
            .unwrap_err();
        let rendered = format!("{err}");
        assert!(rendered.contains("state.data"));
        assert!(!rendered.contains(STATE_PATH));
        // Nothing was recorded (fail-closed).
        let binding_id = derive_state_binding_id("ipk_app", "state.data");
        assert!(!db.state_binding_target_exists(&binding_id).unwrap());
        assert!(!db.state_binding_ref_exists(&binding_id).unwrap());
    }

    #[test]
    fn resolve_state_binding_rejects_non_state_condition_key() {
        let (_d, db) = temp_db();
        // A namespaced key from a different namespace is rejected before any write.
        let err =
            resolve_state_binding_from_path(&db, "ipk_app", "secret.TOKEN", "data", STATE_PATH)
                .unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("state"));
        let binding_id = derive_state_binding_id("ipk_app", "secret.TOKEN");
        assert!(!db.state_binding_target_exists(&binding_id).unwrap());
    }

    #[test]
    fn resolve_state_binding_rejects_condition_key_for_different_state() {
        let (_d, db) = temp_db();
        // A well-formed `state.*` condition key that names a DIFFERENT state than
        // the bare `state_key` the caller is resolving must be rejected — never
        // silently bind `state.other` while the caller believes it bound `data`.
        let err =
            resolve_state_binding_from_path(&db, "ipk_app", "state.other", "data", STATE_PATH)
                .unwrap_err();
        let rendered = format!("{err}");
        // The error names the logical keys, never the host path.
        assert!(rendered.contains("state.other"));
        assert!(rendered.contains("data"));
        assert!(!rendered.contains(STATE_PATH));
        // Nothing was recorded for EITHER state — fail-closed before any write.
        let mismatched_id = derive_state_binding_id("ipk_app", "state.other");
        let claimed_id = derive_state_binding_id("ipk_app", "state.data");
        assert!(!db.state_binding_target_exists(&mismatched_id).unwrap());
        assert!(!db.state_binding_ref_exists(&mismatched_id).unwrap());
        assert!(!db.state_binding_target_exists(&claimed_id).unwrap());
        assert!(!db.state_binding_ref_exists(&claimed_id).unwrap());
    }

    #[test]
    fn resolve_state_binding_accepts_bare_matching_state_key() {
        let (_d, db) = temp_db();
        // A bare condition key that equals the state key resolves, canonicalizing
        // to `state.<state_key>` (same id/input as the namespaced form).
        let resolved =
            resolve_state_binding_from_path(&db, "ipk_app", "data", "data", STATE_PATH).unwrap();
        let canonical_id = derive_state_binding_id("ipk_app", "state.data");
        assert_eq!(resolved.binding_id, canonical_id);
        assert_eq!(
            resolved.launch_input,
            format!("state.data=binding:{canonical_id}")
        );
        assert!(db.state_binding_ref_exists(&resolved.binding_id).unwrap());
        // A bare key that does NOT equal the state key is rejected before any write.
        let err = resolve_state_binding_from_path(&db, "ipk_app", "other", "data", STATE_PATH)
            .unwrap_err();
        let rendered = format!("{err}");
        assert!(rendered.contains("other"));
        assert!(rendered.contains("data"));
        assert!(!rendered.contains(STATE_PATH));
        assert!(
            !db.state_binding_target_exists(&derive_state_binding_id("ipk_app", "state.other"))
                .unwrap()
        );
    }

    #[test]
    fn resolve_state_binding_accepts_namespaced_matching_state_key() {
        let (_d, db) = temp_db();
        // A namespaced condition key that equals `state.<state_key>` resolves and
        // records the canonical binding.
        let resolved =
            resolve_state_binding_from_path(&db, "ipk_app", "state.data", "data", STATE_PATH)
                .unwrap();
        let canonical_id = derive_state_binding_id("ipk_app", "state.data");
        assert_eq!(resolved.binding_id, canonical_id);
        assert_eq!(
            resolved.launch_input,
            format!("state.data=binding:{canonical_id}")
        );
        assert!(db.state_binding_ref_exists(&resolved.binding_id).unwrap());
    }

    #[test]
    fn resolve_state_binding_id_never_contains_the_path() {
        // The id is derived from ipk + condition key only — never the host path.
        let id = derive_state_binding_id("ipk_app", "state.data");
        assert!(id.starts_with("binding_"));
        assert!(!id.contains('/'), "binding id must not contain a path");
        assert!(!id.contains(':'), "binding id must not look like a scheme");
        assert!(!id.contains(STATE_PATH));
    }

    #[test]
    fn resolve_state_binding_proof_row_does_not_carry_the_path() {
        let (_d, db) = temp_db();
        let resolved =
            resolve_state_binding_from_path(&db, "ipk_app", "state.data", "data", STATE_PATH)
                .unwrap();
        // The proof ref carries logical identity only. Its lookup row (read via
        // the relaunch admission input) must not carry the raw path. The path
        // lives only in the local-private target store.
        let input = db
            .load_relaunch_admission_input("ipk_app", None, None)
            .unwrap();
        assert!(
            input
                .claims
                .iter()
                .all(|c| !c.detail_json.contains(STATE_PATH)),
            "no ledger claim may carry the raw path"
        );
        // The path is reachable only through the target store.
        assert_eq!(
            db.read_state_binding_target(&resolved.binding_id)
                .unwrap()
                .unwrap()
                .target_path,
            STATE_PATH
        );
    }
}
