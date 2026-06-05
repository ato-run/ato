//! Installed-app relaunch preflight (#508).
//!
//! Before relaunching an installed app, read its launch conditions from the
//! Installed-State DB **ledger** (the SOT, #527) — not from the manifest /
//! lockfile — and turn them into a typed pass / warn / block decision via
//! [`evaluate_relaunch_admission`]. A blocked decision aborts the launch with
//! [`ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED`]; warnings are logged and the launch
//! continues.
//!
//! Scope: installed-app launches only (an install profile key + revision is
//! available). `ato run .` / non-installed launches skip this entirely. The
//! preflight runs in the run pipeline *before* the executor, never in the
//! executor or the launch hot path.

use anyhow::{Result, bail};
use capsule_core::installed_state::{
    InstalledStateDb, RelaunchAdmission, RelaunchAdmissionReason, evaluate_relaunch_admission,
};

use crate::utils::error::ATO_ERR_RELAUNCH_CONDITION_UNSATISFIED;

/// Run relaunch preflight for an installed-app launch, gated on the install
/// identity. `identity` is `Some((install_profile_key, install_revision_id))`
/// for an installed-app launch, `None` for `ato run` / non-installed launches
/// (which return `Ok(())` immediately, untouched).
///
/// Best-effort on infrastructure: if the installed-state DB can't be opened, the
/// preflight is skipped (warn + continue) rather than blocking the launch. A
/// genuine unsatisfied required condition still returns a typed error.
pub fn run_relaunch_preflight(identity: Option<(&str, &str)>) -> Result<()> {
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
    let warnings =
        preflight_installed_relaunch_decision(&db, install_profile_key, Some(install_revision_id))?;
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

/// Core seam: load the ledger for `(install_profile_key, install_revision_id)`,
/// evaluate relaunch admission, and either return the (non-blocking) warnings or
/// fail with a typed error. Takes the DB explicitly so it is testable with a
/// temporary ledger.
pub fn preflight_installed_relaunch_decision(
    db: &InstalledStateDb,
    install_profile_key: &str,
    install_revision_id: Option<&str>,
) -> Result<Vec<RelaunchAdmissionReason>> {
    let input = db.load_relaunch_admission_input(install_profile_key, install_revision_id, None)?;
    match evaluate_relaunch_admission(input) {
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
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"))
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
        let err = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"))
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
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"))
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
        let warnings = preflight_installed_relaunch_decision(&db, "ipk_app", Some("rev1"))
            .expect("missing ledger must not block");
        assert_eq!(warnings, vec![RelaunchAdmissionReason::LedgerMissing]);
    }

    #[test]
    fn non_installed_run_skips_relaunch_ledger_preflight() {
        // No install identity → no DB read, no preflight, immediate Ok.
        assert!(run_relaunch_preflight(None).is_ok());
    }
}
