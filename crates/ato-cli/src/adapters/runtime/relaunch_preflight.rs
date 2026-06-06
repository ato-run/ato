//! Installed-app relaunch preflight (#508).
//!
//! Before relaunching an installed app, read its launch conditions from the
//! Installed-State DB **ledger** (the SOT, #527) — not from the manifest /
//! lockfile — **resolve** the ones that depend on local device facts (host env
//! presence, a confirmed secret grant, a confirmed state binding), then turn the
//! resolved conditions into a typed pass / warn / block decision via
//! [`evaluate_relaunch_admission`]. A blocked decision aborts the launch with
//! [`ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED`]; warnings are logged and the launch
//! continues.
//!
//! Resolution never reads/stores a value: it checks host-env *presence*, a
//! *redacted grant reference*, or a *logical binding reference* — never an env
//! value, secret value, token, or raw host path. The in-memory resolved claims
//! are authoritative for the current launch; durable resolutions are written
//! back best-effort (a persistence failure never blocks the launch).
//!
//! Scope: installed-app launches only (an install profile key + revision is
//! available). `ato run .` / non-installed launches skip this entirely. The
//! preflight runs in the run pipeline *before* the executor, never in the
//! executor or the launch hot path.

use anyhow::{Result, bail};
use capsule_core::installed_state::{
    InstalledStateDb, LaunchConditionInput, RelaunchAdmission, RelaunchAdmissionReason,
    RelaunchResolutionContext, apply_capsule_launch_inputs_to_claims, evaluate_relaunch_admission,
    resolve_relaunch_conditions,
};

use crate::utils::error::ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED;

/// The production resolver probes, backed by the installed-state DB registries.
///
/// - **env**: real host-env presence (`std::env::var_os`), value never read.
/// - **secret grant**: existence-only lookup in `secret_grant_refs` — checks a
///   redacted grant id, never reads/decrypts a secret value or calls the
///   value-returning secret store. A DB error resolves to `false` (conservative
///   unresolved beats fake satisfaction).
/// - **state binding**: existence-only lookup in `state_binding_refs` — checks a
///   logical binding id, never a raw host path. Errors resolve to `false`.
fn production_resolution_context(db: &InstalledStateDb) -> RelaunchResolutionContext {
    let secret_db = db.clone();
    let state_db = db.clone();
    RelaunchResolutionContext {
        env_present: Box::new(|name| std::env::var_os(name).is_some()),
        secret_grant_exists: Box::new(move |grant_ref| {
            secret_db
                .secret_grant_ref_exists(grant_ref)
                .unwrap_or(false)
        }),
        state_binding_exists: Box::new(move |binding_ref| {
            state_db
                .state_binding_ref_exists(binding_ref)
                .unwrap_or(false)
        }),
    }
}

/// Run relaunch preflight for an installed-app launch, gated on the install
/// identity. `identity` is `Some((install_profile_key, install_revision_id))`
/// for an installed-app launch, `None` for `ato run` / non-installed launches
/// (which return `Ok(())` immediately, untouched).
///
/// Best-effort on infrastructure: if the installed-state DB can't be opened, the
/// preflight is skipped (warn + continue) rather than blocking the launch. A
/// genuine unsatisfied required condition still returns a typed error.
pub fn run_relaunch_preflight(
    identity: Option<(&str, &str)>,
    inputs: &[LaunchConditionInput],
) -> Result<()> {
    let Some((install_profile_key, install_revision_id)) = identity else {
        return Ok(());
    };
    let db = match InstalledStateDb::open_default() {
        Ok(db) => db,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "installed-state DB unavailable; skipping relaunch preflight"
            );
            return Ok(());
        }
    };
    let warnings = preflight_installed_relaunch_decision(
        &db,
        install_profile_key,
        Some(install_revision_id),
        inputs,
    )?;
    for warning in &warnings {
        tracing::warn!(
            install_profile_key,
            install_revision_id,
            reason = %warning.describe(),
            "installed relaunch preflight warning"
        );
    }
    Ok(())
}

/// Overlay `capsule://` query `inputs`, resolve against the **production** probes,
/// and return the admission decision. The inputs are **not** proofs — the
/// resolver still confirms grant/binding existence against the DB registries; an
/// absent grant/binding stays blocked. Pass `&[]` for no query inputs.
pub fn preflight_installed_relaunch_decision(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    inputs: &[LaunchConditionInput],
) -> Result<Vec<RelaunchAdmissionReason>> {
    preflight_installed_relaunch_decision_with_resolver(
        db,
        install_profile_key,
        install_revision_id,
        &production_resolution_context(db),
        inputs,
    )
}

/// Core seam: load the ledger for `(install_profile_key, install_revision_id)`,
/// **overlay** the `capsule://` query inputs, **resolve** against the given
/// local-fact probes, best-effort persist the durable resolutions, then evaluate
/// admission and either return the (non-blocking) warnings or fail with a typed
/// error. Takes the DB, resolver context, and inputs explicitly so it is testable
/// with a temporary ledger, injected probes, and parsed inputs.
pub fn preflight_installed_relaunch_decision_with_resolver(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    resolver: &RelaunchResolutionContext,
    inputs: &[LaunchConditionInput],
) -> Result<Vec<RelaunchAdmissionReason>> {
    preflight_with_persist(
        db,
        install_profile_key,
        install_revision_id,
        resolver,
        inputs,
        |claims| {
            db.record_resolved_launch_conditions(
                install_profile_key,
                install_revision_id,
                None,
                claims,
            )
            .map_err(anyhow::Error::from)
        },
    )
}

/// As [`preflight_installed_relaunch_decision_with_resolver`], but
/// with the durable write-through injected so the best-effort persistence path is
/// deterministically testable. The write-through is **best-effort**: a `persist`
/// error is logged and the launch continues on the in-memory resolved claims.
fn preflight_with_persist(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    resolver: &RelaunchResolutionContext,
    inputs: &[LaunchConditionInput],
    persist: impl FnOnce(&[capsule_core::installed_state::LaunchConditionClaim]) -> Result<()>,
) -> Result<Vec<RelaunchAdmissionReason>> {
    let mut input =
        db.load_relaunch_admission_input(install_profile_key, install_revision_id, None)?;
    // Overlay the capsule:// query inputs onto the in-memory claims before
    // resolution. These select grants/bindings to try; they are not proofs (the
    // resolver still confirms existence against the DB registries) and never
    // write the registries.
    input.claims = apply_capsule_launch_inputs_to_claims(&input.claims, inputs)
        .map_err(anyhow::Error::from)?;
    let resolution = resolve_relaunch_conditions(input.into(), resolver);

    // Best-effort write-through of *durable* resolutions only (transient host-env
    // presence is excluded). The in-memory resolved claims remain authoritative
    // for this launch, so a persistence failure warns and continues.
    if let Some(persist_claims) = resolution.durable_persist_claims() {
        if let Err(err) = persist(&persist_claims) {
            tracing::warn!(
                error = %err,
                install_profile_key,
                "failed to persist resolved launch conditions; \
                 using in-memory resolution for this launch"
            );
        }
    }
    for update in &resolution.updates {
        tracing::debug!(
            kind = update.kind.as_str(),
            condition_key = %update.condition_key,
            source = ?update.source,
            "resolved installed launch condition"
        );
    }

    match evaluate_relaunch_admission(resolution.to_admission_input()) {
        RelaunchAdmission::Admitted { warnings } => Ok(warnings),
        RelaunchAdmission::Blocked { reasons, .. } => bail!(
            "{}",
            relaunch_blocked_message(install_profile_key, install_revision_id, &reasons)
        ),
    }
}

/// Build the typed block error message. Carries condition *keys* (names) only —
/// never any secret value.
fn relaunch_blocked_message(
    install_profile_key: &str,
    install_revision_id: Option<&str>,
    reasons: &[RelaunchAdmissionReason],
) -> String {
    let lines = reasons
        .iter()
        .map(|reason| format!("- {}", reason.describe()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{code}: cannot relaunch installed app {ipk} (revision {revision}) — \
         {count} required launch condition(s) not satisfied:\n{lines}\n\
         Resolve the listed conditions, then relaunch.",
        code = ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED,
        ipk = install_profile_key,
        revision = install_revision_id.unwrap_or("?"),
        count = reasons.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::installed_state::{
        InstalledStateDb, LaunchConditionClaim, LaunchConditionKind, LaunchConditionSource,
        LaunchConditionStatus, app_service_endpoint, launch_condition_from_port_declaration,
    };

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = InstalledStateDb::open(dir.path().join("state")).expect("open db");
        (dir, db)
    }

    fn claim(
        kind: LaunchConditionKind,
        condition_key: &str,
        status: LaunchConditionStatus,
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
            detail_json: "{}".to_string(),
            redacted: true,
        }
    }

    fn record(db: &InstalledStateDb, claims: &[LaunchConditionClaim]) {
        db.record_installed_launch_ledger("ipk_app", Some("rev1"), None, claims)
            .expect("record ledger");
    }

    #[test]
    fn installed_relaunch_preflight_blocks_on_secret_user_grant_required() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
            )],
        );
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect_err("must block");
        let msg = err.to_string();
        assert!(msg.contains(ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED));
        assert!(msg.contains("OPENAI_API_KEY"));
        assert!(msg.contains("requires user grant"));
        // No secret value can appear (we only ever recorded the name).
        assert!(!msg.contains("sk-"));
    }

    #[test]
    fn installed_relaunch_preflight_blocks_on_explicit_state_user_grant_required() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
            )],
        );
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect_err("explicit state binding must block");
        assert!(err.to_string().contains("state data requires user grant"));
    }

    #[test]
    fn installed_relaunch_preflight_allows_unknown_port_condition() {
        let (_d, db) = temp_db();
        let endpoint = app_service_endpoint("ipk_app", "main");
        let mut port = launch_condition_from_port_declaration(
            "ipk_app",
            Some("rev1"),
            &endpoint,
            "tcp",
            Some(3000),
            "manifest.targets.port",
            Some("remap"),
            LaunchConditionStatus::Unknown,
        );
        port.install_revision_id = Some("rev1".to_string());
        record(&db, &[port]);
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect("unknown port must not block");
        // The port surfaces as a (non-blocking) Unknown warning.
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, RelaunchAdmissionReason::Unknown { .. }))
        );
    }

    #[test]
    fn installed_relaunch_preflight_warns_but_continues_when_ledger_missing() {
        let (_d, db) = temp_db();
        // No ledger recorded for this revision → LedgerMissing warning, not block.
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect("missing ledger must not block");
        assert_eq!(warnings, vec![RelaunchAdmissionReason::LedgerMissing]);
    }

    #[test]
    fn non_installed_run_skips_relaunch_ledger_preflight() {
        // No install identity → no DB read, no preflight, immediate Ok.
        assert!(run_relaunch_preflight(None, &[]).is_ok());
    }

    // ── Resolver-driven preflight (#508) ─────────────────────────────────────

    use capsule_core::installed_state::RelaunchResolutionContext;

    fn resolver(env: bool, grant: bool, binding: bool) -> RelaunchResolutionContext {
        RelaunchResolutionContext {
            env_present: Box::new(move |_| env),
            secret_grant_exists: Box::new(move |_| grant),
            state_binding_exists: Box::new(move |_| binding),
        }
    }

    fn claim_detail(
        kind: LaunchConditionKind,
        condition_key: &str,
        status: LaunchConditionStatus,
        detail_json: &str,
    ) -> LaunchConditionClaim {
        let mut c = claim(kind, condition_key, status);
        c.detail_json = detail_json.to_string();
        c
    }

    #[test]
    fn installed_relaunch_preflight_allows_secret_when_grant_probe_satisfies() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"projection":"env","grant_ref":"ato-secret://store/openai"}"#,
            )],
        );
        let warnings = preflight_installed_relaunch_decision_with_resolver(
            &db,
            "ipk_app",
            Some("rev1"),
            &resolver(false, true, false),
            &[],
        )
        .expect("a confirmed grant must lift the secret to Satisfied → admit");
        assert!(
            warnings
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::UserGrantRequired { .. }))
        );
        // The durable resolution is written back: a reload shows Satisfied.
        let reloaded = db.list_launch_condition_claims("ipk_app").unwrap();
        let secret = reloaded
            .iter()
            .find(|c| c.condition_key == "OPENAI_API_KEY")
            .unwrap();
        assert_eq!(secret.status, LaunchConditionStatus::Satisfied);
    }

    #[test]
    fn installed_relaunch_preflight_still_blocks_secret_without_grant() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"grant_ref":"ato-secret://store/openai"}"#,
            )],
        );
        // Grant ref present but the probe says it does not exist → still blocks.
        let err = preflight_installed_relaunch_decision_with_resolver(
            &db,
            "ipk_app",
            Some("rev1"),
            &resolver(false, false, false),
            &[],
        )
        .expect_err("no confirmed grant → block");
        assert!(
            err.to_string()
                .contains(ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED)
        );
    }

    #[test]
    fn installed_relaunch_preflight_allows_explicit_state_when_binding_probe_satisfies() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"binding_ref":"ato-state://app/data","durability":"persistent"}"#,
            )],
        );
        let warnings = preflight_installed_relaunch_decision_with_resolver(
            &db,
            "ipk_app",
            Some("rev1"),
            &resolver(false, false, true),
            &[],
        )
        .expect("a confirmed binding must lift the state to Satisfied → admit");
        assert!(
            warnings
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::UserGrantRequired { .. }))
        );
    }

    #[test]
    fn installed_relaunch_preflight_env_present_lifts_unknown_to_satisfied() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Env,
                "DATABASE_URL",
                LaunchConditionStatus::Unknown,
                r#"{"source":"manifest.required_env"}"#,
            )],
        );
        let warnings = preflight_installed_relaunch_decision_with_resolver(
            &db,
            "ipk_app",
            Some("rev1"),
            &resolver(true, false, false),
            &[],
        )
        .expect("present host env lifts Unknown → Satisfied");
        assert!(
            warnings
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::Unknown { .. })),
            "resolved env must not surface as an Unknown warning"
        );
        // Transient env presence must NOT be persisted (would fake-satisfy a
        // future launch where the env is absent).
        let reloaded = db.list_launch_condition_claims("ipk_app").unwrap();
        let env = reloaded
            .iter()
            .find(|c| c.condition_key == "DATABASE_URL")
            .unwrap();
        assert_eq!(
            env.status,
            LaunchConditionStatus::Unknown,
            "transient env-presence resolution must not be persisted"
        );
    }

    #[test]
    fn installed_relaunch_preflight_unknown_port_still_warns_not_blocks() {
        let (_d, db) = temp_db();
        let endpoint = app_service_endpoint("ipk_app", "main");
        let mut port = launch_condition_from_port_declaration(
            "ipk_app",
            Some("rev1"),
            &endpoint,
            "tcp",
            Some(3000),
            "manifest.targets.port",
            Some("remap"),
            LaunchConditionStatus::Unknown,
        );
        port.install_revision_id = Some("rev1".to_string());
        record(&db, &[port]);
        // Even with all probes generous, the port is never resolved this slice.
        let warnings = preflight_installed_relaunch_decision_with_resolver(
            &db,
            "ipk_app",
            Some("rev1"),
            &resolver(true, true, true),
            &[],
        )
        .expect("unknown port stays a warning, never a block");
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, RelaunchAdmissionReason::Unknown { .. }))
        );
    }

    #[test]
    fn resolver_writeback_failure_warns_but_uses_in_memory_resolution() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"grant_ref":"ato-secret://store/openai"}"#,
            )],
        );
        // The grant is confirmed (durable resolution) but persistence fails. The
        // launch must still be admitted on the in-memory resolution.
        let result = preflight_with_persist(
            &db,
            "ipk_app",
            Some("rev1"),
            &resolver(false, true, false),
            &[],
            |_claims| anyhow::bail!("simulated write-through failure"),
        );
        assert!(
            result.is_ok(),
            "a persistence failure must not block the launch: {result:?}"
        );
    }

    // ── Production probes are DB-backed (no longer constant false) ───────────

    #[test]
    fn production_secret_probe_lifts_user_grant_when_grant_recorded() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"projection":"env","grant_ref":"openai-default"}"#,
            )],
        );
        // Existence-only metadata row — no secret value involved.
        db.record_secret_grant_ref(
            "ipk_app",
            Some("ato.run/koh0920/hello"),
            "secret.OPENAI_API_KEY",
            "openai-default",
        )
        .unwrap();
        // The PRODUCTION decision (DB-backed probe) must now admit.
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect("recorded grant lifts the secret → admit");
        assert!(
            warnings
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::UserGrantRequired { .. }))
        );
    }

    #[test]
    fn production_secret_probe_keeps_user_grant_when_grant_absent() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"grant_ref":"missing-grant"}"#,
            )],
        );
        // No grant recorded → production probe returns false → still blocks.
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect_err("absent grant must still block");
        assert!(
            err.to_string()
                .contains(ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED)
        );
    }

    #[test]
    fn production_state_probe_lifts_user_grant_when_binding_recorded() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"binding_ref":"user-data","durability":"persistent"}"#,
            )],
        );
        db.record_state_binding_ref(
            "ipk_app",
            Some("ato.run/koh0920/hello"),
            "state.data",
            "data",
            "user-data",
        )
        .unwrap();
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect("recorded binding lifts the state → admit");
        assert!(
            warnings
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::UserGrantRequired { .. }))
        );
    }

    #[test]
    fn secret_grant_probe_uses_metadata_id_not_secret_value() {
        // The grant registry stores only a logical id; presence is decided from
        // that id alone — no secret value is ever recorded or consulted.
        let (_d, db) = temp_db();
        db.record_secret_grant_ref("ipk_app", None, "secret.K", "g1")
            .unwrap();
        assert!(db.secret_grant_ref_exists("g1").unwrap());
        // The stored row carries no secret value (only the id / ref / status).
        assert!(!db.secret_grant_ref_exists("sk-anything").unwrap());
    }

    #[test]
    fn production_secret_probe_does_not_satisfy_raw_token_id_even_if_attempted() {
        let (_d, db) = temp_db();
        // A secret condition whose grant_ref is a raw token. Even if some caller
        // tried to record it, the registry boundary rejects it, so the probe
        // returns false and the condition stays blocked.
        assert!(
            db.record_secret_grant_ref("ipk_app", None, "secret.K", "sk-raw-token")
                .is_err()
        );
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"grant_ref":"sk-raw-token"}"#,
            )],
        );
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect_err("a raw-token grant_ref must never satisfy a secret condition");
        assert!(
            err.to_string()
                .contains(ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED)
        );
    }

    #[test]
    fn production_state_probe_does_not_satisfy_raw_path_id_even_if_attempted() {
        let (_d, db) = temp_db();
        assert!(
            db.record_state_binding_ref("ipk_app", None, "state.data", "data", "/Users/koh/data")
                .is_err()
        );
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"binding_ref":"/Users/koh/data"}"#,
            )],
        );
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &[])
            .expect_err("a raw-path binding_ref must never satisfy a state condition");
        assert!(
            err.to_string()
                .contains(ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED)
        );
    }

    // ── capsule:// query input overlay → preflight (#508) ────────────────────

    use capsule_core::installed_state::parse_capsule_launch_input;

    fn parse_inputs(url: &str) -> Vec<LaunchConditionInput> {
        parse_capsule_launch_input(url)
            .expect("parse query")
            .conditions
    }

    #[test]
    fn capsule_query_secret_grant_input_lifts_when_grant_exists() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"projection":"env","grant_ref":null}"#,
            )],
        );
        db.record_secret_grant_ref(
            "ipk_app",
            Some("ato.run/koh0920/hello"),
            "secret.OPENAI_API_KEY",
            "openai-default",
        )
        .unwrap();
        let inputs = parse_inputs(
            "capsule://ato.run/koh0920/hello?secret.OPENAI_API_KEY=grant:openai-default",
        );
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &inputs)
            .expect("query selects an existing grant → admit");
        assert!(
            warnings
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::UserGrantRequired { .. }))
        );
    }

    #[test]
    fn capsule_query_secret_grant_input_blocks_when_grant_absent() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                "{}",
            )],
        );
        // The query selects grant `missing`, but it is not in the registry.
        let inputs = parse_inputs("capsule://ato.run/x?secret.OPENAI_API_KEY=grant:missing");
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &inputs)
            .expect_err("an absent grant must still block");
        assert!(
            err.to_string()
                .contains(ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED)
        );
    }

    #[test]
    fn capsule_query_state_binding_input_lifts_when_binding_exists() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                r#"{"durability":"persistent"}"#,
            )],
        );
        db.record_state_binding_ref(
            "ipk_app",
            Some("ato.run/x"),
            "state.data",
            "data",
            "user-data",
        )
        .unwrap();
        let inputs = parse_inputs("capsule://ato.run/x?state.data=binding:user-data");
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &inputs)
            .expect("query selects an existing binding → admit");
        assert!(
            warnings
                .iter()
                .all(|w| !matches!(w, RelaunchAdmissionReason::UserGrantRequired { .. }))
        );
    }

    #[test]
    fn capsule_query_state_binding_input_blocks_when_binding_absent() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::State,
                "data",
                LaunchConditionStatus::UserGrantRequired,
                "{}",
            )],
        );
        let inputs = parse_inputs("capsule://ato.run/x?state.data=binding:missing");
        assert!(
            preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &inputs).is_err()
        );
    }

    #[test]
    fn capsule_query_input_does_not_record_grant_or_binding_ref() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
                "{}",
            )],
        );
        let inputs = parse_inputs("capsule://ato.run/x?secret.OPENAI_API_KEY=grant:g1");
        // Blocks (grant absent) AND the query must not have created a registry row.
        let _ = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &inputs);
        assert!(
            !db.secret_grant_ref_exists("g1").unwrap(),
            "a query input must never create a grant registry row"
        );
    }

    #[test]
    fn capsule_query_unknown_condition_errors() {
        let (_d, db) = temp_db();
        record(
            &db,
            &[claim_detail(
                LaunchConditionKind::Secret,
                "OTHER",
                LaunchConditionStatus::UserGrantRequired,
                "{}",
            )],
        );
        // The query references a secret the app does not declare.
        let inputs = parse_inputs("capsule://ato.run/x?secret.NOPE=grant:g1");
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &inputs)
            .expect_err("unknown condition must error");
        assert!(err.to_string().contains("unknown condition"));
    }

    #[test]
    fn capsule_query_port_input_is_ignored_by_relaunch_preflight_for_now() {
        let (_d, db) = temp_db();
        let endpoint = app_service_endpoint("ipk_app", "main");
        let mut port = launch_condition_from_port_declaration(
            "ipk_app",
            Some("rev1"),
            &endpoint,
            "tcp",
            Some(3000),
            "manifest.targets.port",
            Some("remap"),
            LaunchConditionStatus::Unknown,
        );
        port.install_revision_id = Some("rev1".to_string());
        record(&db, &[port]);
        // A port query input is ignored (port stays Unknown → warns, not blocks).
        let inputs = parse_inputs("capsule://ato.run/x?port.main=3001");
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"), &inputs)
            .expect("port input is ignored, never blocks");
        assert!(
            warnings
                .iter()
                .any(|w| matches!(w, RelaunchAdmissionReason::Unknown { .. }))
        );
    }

    #[test]
    fn non_installed_run_with_inputs_still_skips() {
        let inputs = parse_inputs("capsule://ato.run/x?secret.K=grant:g1");
        assert!(run_relaunch_preflight(None, &inputs).is_ok());
    }
}
