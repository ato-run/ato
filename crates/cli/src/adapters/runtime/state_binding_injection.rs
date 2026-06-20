//! Materialize an admitted state binding into the runtime as a mount during
//! installed relaunch (#508).
//!
//! #547 created real state bindings from `state.*=prompt` (a chosen target dir →
//! `state_binding_target` row carrying the local-private host path + a logical
//! `state_binding_ref` proof → the input rewritten to `binding:<id>`). The
//! relaunch preflight (#552/#555) then *admits* a `state.K=binding:<id>` input by
//! confirming the **ref** exists — but admission alone never reads the bound
//! target or mounts it. This module closes that gap: it consumes an admitted
//! `binding:<id>` at runtime, reads the bound target back, and projects it onto a
//! dedicated, receipt-excluded mount channel that reaches the spawned
//! process/container.
//!
//! Two security properties hold here, mirroring secret injection:
//! - **Binding existence is not target availability.** A `state_binding_ref`
//!   proves a binding was selected; the concrete `target_path` must still exist in
//!   `state_binding_targets`. A missing target blocks the launch with a typed,
//!   actionable error — it is never silently skipped.
//! - **Raw host paths are runtime-only and redacted.** The bound `target_path`
//!   travels on the dedicated [`RuntimeStateBindingMount`] channel, whose `Debug`
//!   redacts the source path. That channel is excluded from the receipt/session
//!   mount + env observation, so a raw host path never reaches the execution
//!   receipt, session record, logs, or an error message.
//!
//! The **guest** mount target (e.g. `/app/data`) comes from the state launch
//! condition claim's `mount_target` detail, recorded at install time from the
//! manifest's `services.main.state_bindings[].target` (#528). Nothing here reads
//! `capsule.toml`, the manifest, or a lockfile — the installed-state ledger and
//! the state-binding-target store are the source of truth.
//!
//! [`RuntimeLaunchContext`]: crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use capsule::installed_state::{
    InstalledStateDb, LaunchConditionClaim, LaunchConditionInput, LaunchConditionInputKind,
    LaunchConditionInputValue, LaunchConditionKind,
};

use crate::utils::error::{
    ATO_ERR_LAUNCH_CONDITION_STATE_MOUNT_FAILED, ATO_ERR_LAUNCH_CONDITION_STATE_TARGET_MISSING,
};

/// A resolved state-binding mount bound for the runtime. The `source` is the raw,
/// local-private host path the binding resolves to; its `Debug` is redacted so the
/// path can never leak via `{:?}` (mirrors `SecretValue` / `StateBindingTargetRecord`).
/// The `target` is the guest-side mount path (non-sensitive).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RuntimeStateBindingMount {
    /// Logical state key (the launch-condition key, e.g. `data`). Non-sensitive.
    pub state_key: String,
    /// Logical binding id (e.g. `user-data`). Non-sensitive.
    pub binding_id: String,
    /// Raw, local-private host path the binding materializes to. Redacted in Debug.
    pub source: PathBuf,
    /// Guest-side mount target (e.g. `/app/data`). Non-sensitive.
    pub target: String,
    /// Whether the mount is read-only.
    pub readonly: bool,
}

// Redact the raw host `source` from Debug: `{:?}` must never leak the bound host
// path. The guest target / state key / binding id are non-sensitive and shown.
impl std::fmt::Debug for RuntimeStateBindingMount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeStateBindingMount")
            .field("state_key", &self.state_key)
            .field("binding_id", &self.binding_id)
            .field("source", &"***redacted***")
            .field("target", &self.target)
            .field("readonly", &self.readonly)
            .finish()
    }
}

/// A single planned state-binding materialization — state key + the binding id
/// whose target to mount + the guest mount target. Carries **no** host path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StateBindingMount {
    pub state_key: String,
    pub binding_id: String,
    pub mount_target: String,
}

/// The pure plan: which state bindings project to which guest mount targets.
/// Host-path-free (the host path is read only in the resolve step).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct StateBindingMaterializationPlan {
    pub mounts: Vec<StateBindingMount>,
}

fn state_err(msg: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(msg.into())
}

/// Plan state-binding materializations from admitted launch inputs (pure,
/// host-path-free).
///
/// Only `state.<key>=binding:<id>` inputs project this slice (`=prompt`,
/// `use-existing`, and `required` carry no admitted binding). Each must match a
/// `State` ledger claim (bare `K` or namespaced `state.K`) or it is a typed
/// unknown-condition error. The guest mount target is read from the matched
/// claim's `mount_target` detail; a state claim with no recorded `mount_target`
/// (the state declares no service mount) is skipped — there is nothing to mount.
pub(crate) fn plan_state_binding_materialization(
    claims: &[LaunchConditionClaim],
    inputs: &[LaunchConditionInput],
) -> Result<StateBindingMaterializationPlan> {
    let mut mounts = Vec::new();
    for input in inputs {
        if input.kind != LaunchConditionInputKind::State {
            continue;
        }
        let LaunchConditionInputValue::Binding(binding_id) = &input.value else {
            // `prompt` (not yet rewritten), `use-existing`, `required` carry no
            // admitted binding id.
            continue;
        };
        let matched = claims.iter().find(|c| {
            c.kind == LaunchConditionKind::State
                && matches_key(&c.condition_key, "state", &input.key)
        });
        let Some(matched) = matched else {
            return Err(state_err(format!(
                "launch input references unknown condition 'state.{}'",
                input.key
            )));
        };
        // The guest mount target is recorded in the claim detail at install time
        // (#528). A state with no service mount has none — nothing to materialize.
        let Some(mount_target) = mount_target_from_detail(&matched.detail_json) else {
            continue;
        };
        mounts.push(StateBindingMount {
            state_key: input.key.clone(),
            binding_id: binding_id.clone(),
            mount_target,
        });
    }
    Ok(StateBindingMaterializationPlan { mounts })
}

/// Resolve admitted state bindings into concrete runtime mounts by re-confirming
/// the binding **proof** and then reading the bound target back, after relaunch
/// preflight admission and before spawn.
///
/// The design splits proof from value: `state_binding_refs` is the proof ledger,
/// `state_binding_targets` is the local-private value store (mirroring `refs` vs
/// the SecretStore on the secret side). Mounting on the target row alone would let
/// a half state — a target written without (or after losing) its proof — still
/// mount, so the ref proof is re-checked here first, exactly like
/// `resolve_secret_injection` re-checks `read_secret_grant_ref`.
///
/// For each planned binding, in order:
/// 1. re-confirm the `state_binding_ref` proof: it must exist, belong to **this**
///    install profile, target the same condition (`state.<key>`) and `state_key`,
///    and be `bound` — never mount on a missing, foreign, mismatched, or non-bound
///    proof;
/// 2. read the bound target; an admitted binding whose concrete target was never
///    recorded blocks the launch (binding selection is not target availability);
/// 3. confirm via the target row that it too belongs to this install profile.
///
/// No raw host path ever enters an error message. Short-circuits with no
/// state-binding input (no ledger read). Reads only the installed-state ledger and
/// the state-binding stores — never the manifest/lockfile.
pub(crate) fn resolve_state_binding_materialization(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    inputs: &[LaunchConditionInput],
) -> Result<Vec<RuntimeStateBindingMount>> {
    if !inputs.iter().any(is_state_binding_input) {
        return Ok(Vec::new());
    }

    let claims = db
        .load_relaunch_admission_input(install_profile_key, install_revision_id, None)
        .context("load installed-state ledger for state binding materialization")?
        .claims;
    let plan = plan_state_binding_materialization(&claims, inputs)?;

    let mut resolved = Vec::with_capacity(plan.mounts.len());
    for entry in &plan.mounts {
        // Proof FIRST: re-confirm the `state_binding_ref` before touching the value
        // store. `refs` is the proof ledger; `targets` is the value store. A target
        // row without a matching bound ref is a forged/half state and must not mount
        // (mirrors `resolve_secret_injection`'s `read_secret_grant_ref` re-check).
        let expected_condition_key = format!("state.{}", entry.state_key);
        match db.read_state_binding_ref(&entry.binding_id)? {
            Some(rec)
                if rec.status == "bound"
                    && rec.install_profile_key == install_profile_key
                    && rec.condition_key == expected_condition_key
                    && rec.state_key == entry.state_key => {}
            Some(rec) if rec.install_profile_key != install_profile_key => {
                bail!(
                    "{code}: state binding for 'state.{key}' belongs to a different installed app; \
                     refusing to mount",
                    code = ATO_ERR_LAUNCH_CONDITION_STATE_MOUNT_FAILED,
                    key = entry.state_key,
                );
            }
            _ => {
                // No matching bound proof for this binding/condition. Selection is
                // not proof — block rather than mount on the target row alone.
                bail!(
                    "{code}: no bound state binding for 'state.{key}'; relaunch with \
                     state.{key}=prompt to bind it",
                    code = ATO_ERR_LAUNCH_CONDITION_STATE_TARGET_MISSING,
                    key = entry.state_key,
                );
            }
        }

        // Read the bound target. An admitted binding whose concrete target was
        // never recorded blocks the launch (selection is not availability).
        let record = db
            .read_state_binding_target(&entry.binding_id)?
            .ok_or_else(|| {
                state_err(format!(
                    "{code}: no materialized target for 'state.{key}'; relaunch with \
                     state.{key}=prompt to bind it",
                    code = ATO_ERR_LAUNCH_CONDITION_STATE_TARGET_MISSING,
                    key = entry.state_key,
                ))
            })?;

        // Ownership: the target must belong to THIS install profile. Defence in
        // depth — never mount a directory bound by another installed app. The
        // host path is never named in the error.
        if record.install_profile_key != install_profile_key {
            bail!(
                "{code}: state binding for 'state.{key}' belongs to a different installed app; \
                 refusing to mount",
                code = ATO_ERR_LAUNCH_CONDITION_STATE_MOUNT_FAILED,
                key = entry.state_key,
            );
        }

        resolved.push(RuntimeStateBindingMount {
            state_key: entry.state_key.clone(),
            binding_id: entry.binding_id.clone(),
            source: PathBuf::from(record.target_path),
            target: entry.mount_target.clone(),
            // Persistent state bindings are writable: the app must be able to
            // persist into the bound directory.
            readonly: false,
        });
    }
    Ok(resolved)
}

fn is_state_binding_input(input: &LaunchConditionInput) -> bool {
    input.kind == LaunchConditionInputKind::State
        && matches!(input.value, LaunchConditionInputValue::Binding(_))
}

/// Extract the non-sensitive guest `mount_target` from a state claim's
/// `detail_json`. Returns `None` for malformed JSON or an absent/empty value.
fn mount_target_from_detail(detail_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(detail_json).ok()?;
    let target = value.get("mount_target")?.as_str()?.trim();
    if target.is_empty() {
        None
    } else {
        Some(target.to_string())
    }
}

/// Match a ledger claim key against `<namespace>.<input_key>` (bare or namespaced).
fn matches_key(claim_key: &str, namespace: &str, input_key: &str) -> bool {
    claim_key == input_key || claim_key == format!("{namespace}.{input_key}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::installed_state::{LaunchConditionSource, LaunchConditionStatus};

    const RAW_PATH: &str = "/Users/koh/.local/share/app/data";

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = InstalledStateDb::open(dir.path().join("state")).unwrap();
        (dir, db)
    }

    fn state_claim(
        ipk: &str,
        rev: &str,
        condition_key: &str,
        detail_json: &str,
    ) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: ipk.to_string(),
            install_revision_id: Some(rev.to_string()),
            provider_id: None,
            kind: LaunchConditionKind::State,
            condition_key: condition_key.to_string(),
            status: LaunchConditionStatus::Satisfied,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: detail_json.to_string(),
            redacted: true,
        }
    }

    fn binding_input(key: &str, binding_id: &str) -> LaunchConditionInput {
        LaunchConditionInput {
            kind: LaunchConditionInputKind::State,
            key: key.to_string(),
            value: LaunchConditionInputValue::Binding(binding_id.to_string()),
        }
    }

    // ── pure planner ────────────────────────────────────────────────────────

    #[test]
    fn state_binding_plan_maps_binding_to_guest_target() {
        let claims = vec![state_claim(
            "ipk",
            "rev1",
            "data",
            r#"{"durability":"persistent","mount_target":"/app/data"}"#,
        )];
        let plan =
            plan_state_binding_materialization(&claims, &[binding_input("data", "user-data")])
                .unwrap();
        assert_eq!(
            plan.mounts,
            vec![StateBindingMount {
                state_key: "data".to_string(),
                binding_id: "user-data".to_string(),
                mount_target: "/app/data".to_string(),
            }]
        );
    }

    #[test]
    fn state_binding_plan_matches_namespaced_ledger_key() {
        let claims = vec![state_claim(
            "ipk",
            "rev1",
            "state.data",
            r#"{"mount_target":"/app/data"}"#,
        )];
        let plan =
            plan_state_binding_materialization(&claims, &[binding_input("data", "user-data")])
                .unwrap();
        assert_eq!(plan.mounts.len(), 1);
        assert_eq!(plan.mounts[0].mount_target, "/app/data");
    }

    #[test]
    fn state_binding_plan_skips_state_without_mount_target() {
        // A state with no recorded mount_target has nothing to mount.
        let claims = vec![state_claim(
            "ipk",
            "rev1",
            "data",
            r#"{"durability":"ephemeral"}"#,
        )];
        let plan =
            plan_state_binding_materialization(&claims, &[binding_input("data", "user-data")])
                .unwrap();
        assert!(plan.mounts.is_empty());
    }

    #[test]
    fn state_binding_plan_ignores_prompt_and_non_binding_inputs() {
        let claims = vec![state_claim(
            "ipk",
            "rev1",
            "data",
            r#"{"mount_target":"/app/data"}"#,
        )];
        let inputs = vec![
            LaunchConditionInput {
                kind: LaunchConditionInputKind::State,
                key: "data".to_string(),
                value: LaunchConditionInputValue::Prompt,
            },
            LaunchConditionInput {
                kind: LaunchConditionInputKind::State,
                key: "data".to_string(),
                value: LaunchConditionInputValue::UseExisting,
            },
        ];
        assert!(
            plan_state_binding_materialization(&claims, &inputs)
                .unwrap()
                .mounts
                .is_empty()
        );
    }

    #[test]
    fn state_binding_plan_rejects_unknown_state_condition() {
        let claims = vec![state_claim(
            "ipk",
            "rev1",
            "OTHER",
            r#"{"mount_target":"/x"}"#,
        )];
        let err =
            plan_state_binding_materialization(&claims, &[binding_input("data", "user-data")])
                .unwrap_err();
        assert!(err.to_string().contains("unknown condition"));
    }

    // ── resolve (target retrieval) ────────────────────────────────────────────

    #[test]
    fn state_binding_materialization_reads_target_after_ref() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"durability":"persistent","mount_target":"/app/data"}"#,
        ))
        .unwrap();
        db.record_state_binding_ref("ipk_app", None, "state.data", "data", "user-data")
            .unwrap();
        db.record_state_binding_target("user-data", "ipk_app", RAW_PATH)
            .unwrap();
        let resolved = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].state_key, "data");
        assert_eq!(resolved[0].binding_id, "user-data");
        assert_eq!(resolved[0].target, "/app/data");
        assert_eq!(resolved[0].source, PathBuf::from(RAW_PATH));
        assert!(!resolved[0].readonly);
    }

    #[test]
    fn state_binding_materialization_blocks_when_target_missing() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"mount_target":"/app/data"}"#,
        ))
        .unwrap();
        // The binding proof exists (ref bound for this app/condition) but the
        // local-private target row was never recorded — selection is not
        // availability, so the launch is blocked.
        db.record_state_binding_ref("ipk_app", None, "state.data", "data", "user-data")
            .unwrap();
        let err = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(ATO_ERR_LAUNCH_CONDITION_STATE_TARGET_MISSING));
        // The error never names a raw host path.
        assert!(!msg.contains(RAW_PATH));
    }

    #[test]
    fn state_binding_materialization_blocks_when_ref_missing_even_if_target_exists() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"mount_target":"/app/data"}"#,
        ))
        .unwrap();
        // A target row exists but NO `state_binding_ref` proof was recorded — the
        // exact half state the proof re-check guards against. Mounting on the target
        // alone would forge proof, so the launch must be blocked.
        db.record_state_binding_target("user-data", "ipk_app", RAW_PATH)
            .unwrap();
        let err = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(ATO_ERR_LAUNCH_CONDITION_STATE_TARGET_MISSING),
            "missing proof must block the launch with a typed error"
        );
        // Even though a target row exists, its raw host path must not leak.
        assert!(!msg.contains(RAW_PATH));
    }

    #[test]
    fn state_binding_materialization_requires_ref_and_target() {
        // Only the full proof+value pair mounts: ref bound for this app/condition
        // AND the local-private target recorded. Both present → exactly one mount.
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"durability":"persistent","mount_target":"/app/data"}"#,
        ))
        .unwrap();
        db.record_state_binding_ref("ipk_app", None, "state.data", "data", "user-data")
            .unwrap();
        db.record_state_binding_target("user-data", "ipk_app", RAW_PATH)
            .unwrap();
        let resolved = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].binding_id, "user-data");
        assert_eq!(resolved[0].source, PathBuf::from(RAW_PATH));
    }

    #[test]
    fn state_binding_materialization_blocks_ref_owned_by_other_app() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"mount_target":"/app/data"}"#,
        ))
        .unwrap();
        // The proof ref is bound under a DIFFERENT install profile. The target row
        // is owned by this app, but the proof says it is another app's binding —
        // refuse before even reading the target.
        db.record_state_binding_ref("ipk_other", None, "state.data", "data", "user-data")
            .unwrap();
        db.record_state_binding_target("user-data", "ipk_app", RAW_PATH)
            .unwrap();
        let err = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(ATO_ERR_LAUNCH_CONDITION_STATE_MOUNT_FAILED));
        assert!(!msg.contains(RAW_PATH));
    }

    #[test]
    fn state_binding_materialization_blocks_ref_for_different_condition() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"mount_target":"/app/data"}"#,
        ))
        .unwrap();
        // The proof ref exists for THIS app but binds a DIFFERENT condition
        // (`state.cache`/`cache`), while the input/claim is `state.data`. A proof for
        // one condition must not satisfy another — block the mount.
        db.record_state_binding_ref("ipk_app", None, "state.cache", "cache", "user-data")
            .unwrap();
        db.record_state_binding_target("user-data", "ipk_app", RAW_PATH)
            .unwrap();
        let err = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(ATO_ERR_LAUNCH_CONDITION_STATE_TARGET_MISSING),
            "a proof for a different condition must not satisfy this binding"
        );
        assert!(!msg.contains(RAW_PATH));
    }

    #[test]
    fn state_binding_materialization_blocks_other_app_binding() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"mount_target":"/app/data"}"#,
        ))
        .unwrap();
        // The proof ref passes for this app, but the target row is owned by a
        // DIFFERENT install profile — defence in depth: the target-ownership check
        // refuses even after the proof check.
        db.record_state_binding_ref("ipk_app", None, "state.data", "data", "user-data")
            .unwrap();
        db.record_state_binding_target("user-data", "ipk_other", RAW_PATH)
            .unwrap();
        let err = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(ATO_ERR_LAUNCH_CONDITION_STATE_MOUNT_FAILED));
        assert!(!msg.contains(RAW_PATH));
    }

    #[test]
    fn state_binding_materialization_error_never_includes_raw_path() {
        let (_d, db) = temp_db();
        db.record_launch_condition_claim(&state_claim(
            "ipk_app",
            "rev1",
            "data",
            r#"{"mount_target":"/app/data"}"#,
        ))
        .unwrap();
        db.record_state_binding_ref("ipk_app", None, "state.data", "data", "user-data")
            .unwrap();
        db.record_state_binding_target("user-data", "ipk_other", RAW_PATH)
            .unwrap();
        let err = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[binding_input("data", "user-data")],
        )
        .unwrap_err();
        assert!(!format!("{err:#}").contains(RAW_PATH));
    }

    #[test]
    fn resolve_no_state_binding_input_is_noop() {
        let (_d, db) = temp_db();
        // A secret-grant input only → no state read, no ledger dependency.
        let resolved = resolve_state_binding_materialization(
            &db,
            "ipk_app",
            Some("rev1"),
            &[LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: "OPENAI_API_KEY".to_string(),
                value: LaunchConditionInputValue::Grant("g1".to_string()),
            }],
        )
        .unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn state_binding_mount_debug_redacts_target_path() {
        let mount = RuntimeStateBindingMount {
            state_key: "data".to_string(),
            binding_id: "user-data".to_string(),
            source: PathBuf::from(RAW_PATH),
            target: "/app/data".to_string(),
            readonly: false,
        };
        let rendered = format!("{mount:?}");
        assert!(!rendered.contains(RAW_PATH), "debug leaked the host path");
        assert!(rendered.contains("redacted"));
        // Non-sensitive fields stay visible.
        assert!(rendered.contains("data"));
        assert!(rendered.contains("/app/data"));
    }

    #[test]
    fn state_binding_mount_not_in_receipt_or_session_record() {
        // The state-mount channel must be excluded from every launch-context
        // surface the execution receipt / session record observes: the mount
        // identity is computed over `injected_mounts()` and the env identity over
        // `merged_env*` / `env_permission_keys`. The bound host path lives only on
        // `state_mounts()`, which none of those read.
        use crate::adapters::runtime::executors::launch_context::RuntimeLaunchContext;
        let ctx = RuntimeLaunchContext::empty().with_state_mounts(vec![RuntimeStateBindingMount {
            state_key: "data".to_string(),
            binding_id: "user-data".to_string(),
            source: PathBuf::from(RAW_PATH),
            target: "/app/data".to_string(),
            readonly: false,
        }]);
        assert!(
            ctx.injected_mounts().is_empty(),
            "state mount must not be in injected_mounts (receipt filesystem observer)"
        );
        assert!(ctx.merged_env().is_empty());
        assert!(ctx.merged_env_with_origins().is_empty());
        assert!(ctx.env_permission_keys().is_empty());
        // The raw host path must not appear in the context's Debug rendering
        // (the launch context is what may be logged).
        assert!(!format!("{ctx:?}").contains(RAW_PATH));
        // But the mount is reachable on its own channel for the spawn boundary.
        assert_eq!(ctx.state_mounts().len(), 1);
    }
}
