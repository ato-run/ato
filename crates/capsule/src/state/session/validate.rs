//! Record-only validation. Decides whether a `StoredSessionInfo` is
//! safe to reuse without re-running `ato app resolve` or
//! `ato app session start`.
//!
//! "Record-only" means: we MUST NOT re-derive the launch digest here.
//! We only check that the digest exists and that the on-disk record's
//! schema is recent enough to be trusted (RFC §3.2 — the Phase 1
//! design intentionally leaves digest-mismatch detection to the
//! subprocess fallback path so the fast path stays small and pure).

use std::time::Duration;

use crate::state::session::healthcheck::http_get_ok;
use crate::state::session::process::{pid_is_alive, process_start_time_unix_ms};
use crate::state::session::record::{SCHEMA_VERSION_V2, StoredSessionInfo};

/// Inputs to `validate_record_only`. Lets the caller distinguish
/// "no candidate found" from "found but rejected" without relying on
/// out-of-band logging.
pub struct RecordValidationParams<'a> {
    /// The handle the user clicked. Compared against `handle`,
    /// `normalized_handle`, and `canonical_handle` on the record.
    pub requested_handle: &'a str,
    /// Healthcheck timeout. The fast path budget should be small —
    /// 200 ms is a reasonable default; the caller can shorten if
    /// click-to-paint pressure is acute.
    pub healthcheck_timeout: Duration,
}

/// Outcome of validating one stored record. Distinct variants so the
/// caller can log a precise reason and surface the right metric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordValidationOutcome {
    /// The record passes all five reuse conditions and may be turned
    /// into a `CapsuleLaunchSession` by the caller.
    Reusable,
    /// `schema_version` is missing or below v2 — record predates the
    /// App Session Materialization layer and must be displayed only.
    StaleSchema,
    /// `launch_digest` is missing — caller cannot trust the record.
    MissingLaunchDigest,
    /// The handle on the record doesn't match the requested handle.
    HandleMismatch,
    /// The record's `install_profile_key` doesn't match the requested one
    /// (or the record has none). Only produced by
    /// [`validate_record_for_install_profile_key`].
    InstallProfileKeyMismatch,
    /// The recorded PID is no longer alive.
    PidNotAlive,
    /// The platform reports a different process start time than the
    /// record — likely OS PID reuse. v0 falls back rather than risk
    /// a wrong attach.
    StartTimeMismatch,
    /// HTTP healthcheck didn't return 200 within the timeout, or the
    /// record has no healthcheck URL to probe.
    HealthcheckFailed,
}

/// Returns `true` when `requested` matches at least one of the record's
/// canonical, normalized, or raw handle fields. This tolerates the
/// `capsule://...` ↔ `publisher/slug` representation drift the
/// Desktop sees in real-world clicks.
pub fn handle_matches_record(requested: &str, record: &StoredSessionInfo) -> bool {
    requested == record.handle
        || requested == record.normalized_handle
        || record
            .canonical_handle
            .as_deref()
            .is_some_and(|canonical| canonical == requested)
}

/// Validate one record against the requested handle. Pure helper —
/// caller is responsible for picking the right candidate from
/// `read_session_records`.
///
/// ### Validation order (each step short-circuits on failure)
///
/// 1. `handle` matches one of `handle` / `normalized_handle` /
///    `canonical_handle` on the record (cheap — string compare).
/// 2. `schema_version >= SCHEMA_VERSION_V2` (cheap — int compare).
///    Pre-v0.4 records are display-only; reuse gate is opt-in via
///    schema bump.
/// 3. `launch_digest.is_some()` (cheap — option check). Records
///    written by older v0 paths or hand-edited may lack this; reuse
///    is unsafe without it.
/// 4. `pid_is_alive(record.pid)` — if the recorded PID is no longer
///    alive the session process has exited and cannot be reused.
/// 5. `process_start_time_unix_ms` match — if the OS reports a
///    different start time than the record, the PID has been reused
///    (OS PID wrap-around). This prevents attaching to an unrelated
///    process that inherited the same PID number.
/// 6. **Healthcheck** (~5–50 ms over loopback). Returns
///    `HealthcheckFailed` on any failure mode (no URL, timeout,
///    non-200, parse error).
pub fn validate_record_only(
    record: &StoredSessionInfo,
    params: &RecordValidationParams<'_>,
) -> RecordValidationOutcome {
    if !handle_matches_record(params.requested_handle, record) {
        return RecordValidationOutcome::HandleMismatch;
    }
    validate_record_liveness(record, params.healthcheck_timeout)
}

/// Validate one record for reuse, keyed by `install_profile_key` instead of
/// capsule handle. This is the identity contract for **installed-app
/// relaunch** (#261): the durable identity of an installed entry is its
/// `install_profile_key`, never the capsule handle. A session produced by
/// `ato launch <install_profile_key>` is matched by its stamped
/// `install_profile_key`, so a handle that has drifted from the record's
/// canonical handle (after install, rename, or alias normalization) does not
/// cause a miss.
///
/// Returns [`RecordValidationOutcome::InstallProfileKeyMismatch`] when the
/// record carries a different `install_profile_key` (or none at all);
/// otherwise it runs the identical liveness checks as [`validate_record_only`]
/// (schema, digest, pid alive, start-time, healthcheck). The capsule handle is
/// deliberately *not* consulted.
pub fn validate_record_for_install_profile_key(
    record: &StoredSessionInfo,
    install_profile_key: &str,
    healthcheck_timeout: Duration,
) -> RecordValidationOutcome {
    if record.install_profile_key.as_deref() != Some(install_profile_key) {
        return RecordValidationOutcome::InstallProfileKeyMismatch;
    }
    validate_record_liveness(record, healthcheck_timeout)
}

/// Steps 2–6 of the reuse contract (everything after identity matching),
/// shared by the handle-keyed ([`validate_record_only`]) and
/// install-profile-key-keyed ([`validate_record_for_install_profile_key`])
/// entry points. The caller has already confirmed the record's identity.
fn validate_record_liveness(
    record: &StoredSessionInfo,
    healthcheck_timeout: Duration,
) -> RecordValidationOutcome {
    if record.schema_version.unwrap_or(1) < SCHEMA_VERSION_V2 {
        return RecordValidationOutcome::StaleSchema;
    }
    if record.launch_digest.is_none() {
        return RecordValidationOutcome::MissingLaunchDigest;
    }

    if !pid_is_alive(record.pid as u32) {
        return RecordValidationOutcome::PidNotAlive;
    }

    if let Some(recorded_start) = record.process_start_time_unix_ms {
        match process_start_time_unix_ms(record.pid as u32) {
            Some(current_start) => {
                if recorded_start != current_start {
                    return RecordValidationOutcome::StartTimeMismatch;
                }
            }
            None => {
                return RecordValidationOutcome::StartTimeMismatch;
            }
        }
    }

    let healthcheck_url = healthcheck_url_for(record);
    let url = match healthcheck_url {
        Some(url) => url,
        None => return RecordValidationOutcome::HealthcheckFailed,
    };
    match http_get_ok(url, healthcheck_timeout) {
        Ok(true) => RecordValidationOutcome::Reusable,
        Ok(false) | Err(_) => RecordValidationOutcome::HealthcheckFailed,
    }
}

/// Pick the right healthcheck URL for the record's display strategy.
/// Guest sessions advertise a `/health` endpoint; web sessions expose
/// the same on the dev-server. Terminal / service variants don't
/// bind an HTTP port, so the fast path can't validate them at v0 —
/// they fall through to the subprocess path.
fn healthcheck_url_for(record: &StoredSessionInfo) -> Option<&str> {
    if let Some(guest) = record.guest.as_ref() {
        return Some(guest.healthcheck_url.as_str());
    }
    if let Some(web) = record.web.as_ref() {
        return Some(web.healthcheck_url.as_str());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::session::record::{GuestSessionDisplay, SCHEMA_VERSION_V2};
    use ato_protocol::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};

    fn base_record() -> StoredSessionInfo {
        StoredSessionInfo {
            session_id: "ato-desktop-session-1".to_string(),
            launch_key: None,
            handle: "capsule://ato.run/koh0920/byok-ai-chat".to_string(),
            normalized_handle: "koh0920/byok-ai-chat".to_string(),
            canonical_handle: Some("koh0920/byok-ai-chat".to_string()),
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "main".to_string(),
                runtime: Some("node".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: std::process::id() as i32,
            log_path: "/tmp/x.log".to_string(),
            manifest_path: "/tmp/manifest.toml".to_string(),
            target_label: "main".to_string(),
            notes: vec![],
            readiness_confirmed: false,
            guest: Some(GuestSessionDisplay {
                adapter: "node".to_string(),
                frontend_entry: "index.html".to_string(),
                transport: "http".to_string(),
                // Port 1 is unbound — healthcheck always fails.
                healthcheck_url: "http://127.0.0.1:1/health".to_string(),
                invoke_url: "http://127.0.0.1:1/invoke".to_string(),
                capabilities: vec![],
            }),
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: Some(SCHEMA_VERSION_V2),
            launch_digest: Some("d".repeat(64)),
            // Match the running process so the start-time check passes
            // and the test reaches the healthcheck step deterministically.
            process_start_time_unix_ms: crate::state::session::process::process_start_time_unix_ms(
                std::process::id(),
            ),
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
            placement_provider: None,
            placement_provider_id: None,
            placement_id: None,
            placement_fingerprint: None,
            placement_facets: None,
            user_visible_url: None,
            requested_by_client: None,
            runtime_owner: None,
        }
    }

    fn params() -> RecordValidationParams<'static> {
        RecordValidationParams {
            requested_handle: "capsule://ato.run/koh0920/byok-ai-chat",
            healthcheck_timeout: Duration::from_millis(50),
        }
    }

    #[test]
    fn handle_matches_canonical_or_normalized() {
        let record = base_record();
        assert!(handle_matches_record(
            "capsule://ato.run/koh0920/byok-ai-chat",
            &record
        ));
        assert!(handle_matches_record("koh0920/byok-ai-chat", &record));
    }

    #[test]
    fn rejects_handle_mismatch() {
        let record = base_record();
        let p = RecordValidationParams {
            requested_handle: "publisher/other-slug",
            healthcheck_timeout: Duration::from_millis(50),
        };
        assert_eq!(
            validate_record_only(&record, &p),
            RecordValidationOutcome::HandleMismatch
        );
    }

    #[test]
    fn rejects_schema_v1_record() {
        let mut record = base_record();
        record.schema_version = None;
        assert_eq!(
            validate_record_only(&record, &params()),
            RecordValidationOutcome::StaleSchema
        );
    }

    #[test]
    fn rejects_missing_launch_digest() {
        let mut record = base_record();
        record.launch_digest = None;
        assert_eq!(
            validate_record_only(&record, &params()),
            RecordValidationOutcome::MissingLaunchDigest
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn dead_pid_returns_pid_not_alive() {
        // PID 0 is special on Unix and never alive. The PID gate
        // short-circuits before healthcheck because a dead process
        // cannot be reused regardless of whether a different process
        // happens to be listening on the same port.
        let mut record = base_record();
        record.pid = 0;
        assert_eq!(
            validate_record_only(&record, &params()),
            RecordValidationOutcome::PidNotAlive,
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn start_time_mismatch_returns_start_time_mismatch() {
        // When the recorded PID is alive but the OS reports a
        // different process start time, the PID has been reused by
        // a different process. Must fail before healthcheck to
        // prevent attaching to the wrong session.
        let mut record = base_record();
        record.process_start_time_unix_ms = Some(1);
        assert_eq!(
            validate_record_only(&record, &params()),
            RecordValidationOutcome::StartTimeMismatch,
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn healthcheck_failure_returned_when_endpoint_dead() {
        // Sanity check the authoritative path: with everything
        // except the healthcheck OK, the validator returns
        // HealthcheckFailed.
        let record = base_record();
        assert_eq!(
            validate_record_only(&record, &params()),
            RecordValidationOutcome::HealthcheckFailed
        );
    }

    // ── install_profile_key-keyed validation (#261) ─────────────────────────

    const TEST_IPK: &str = "ipk_abc00000000000000000000000000";

    fn ipk_record(ipk: Option<&str>) -> StoredSessionInfo {
        let mut record = base_record();
        record.install_profile_key = ipk.map(str::to_string);
        record
    }

    #[test]
    fn ipk_mismatch_when_record_key_differs() {
        let record = ipk_record(Some("ipk_other000000000000000000000000"));
        assert_eq!(
            validate_record_for_install_profile_key(&record, TEST_IPK, Duration::from_millis(50)),
            RecordValidationOutcome::InstallProfileKeyMismatch
        );
    }

    #[test]
    fn ipk_mismatch_when_record_has_no_key() {
        let record = ipk_record(None);
        assert_eq!(
            validate_record_for_install_profile_key(&record, TEST_IPK, Duration::from_millis(50)),
            RecordValidationOutcome::InstallProfileKeyMismatch
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn ipk_match_ignores_handle_and_reaches_liveness() {
        // The capsule handle on the record is irrelevant once the
        // install_profile_key matches: even with a completely different
        // handle the validator does NOT short-circuit on identity. It runs
        // the liveness checks instead — here the (unbound port-1) healthcheck
        // is what stops reuse, proving the ipk match itself succeeded and the
        // handle was never consulted.
        let mut record = ipk_record(Some(TEST_IPK));
        record.handle = "capsule://unrelated/other-app".to_string();
        record.normalized_handle = "unrelated/other-app".to_string();
        record.canonical_handle = Some("unrelated/other-app".to_string());
        assert_eq!(
            validate_record_for_install_profile_key(&record, TEST_IPK, Duration::from_millis(50)),
            RecordValidationOutcome::HealthcheckFailed
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn ipk_match_dead_pid_returns_pid_not_alive() {
        let mut record = ipk_record(Some(TEST_IPK));
        record.pid = 0;
        assert_eq!(
            validate_record_for_install_profile_key(&record, TEST_IPK, Duration::from_millis(50)),
            RecordValidationOutcome::PidNotAlive
        );
    }

    // NOTE: the full success path (`Reusable` with a live 200 healthcheck) is
    // intentionally not asserted here. It requires an in-process HTTP server,
    // which this suite has found to be nondeterministic under parallel test
    // load (the loopback probe can fail fast even with a multi-second budget).
    // The identity contract this function changes is covered deterministically
    // by the tests above; the shared liveness path (schema/digest/pid/start-
    // time/healthcheck) is unchanged from `validate_record_only` and exercised
    // by `capsule::healthcheck::tests` plus the handle-keyed suite.
}
