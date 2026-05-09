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

use crate::healthcheck::http_get_ok;
use crate::record::{StoredSessionInfo, SCHEMA_VERSION_V2};

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
/// The reuse gate uses **healthcheck as the authoritative signal**:
/// if a process is listening on the recorded healthcheck URL and
/// answers 200, the session is reusable, full stop. PID and
/// start-time checks are advisory pre-gates only — they short-circuit
/// the expensive healthcheck when we *know* the session is dead, but
/// they cannot reject reuse when the healthcheck would have passed.
///
/// ### Why healthcheck wins over PID
///
/// Real-world capsules (`byok-ai-chat` measured 2026-04-29) often use
/// shells that fork and exit:
///
/// ```text
/// ato-cli → spawn npm → npm forks `next start` → npm exits
/// ```
///
/// `ato-cli` records `npm`'s PID, but `npm` is no longer alive once
/// the actual web server (`next`, a different PID, child of npm) is
/// up and serving. A strict `pid_is_alive(record.pid)` gate misses
/// this case and forces the user back through the cold path even
/// though the session is fully working.
///
/// The healthcheck is what the user actually cares about: "does the
/// app respond?" If yes, attaching a new pane to it is safe. If no,
/// reuse is unsafe regardless of what `pid_is_alive` says.
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
/// 4. **Healthcheck** (the authoritative check, ~5–50 ms over loop-
///    back). Returns `HealthcheckFailed` on any failure mode (no URL,
///    timeout, non-200, parse error).
///
/// PID + start_time are NOT consulted as gates. The fields stay on the
/// record for diagnostics and future use (e.g. a future `force-stop`
/// path that needs to signal something specific).
pub fn validate_record_only(
    record: &StoredSessionInfo,
    params: &RecordValidationParams<'_>,
) -> RecordValidationOutcome {
    if !handle_matches_record(params.requested_handle, record) {
        return RecordValidationOutcome::HandleMismatch;
    }
    if record.schema_version.unwrap_or(1) < SCHEMA_VERSION_V2 {
        return RecordValidationOutcome::StaleSchema;
    }
    if record.launch_digest.is_none() {
        return RecordValidationOutcome::MissingLaunchDigest;
    }

    let healthcheck_url = healthcheck_url_for(record);
    let url = match healthcheck_url {
        Some(url) => url,
        None => return RecordValidationOutcome::HealthcheckFailed,
    };
    match http_get_ok(url, params.healthcheck_timeout) {
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
    use crate::record::{GuestSessionDisplay, SCHEMA_VERSION_V2};
    use capsule_wire::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};

    fn base_record() -> StoredSessionInfo {
        StoredSessionInfo {
            session_id: "ato-desktop-session-1".to_string(),
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
            orchestration_services: None,
            schema_version: Some(SCHEMA_VERSION_V2),
            launch_digest: Some("d".repeat(64)),
            // Match the running process so the start-time check passes
            // and the test reaches the healthcheck step deterministically.
            process_start_time_unix_ms: crate::process::process_start_time_unix_ms(
                std::process::id(),
            ),
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
    fn dead_pid_does_not_block_when_healthcheck_would_be_reachable() {
        // Real-world capsules (`npm run start`) record a wrapper PID
        // that exits as soon as the actual server is up. The fast
        // path must not reject reuse just because the recorded PID
        // is dead — only the healthcheck does. Here the healthcheck
        // URL is unbound (port 1) so the validator returns
        // HealthcheckFailed (not PidNotAlive) — the gate is the
        // healthcheck, not the PID.
        let mut record = base_record();
        record.pid = 0;
        assert_eq!(
            validate_record_only(&record, &params()),
            RecordValidationOutcome::HealthcheckFailed,
            "PID is no longer a gate; absent of an alive healthcheck, \
             rejection must come from HealthcheckFailed, not PidNotAlive"
        );
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn start_time_mismatch_does_not_block_validation_either() {
        // Same rationale as `dead_pid_does_not_block_…`: the
        // process_start_time field stays on the record for
        // diagnostics but does not gate reuse. The unbound
        // healthcheck URL is what fails here.
        let mut record = base_record();
        record.process_start_time_unix_ms = Some(1);
        assert_eq!(
            validate_record_only(&record, &params()),
            RecordValidationOutcome::HealthcheckFailed
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
}
