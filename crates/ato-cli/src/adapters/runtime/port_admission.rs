//! Port admission for installed-app launches (#508).
//!
//! Before an installed app's web service binds its port, consult the
//! installed-state port-claim ledger: if the preferred port is taken by a
//! *different* installed endpoint, the conflict policy (default Remap) chooses
//! an alternative, and the resolved port is injected as `PORT`. After a
//! successful launch the claim is recorded so future relaunches / other apps
//! see the reservation.
//!
//! Scope (this slice): only installed-app launches (an `install_profile_key` is
//! available) and the `main` service's `PORT` are affected; `ato run` /
//! non-installed launches keep their existing port resolution untouched. Only
//! the default Remap policy is wired (Prompt/Fail are not surfaced yet).
//!
//! TOCTOU: `os_port_is_free` reports availability at decision time; a later bind
//! can still race. The launch path should retry remap on bind failure — left as
//! a follow-up; this module returns a plan that a retry loop can re-run.

use anyhow::{Result, bail};
use capsule_core::installed_state::{
    ConflictPolicy, InstalledStateDb, PortAdmission, PortClaim, os_port_is_free,
};

/// Resolved port + the claim to record after a successful launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortAdmissionPlan {
    /// The port to inject as `PORT` (the preferred port, or a remap of it).
    pub resolved_port: u16,
    /// The claim to persist once the launch succeeds.
    pub claim: PortClaim,
}

/// Default conflict policy until a per-app policy is configurable.
pub const DEFAULT_PORT_CONFLICT_POLICY: ConflictPolicy = ConflictPolicy::Remap;

/// The logical endpoint string for an installed app's service.
pub fn logical_endpoint(install_profile_key: &str, service_name: &str) -> String {
    format!("ato://app/{install_profile_key}/{service_name}")
}

/// Compute a port admission plan for an installed-app service launch, using the
/// real OS availability probe. `Ok(None)` when admission does not apply (no
/// install identity or no preferred port → existing resolution is left alone).
pub fn plan_port_admission(
    db: &InstalledStateDb,
    install_profile_key: Option<&str>,
    service_name: &str,
    protocol: &str,
    preferred_port: Option<u16>,
    policy: ConflictPolicy,
) -> Result<Option<PortAdmissionPlan>> {
    plan_port_admission_with(
        db,
        install_profile_key,
        service_name,
        protocol,
        preferred_port,
        policy,
        os_port_is_free,
    )
}

/// Like [`plan_port_admission`] but with an injectable OS-availability probe so
/// the decision can be exercised deterministically in tests.
#[allow(clippy::too_many_arguments)]
pub fn plan_port_admission_with(
    db: &InstalledStateDb,
    install_profile_key: Option<&str>,
    service_name: &str,
    protocol: &str,
    preferred_port: Option<u16>,
    policy: ConflictPolicy,
    os_available: impl Fn(u16) -> bool,
) -> Result<Option<PortAdmissionPlan>> {
    let (Some(ipk), Some(preferred)) = (install_profile_key, preferred_port) else {
        return Ok(None);
    };
    if preferred == 0 {
        // Port 0 means "auto-assign", not a concrete port claim. Leave the
        // existing resolution untouched rather than fabricate an invalid claim
        // (a port-0 PortClaim is rejected by the ledger, see #515).
        return Ok(None);
    }
    let endpoint = logical_endpoint(ipk, service_name);
    let decision =
        db.check_port_admission_with(ipk, &endpoint, protocol, preferred, policy, os_available)?;
    match decision {
        PortAdmission::Admitted { port } | PortAdmission::Remapped { port, .. } => {
            Ok(Some(PortAdmissionPlan {
                resolved_port: port,
                claim: PortClaim {
                    install_profile_key: ipk.to_string(),
                    logical_endpoint: endpoint,
                    preferred_port: preferred,
                    last_actual_port: Some(port),
                    protocol: protocol.to_string(),
                    conflict_policy: policy,
                },
            }))
        }
        PortAdmission::Rejected { preferred, policy } => bail!(
            "{code}: cannot bind port {preferred} for {endpoint} ({policy:?} policy, no alternative available)",
            code = crate::utils::error::ATO_ERR_PORT_CONFLICT,
        ),
    }
}

/// Record the planned port claim after a successful launch. Best-effort: a
/// recording failure must not fail an already-launched service.
pub fn record_port_admission_plan(db: &InstalledStateDb, plan: &PortAdmissionPlan) {
    if let Err(err) = db.record_port_claim(&plan.claim) {
        tracing::warn!(
            error = %err,
            endpoint = %plan.claim.logical_endpoint,
            "failed to record port claim after launch"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = InstalledStateDb::open(dir.path().join("state")).expect("open db");
        (dir, db)
    }

    fn claim(ipk: &str, port: u16) -> PortClaim {
        PortClaim {
            install_profile_key: ipk.to_string(),
            logical_endpoint: logical_endpoint(ipk, "main"),
            preferred_port: port,
            last_actual_port: None,
            protocol: "tcp".to_string(),
            conflict_policy: ConflictPolicy::Remap,
        }
    }

    #[test]
    fn no_install_identity_skips_admission() {
        let (_d, db) = temp_db();
        let plan = plan_port_admission_with(
            &db,
            None,
            "main",
            "tcp",
            Some(3000),
            ConflictPolicy::Remap,
            |_| true,
        )
        .unwrap();
        assert!(plan.is_none(), "non-installed launch must be untouched");
    }

    #[test]
    fn no_preferred_port_skips_admission() {
        let (_d, db) = temp_db();
        let plan = plan_port_admission_with(
            &db,
            Some("ipk_a"),
            "main",
            "tcp",
            None,
            ConflictPolicy::Remap,
            |_| true,
        )
        .unwrap();
        assert!(plan.is_none());
    }

    #[test]
    fn port_zero_skips_admission() {
        let (_d, db) = temp_db();
        // Port 0 ("auto-assign") must not produce an invalid (port-0) claim plan.
        let plan = plan_port_admission_with(
            &db,
            Some("ipk_a"),
            "main",
            "tcp",
            Some(0),
            ConflictPolicy::Remap,
            |_| true,
        )
        .unwrap();
        assert!(
            plan.is_none(),
            "port 0 is auto-assign, not a concrete claim"
        );
    }

    #[test]
    fn fail_policy_conflict_returns_typed_port_conflict() {
        let (_d, db) = temp_db();
        // A different installed app holds 3000; under Fail policy the launch is
        // rejected before binding with the typed error code.
        db.record_port_claim(&claim("ipk_b", 3000)).unwrap();
        let err = plan_port_admission_with(
            &db,
            Some("ipk_a"),
            "main",
            "tcp",
            Some(3000),
            ConflictPolicy::Fail,
            |_| true,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains(crate::utils::error::ATO_ERR_PORT_CONFLICT),
            "rejection must carry the typed code: {err}"
        );
    }

    #[test]
    fn remaps_when_another_installed_app_holds_the_port() {
        let (_d, db) = temp_db();
        db.record_port_claim(&claim("ipk_b", 3000)).unwrap();
        let plan = plan_port_admission_with(
            &db,
            Some("ipk_a"),
            "main",
            "tcp",
            Some(3000),
            ConflictPolicy::Remap,
            |_| true,
        )
        .unwrap()
        .unwrap();
        assert_ne!(
            plan.resolved_port, 3000,
            "must remap away from the held port"
        );
        assert!(plan.resolved_port >= 49152);
        assert_eq!(plan.claim.preferred_port, 3000);
        assert_eq!(plan.claim.last_actual_port, Some(plan.resolved_port));
    }

    #[test]
    fn uncontended_keeps_preferred_and_records_actual() {
        let (_d, db) = temp_db();
        let plan = plan_port_admission_with(
            &db,
            Some("ipk_a"),
            "main",
            "tcp",
            Some(3000),
            ConflictPolicy::Remap,
            |_| true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.resolved_port, 3000);
        assert_eq!(plan.claim.last_actual_port, Some(3000));

        record_port_admission_plan(&db, &plan);
        let claims = db.port_claims().unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].preferred_port, 3000);
        assert_eq!(claims[0].last_actual_port, Some(3000));
    }

    #[test]
    fn same_app_same_endpoint_uses_fast_path() {
        let (_d, db) = temp_db();
        // app-a already holds its own main endpoint at 3000.
        let mut existing = claim("ipk_a", 3000);
        existing.last_actual_port = Some(3000);
        db.record_port_claim(&existing).unwrap();
        // Re-launching the same app/endpoint sees its own claim as self → keeps 3000.
        let plan = plan_port_admission_with(
            &db,
            Some("ipk_a"),
            "main",
            "tcp",
            Some(3000),
            ConflictPolicy::Remap,
            |_| true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            plan.resolved_port, 3000,
            "own endpoint claim must not self-conflict"
        );
    }
}
