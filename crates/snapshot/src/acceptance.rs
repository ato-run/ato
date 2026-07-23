//! Disposable acceptance orchestration for `running` Snapshots (issue #1088).
//!
//! A `running` Snapshot is accepted **only** after an *immutable candidate* is
//! verified through a **disposable** restored Session, and never by letting the
//! verification's side effects into the accepted bytes. The RFC
//! (`docs/rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §8.1) fixes the
//! pipeline:
//!
//! ```text
//! launch build guest
//!   → capture an immutable candidate Snapshot
//!   → restore the candidate into a disposable Session
//!   → run seal_at.command as EXACT argv in that Session
//!   → exit 0: mark accepted   → otherwise: reject
//!   → ALWAYS destroy the verification overlay and Session
//! ```
//!
//! `seal_at.command` is evaluated against a *disposable restore of a candidate*,
//! so it determines **acceptance**, not the capture instant (RFC §6.3). Ato
//! interprets only the process result: **exit 0 is the sole success signal**;
//! any non-zero exit, signal, or timeout rejects, and a timeout MUST terminate
//! the full verification process tree (RFC §6.3 / §8.4).
//!
//! This module is **Gate-0 style**: pure, deterministic orchestration with one
//! injectable IO seam ([`DisposableAcceptanceLifecycle`]). It performs no live
//! builder / runner / firecracker work — that wiring is PR-2. The trait is the
//! entire boundary between this deterministic loop and real capture / restore /
//! process control, mirroring the crate's established `SnapshotBackend` +
//! `FakeSnapshotBackend` "trait seam + fake" pattern so the loop is fully
//! unit-testable without a VM.
//!
//! **Immutability model.** The candidate is held by value as an immutable
//! [`SnapshotManifestV1`]; its content address ([`SnapshotManifestV1::snapshot_id`])
//! is derived once and is the address that is accepted. The disposable Session
//! is a *separate* [`DisposableSessionHandle`] that the lifecycle restores into,
//! runs the command in, and then destroys. Because every verification method
//! borrows the candidate immutably and mutates only the Session overlay, the
//! accepted bytes are the candidate bytes **unchanged**: the overlay is discarded
//! and never folded back into the candidate.
//!
//! **Scope.** `running` capture policy only. `workload_idle`, placeholder
//! revocation, restore-time real bindings, and restart are #1093 and out of
//! scope; so is any HTTP / readiness / gate DSL — `seal_at.command` is exact
//! argv with no implicit shell.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use capsule::snapshot_manifest::{
    AcceptanceStatus, CapturePolicyV1, SanitizationAttestationV1, SecretScanAttestationV1,
    SnapshotCatalogRecord, SnapshotManifestV1,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An immutable capture candidate. Its `manifest` is never mutated by
/// verification; the accepted Snapshot's bytes are exactly these bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSnapshot {
    pub manifest: SnapshotManifestV1,
}

/// An opaque handle to a **disposable** verification Session. Restoring a
/// candidate into it and running `seal_at.command` against it writes only to this
/// Session's overlay, which is always destroyed — never merged into the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableSessionHandle {
    pub opaque_id: String,
}

/// Snapshot-build eligibility for a live-workload `running` capture. A Capsule
/// whose live workload requires External State cannot be captured `running`
/// without binding production secrets / user state, so it fails **closed** here
/// (RFC §8.3) — it must never fall back to a secret-bearing running capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEligibility {
    pub external_state_required_by_live_workload: bool,
}

/// Bounds for one acceptance run: the exact `seal_at` argv, the per-attempt
/// verification timeout, the overall build deadline, and the maximum attempt
/// count. Retries are bounded by **both** the deadline and this count (RFC §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceConfig {
    /// `seal_at.command` as exact argv — executed with no implicit shell and with
    /// argument boundaries preserved (RFC §6.1). Must be non-empty and NUL-free.
    pub seal_at_argv: Vec<String>,
    /// Per-attempt verification timeout. On timeout the full process tree is
    /// terminated and the attempt rejects.
    pub verification_timeout: Duration,
    /// Overall build deadline across all attempts.
    pub total_deadline: Duration,
    /// Maximum number of capture→verify attempts.
    pub maximum_attempts: u32,
}

/// The process result of one `seal_at.command` run. Only [`VerificationOutcome::Exited`]
/// with code `0` accepts; every other variant (a non-zero exit, a signal, a lost
/// child, a timeout, or a cancellation) rejects.
///
/// A signalled or lost process has **no** exit code, so it gets its own variant
/// rather than being squeezed into `Exited`: this makes the exit-0-only invariant
/// *structural*. A seam impl (PR-2) cannot represent "terminated by a signal" or
/// "child was lost" as `Exited(0)` — the only way to reach the accept arm — so it
/// cannot falsely accept a process that never cleanly exited 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// The command exited with this status code. `0` is the sole success signal.
    Exited(i32),
    /// The command was terminated by a signal (`WIFSIGNALED`), carrying the signal
    /// number. A signal is **never** success: the seam MUST surface a signalled
    /// process as `Signalled`, never as `Exited(0)`. Always rejects.
    Signalled(i32),
    /// The child was lost or produced no decodable exit status — an unexpected EOF
    /// on the control channel, a missing exit code, or any wait status that is
    /// neither a clean exit nor a signal. **Never** success; always rejects.
    Lost,
    /// The command exceeded [`AcceptanceConfig::verification_timeout`].
    TimedOut,
    /// The run was cancelled before the command produced a result.
    Cancelled,
}

/// A cooperative cancellation token for an in-flight acceptance run.
#[derive(Debug, Clone, Default)]
pub struct AcceptanceCancellation(Arc<AtomicBool>);

impl AcceptanceCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// One attempt's receipt: which candidate was verified, the outcome label, and
/// whether the (always-attempted) teardown actually ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceAttemptReceipt {
    pub attempt: u32,
    pub candidate_snapshot_id: Option<String>,
    pub outcome: String,
    pub process_tree_terminated: bool,
    pub disposable_session_destroyed: bool,
}

/// The receipt for a whole acceptance run. Records the capture policy, the
/// per-attempt history (so bounded recapture retries are auditable), and — on
/// acceptance — the accepted address plus the sanitization and **redacted**
/// secret-scan attestations carried by the accepted manifest.
///
/// The secret-scan attestation is exactly that: an *attestation that a scan ran*
/// with a redacted verdict. It is **never** a proof of absence of secrets (RFC
/// §8 / §17.3) — it is defense in depth alongside the structural sanitization and
/// the fail-closed External-State eligibility gate, not a substitute for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceReceipt {
    pub capture_policy: CapturePolicyV1,
    pub maximum_attempts: u32,
    pub attempts: Vec<AcceptanceAttemptReceipt>,
    pub accepted_snapshot_id: Option<String>,
    /// Structural-sanitization attestation of the accepted candidate (which
    /// cleanup / revocation steps ran). `None` when nothing was accepted.
    pub sanitization_attestation: Option<SanitizationAttestationV1>,
    /// Redacted secret-scan attestation of the accepted candidate. `None` when
    /// nothing was accepted. Attestation only — never proof of absence.
    pub secret_scan_attestation: Option<SecretScanAttestationV1>,
}

/// A successful acceptance: the accepted catalog record and the run receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceResult {
    pub snapshot: SnapshotCatalogRecord,
    pub receipt: AcceptanceReceipt,
}

/// Fail-closed acceptance errors. Every variant refuses to accept: a candidate
/// that cannot be proven acceptable never yields an accepted [`SnapshotCatalogRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AcceptanceError {
    /// The live workload requires External State, so a `running` capture is
    /// ineligible: it would have to bind production secrets / user state. Fails
    /// closed at the entry point, before any capture (RFC §8.3).
    #[error("running Snapshot is ineligible because the live workload requires External State")]
    ExternalStateRequiresWorkloadIdle,
    /// The candidate's own `capture_policy` is not `running`. This first slice
    /// accepts `running` only; the check is on the candidate manifest itself and
    /// never relies solely on a host advertising `[Running]`.
    #[error("unsupported capture policy: this acceptance path accepts `running` candidates only")]
    UnsupportedCapturePolicy,
    /// The acceptance configuration is malformed (empty/NUL argv, zero timeout,
    /// zero attempts, or a deadline that cannot cover one verification).
    #[error("invalid Snapshot acceptance configuration: {0}")]
    InvalidConfig(&'static str),
    /// The run was cancelled.
    #[error("Snapshot acceptance was cancelled")]
    Cancelled,
    /// A candidate failed its own manifest invariants during the named phase.
    #[error("Snapshot candidate lifecycle failed during {phase}")]
    Lifecycle { phase: &'static str },
    /// Teardown of the disposable Session (process-tree termination and/or
    /// destroy) failed. The Session must be treated as leaked and quarantined.
    #[error("disposable Session cleanup failed")]
    Cleanup,
    /// No candidate was accepted within the configured attempts and deadline. The
    /// receipt carries the full bounded-retry history.
    #[error("Snapshot candidate was not accepted within the configured attempts and deadline")]
    Exhausted { receipt: Box<AcceptanceReceipt> },
}

/// The single injectable IO seam between the pure acceptance loop and the real
/// world. A production impl (PR-2) captures on a live builder, restores into a
/// firecracker microVM with **no** production secret / user state / Ato identity
/// attached (empty/synthetic ephemeral state only, RFC §8.4), runs the exact
/// argv in that guest, and tears the Session's process tree and overlay down.
/// Tests supply a deterministic fake.
pub trait DisposableAcceptanceLifecycle {
    /// Capture an immutable candidate Snapshot for this attempt. A fresh capture
    /// per attempt is what makes recapture retries meaningful.
    fn capture_candidate(&mut self, attempt: u32) -> Result<CandidateSnapshot, String>;

    /// Create an isolated disposable Session with **no** production secrets, user
    /// state, or Ato user identity attached. Implementations MUST treat that
    /// boundary as part of their security contract (RFC §8.4).
    fn create_disposable_session(
        &mut self,
        candidate: &CandidateSnapshot,
    ) -> Result<DisposableSessionHandle, String>;

    /// Restore the immutable candidate into the disposable Session's overlay. The
    /// candidate is borrowed immutably: the restore writes only to the Session.
    fn restore_candidate(
        &mut self,
        session: &DisposableSessionHandle,
        candidate: &CandidateSnapshot,
    ) -> Result<(), String>;

    /// Execute `seal_at.command` as exact argv in the disposable Session, bounded
    /// by `timeout` and cooperatively cancellable. No implicit shell; argument
    /// boundaries are preserved.
    ///
    /// The returned [`VerificationOutcome`] MUST faithfully classify the wait
    /// status: a process that exited maps to [`VerificationOutcome::Exited`] with
    /// its real code; a signal-terminated process (`WIFSIGNALED`) to
    /// [`VerificationOutcome::Signalled`]; a lost / undecodable child (unexpected
    /// EOF, missing exit code, any non-exit non-signal status) to
    /// [`VerificationOutcome::Lost`]; a timeout to [`VerificationOutcome::TimedOut`].
    /// Mapping any non-clean wait status to `Exited(0)` is a **contract violation**:
    /// `Exited(0)` is the sole accept, so an impl that reports a signalled, lost, or
    /// timed-out process as `Exited(0)` would falsely accept it. Only a genuine,
    /// clean exit with code 0 may be reported as `Exited(0)`.
    fn execute_exact_argv(
        &mut self,
        session: &DisposableSessionHandle,
        argv: &[String],
        timeout: Duration,
        cancellation: &AcceptanceCancellation,
    ) -> Result<VerificationOutcome, String>;

    /// Terminate the **full** verification process tree in the Session (RFC §8.4).
    /// Called on timeout / cancellation / verification error before destroy.
    fn terminate_process_tree(&mut self, session: &DisposableSessionHandle) -> Result<(), String>;

    /// Destroy the disposable Session and its overlay. Called unconditionally
    /// after a Session is created — success, failure, timeout, or cancellation.
    fn destroy_disposable_session(
        &mut self,
        session: DisposableSessionHandle,
    ) -> Result<(), String>;
}

/// RAII teardown guard for a live disposable verification Session.
///
/// Between creating a Session and destroying it the loop runs fallible seam
/// methods (restore / execute) and, in PR-2, `DisposableSessionHandle` owns a real
/// firecracker microVM + overlay. Straight-line teardown would leak that VM if any
/// seam method — or a downstream `.clone()` — panicked and unwound the loop before
/// destroy ran. This guard closes that hole: the normal path disarms it via
/// [`DisposableSessionGuard::teardown`] (which records the receipt fields and
/// surfaces failures as [`AcceptanceError::Cleanup`]); on any other exit `Drop`
/// still terminates the process tree and destroys the Session, so a disposable
/// microVM + overlay is never leaked on the panic / early-return path (RFC §8.4).
struct DisposableSessionGuard<'a, L: DisposableAcceptanceLifecycle + ?Sized> {
    lifecycle: &'a mut L,
    /// `Some` while the Session is live; `None` once `teardown` (or a prior `Drop`)
    /// has consumed the handle, which disarms the guard so `Drop` is a no-op.
    session: Option<DisposableSessionHandle>,
}

impl<'a, L: DisposableAcceptanceLifecycle + ?Sized> DisposableSessionGuard<'a, L> {
    fn new(lifecycle: &'a mut L, session: DisposableSessionHandle) -> Self {
        Self {
            lifecycle,
            session: Some(session),
        }
    }

    /// Restore the immutable candidate into the still-live Session's overlay.
    fn restore_candidate(&mut self, candidate: &CandidateSnapshot) -> Result<(), String> {
        let session = self
            .session
            .as_ref()
            .expect("disposable Session handle is live until teardown");
        self.lifecycle.restore_candidate(session, candidate)
    }

    /// Run `seal_at.command` as exact argv in the still-live Session.
    fn execute_exact_argv(
        &mut self,
        argv: &[String],
        timeout: Duration,
        cancellation: &AcceptanceCancellation,
    ) -> Result<VerificationOutcome, String> {
        let session = self
            .session
            .as_ref()
            .expect("disposable Session handle is live until teardown");
        self.lifecycle
            .execute_exact_argv(session, argv, timeout, cancellation)
    }

    /// Disarm the guard and run the normal-path teardown: terminate the process
    /// tree when required, then **always** destroy the Session. Records the outcome
    /// on `receipt` and returns whether either step failed (mapped by the caller to
    /// [`AcceptanceError::Cleanup`]). Consuming `self` here means the subsequent
    /// `Drop` sees `session == None` and does nothing.
    fn teardown(
        mut self,
        terminate_required: bool,
        receipt: &mut AcceptanceAttemptReceipt,
    ) -> bool {
        let session = self
            .session
            .take()
            .expect("disposable Session handle is live until teardown");
        let mut cleanup_failed = false;
        if terminate_required {
            match self.lifecycle.terminate_process_tree(&session) {
                Ok(()) => receipt.process_tree_terminated = true,
                Err(_) => cleanup_failed = true,
            }
        }
        match self.lifecycle.destroy_disposable_session(session) {
            Ok(()) => receipt.disposable_session_destroyed = true,
            Err(_) => cleanup_failed = true,
        }
        cleanup_failed
    }
}

impl<L: DisposableAcceptanceLifecycle + ?Sized> Drop for DisposableSessionGuard<'_, L> {
    fn drop(&mut self) {
        // Only fires when `teardown` did NOT run — i.e. the loop unwound (panic) or
        // returned early while the Session was still live. Best-effort and
        // infallible: terminate the process tree, then destroy the Session, so a
        // live disposable microVM + overlay is never leaked. Errors are
        // unrecoverable here and are deliberately dropped (a `Drop` impl cannot
        // surface them); the normal path uses `teardown` for the fallible,
        // receipted teardown that reports `Cleanup`.
        if let Some(session) = self.session.take() {
            let _ = self.lifecycle.terminate_process_tree(&session);
            let _ = self.lifecycle.destroy_disposable_session(session);
        }
    }
}

/// The pure acceptance orchestrator for `running` Snapshots.
pub struct RunningSnapshotAcceptance;

impl RunningSnapshotAcceptance {
    /// Accept a `running` candidate by verifying it through a disposable restored
    /// Session, or fail closed.
    ///
    /// Ordering of the fail-closed gates:
    /// 1. Configuration is validated.
    /// 2. External-State-for-live-workload eligibility is rejected **before any
    ///    capture** (RFC §8.3).
    /// 3. Per attempt (bounded by `maximum_attempts` **and** `total_deadline`):
    ///    capture an immutable candidate, reject any candidate whose own
    ///    `capture_policy` is not `running`, create a disposable Session, restore
    ///    the candidate into it, run the exact argv, accept on and only on exit
    ///    `0`, and **always** tear the Session down.
    pub fn accept(
        lifecycle: &mut impl DisposableAcceptanceLifecycle,
        eligibility: SnapshotEligibility,
        config: &AcceptanceConfig,
        cancellation: &AcceptanceCancellation,
    ) -> Result<AcceptanceResult, AcceptanceError> {
        validate_config(config)?;
        // Fail closed at the entry point: an External-State live workload is
        // ineligible for a `running` capture and must never fall back to a
        // secret-bearing running capture (RFC §8.3). Checked before any capture.
        if eligibility.external_state_required_by_live_workload {
            return Err(AcceptanceError::ExternalStateRequiresWorkloadIdle);
        }

        let started = Instant::now();
        let mut receipt = AcceptanceReceipt {
            capture_policy: CapturePolicyV1::Running,
            maximum_attempts: config.maximum_attempts,
            attempts: Vec::new(),
            accepted_snapshot_id: None,
            sanitization_attestation: None,
            secret_scan_attestation: None,
        };

        for attempt in 1..=config.maximum_attempts {
            if cancellation.is_cancelled() {
                return Err(AcceptanceError::Cancelled);
            }
            if started.elapsed() >= config.total_deadline {
                break;
            }

            let candidate = match lifecycle.capture_candidate(attempt) {
                Ok(candidate) => candidate,
                Err(_) => {
                    receipt.attempts.push(AcceptanceAttemptReceipt {
                        attempt,
                        candidate_snapshot_id: None,
                        outcome: "capture-failed".to_string(),
                        process_tree_terminated: false,
                        disposable_session_destroyed: false,
                    });
                    continue;
                }
            };
            candidate
                .manifest
                .validate()
                .map_err(|_| AcceptanceError::Lifecycle {
                    phase: "candidate-validation",
                })?;
            // Capture-policy gate on the CANDIDATE MANIFEST ITSELF. The RFC's
            // first slice accepts `running` only; we never rely solely on a host
            // advertising `[Running]` — the candidate must declare `running`.
            if candidate.manifest.capture_policy != CapturePolicyV1::Running {
                return Err(AcceptanceError::UnsupportedCapturePolicy);
            }
            // The accepted address is derived from the candidate bytes ONCE, up
            // front. Verification borrows the candidate immutably and touches only
            // the disposable Session, so this address is exactly what is accepted.
            let snapshot_id =
                candidate
                    .manifest
                    .snapshot_id()
                    .map_err(|_| AcceptanceError::Lifecycle {
                        phase: "candidate-validation",
                    })?;

            let mut attempt_receipt = AcceptanceAttemptReceipt {
                attempt,
                candidate_snapshot_id: Some(snapshot_id.as_str().to_string()),
                outcome: "restore-failed".to_string(),
                process_tree_terminated: false,
                disposable_session_destroyed: false,
            };
            let session = match lifecycle.create_disposable_session(&candidate) {
                Ok(session) => session,
                Err(_) => {
                    attempt_receipt.outcome = "create-session-failed".to_string();
                    receipt.attempts.push(attempt_receipt);
                    continue;
                }
            };
            // The Session is now live. Hold it in an RAII guard so that even if a
            // seam method (restore / execute) or a downstream `.clone()` panics and
            // unwinds the loop before the explicit teardown below, the guard's
            // `Drop` still terminates the process tree and destroys the Session — a
            // real firecracker microVM + overlay must never leak on that path
            // (RFC §8.4). The normal path disarms the guard via `teardown`.
            let mut guard = DisposableSessionGuard::new(&mut *lifecycle, session);

            let (accepted, terminate_required) = if guard.restore_candidate(&candidate).is_err() {
                attempt_receipt.outcome = "restore-failed".to_string();
                (false, false)
            } else {
                match guard.execute_exact_argv(
                    &config.seal_at_argv,
                    config.verification_timeout,
                    cancellation,
                ) {
                    // A cancellation observed after a nominal exit still rejects
                    // and tears the tree down — we never accept a race winner.
                    Ok(VerificationOutcome::Exited(_)) if cancellation.is_cancelled() => {
                        attempt_receipt.outcome = "cancelled".to_string();
                        (false, true)
                    }
                    // Exit 0 within the deadline is the SOLE success signal.
                    Ok(VerificationOutcome::Exited(0))
                        if started.elapsed() < config.total_deadline =>
                    {
                        attempt_receipt.outcome = "accepted".to_string();
                        (true, false)
                    }
                    // Exit 0 that only landed after the deadline is not an accept.
                    Ok(VerificationOutcome::Exited(0)) => {
                        attempt_receipt.outcome = "deadline-exceeded".to_string();
                        (false, false)
                    }
                    Ok(VerificationOutcome::Exited(_)) => {
                        attempt_receipt.outcome = "nonzero-exit".to_string();
                        (false, false)
                    }
                    // A signal-terminated process has NO exit code, so it can never
                    // be `Exited(0)`: it is structurally impossible for it to reach
                    // the accept arm. Reject on the same path as a non-zero exit.
                    Ok(VerificationOutcome::Signalled(_)) => {
                        attempt_receipt.outcome = "signalled".to_string();
                        (false, false)
                    }
                    // A lost / undecodable child likewise has no exit code and can
                    // never be `Exited(0)`. Reject like a non-zero exit.
                    Ok(VerificationOutcome::Lost) => {
                        attempt_receipt.outcome = "lost".to_string();
                        (false, false)
                    }
                    Ok(VerificationOutcome::TimedOut) => {
                        attempt_receipt.outcome = "timeout".to_string();
                        (false, true)
                    }
                    Ok(VerificationOutcome::Cancelled) => {
                        attempt_receipt.outcome = "cancelled".to_string();
                        (false, true)
                    }
                    Err(_) => {
                        attempt_receipt.outcome = "verification-error".to_string();
                        (false, true)
                    }
                }
            };

            // Teardown is UNCONDITIONAL once a Session exists. Disarming the guard
            // runs the same terminate-then-destroy on the normal path: a failed
            // process-tree termination must not skip destroy, so destroy is still
            // attempted and either failure surfaces as `Cleanup`. (If instead the
            // loop had unwound above, the guard's `Drop` would have run this
            // teardown as a best-effort safety net.)
            let cleanup_failed = guard.teardown(terminate_required, &mut attempt_receipt);
            receipt.attempts.push(attempt_receipt);

            if cleanup_failed {
                return Err(AcceptanceError::Cleanup);
            }

            if accepted {
                // Accepted bytes = candidate bytes, unchanged. The overlay that
                // the command wrote to has been destroyed above; the accepted
                // record pins the address derived from the candidate before any
                // verification ran.
                receipt.accepted_snapshot_id = Some(snapshot_id.as_str().to_string());
                receipt.sanitization_attestation =
                    Some(candidate.manifest.sanitization_attestation.clone());
                receipt.secret_scan_attestation =
                    Some(candidate.manifest.secret_scan_attestation.clone());
                return Ok(AcceptanceResult {
                    snapshot: SnapshotCatalogRecord::new(snapshot_id, AcceptanceStatus::Accepted),
                    receipt,
                });
            }
            if cancellation.is_cancelled() {
                return Err(AcceptanceError::Cancelled);
            }
        }

        Err(AcceptanceError::Exhausted {
            receipt: Box::new(receipt),
        })
    }
}

fn validate_config(config: &AcceptanceConfig) -> Result<(), AcceptanceError> {
    if config.maximum_attempts == 0 {
        return Err(AcceptanceError::InvalidConfig(
            "maximum_attempts must be positive",
        ));
    }
    if config.verification_timeout.is_zero() {
        return Err(AcceptanceError::InvalidConfig(
            "verification_timeout must be positive",
        ));
    }
    if config.total_deadline < config.verification_timeout {
        return Err(AcceptanceError::InvalidConfig(
            "total_deadline must cover one verification timeout",
        ));
    }
    // Exact argv, no implicit shell: reject an empty argv, an empty argument, or a
    // NUL (which no exec boundary can carry) so a malformed command never reaches
    // a real executor unquoted (RFC §6.1).
    if config.seal_at_argv.is_empty()
        || config
            .seal_at_argv
            .iter()
            .any(|arg| arg.is_empty() || arg.contains('\0'))
    {
        return Err(AcceptanceError::InvalidConfig(
            "seal_at argv must be non-empty exact arguments without NUL",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::{ContentDigest, ExecutionId};
    use capsule::snapshot_manifest::{
        PortabilityTier, RestoreContractV1, SNAPSHOT_COMPATIBILITY_V1_SCHEMA,
        SNAPSHOT_MANIFEST_V1_SCHEMA, SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA,
        SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA, SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA,
        SnapshotBackendKind, SnapshotCaptureProvenance, SnapshotCompatibilityContractV1,
    };

    use super::*;

    fn digest(fill: char) -> ContentDigest {
        ContentDigest::try_from(format!("blake3:{}", fill.to_string().repeat(64)))
            .expect("valid content digest")
    }

    fn exec_id() -> ExecutionId {
        ExecutionId::new(format!("blake3:{}", "a".repeat(64))).expect("valid execution id")
    }

    /// A valid `running` candidate manifest (the acceptance happy path).
    fn running_manifest() -> SnapshotManifestV1 {
        manifest_with_policy(CapturePolicyV1::Running)
    }

    fn manifest_with_policy(capture_policy: CapturePolicyV1) -> SnapshotManifestV1 {
        SnapshotManifestV1 {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            execution_id: exec_id(),
            compatibility_contract: SnapshotCompatibilityContractV1 {
                schema: SNAPSHOT_COMPATIBILITY_V1_SCHEMA.to_string(),
                backend: SnapshotBackendKind::Firecracker,
                format_version: 2,
                vmm_identity: "firecracker-1.7".to_string(),
                state_codec: "fc-state/v2".to_string(),
                guest_kernel_identity: "vmlinux-6.1-ato".to_string(),
                cpu_template: "T2CL".to_string(),
                runner_restore_contract: "ato-restore/v1".to_string(),
                portability_tier: PortabilityTier::ClassPortable,
                compatibility_class_identity: digest('c'),
            },
            memory_layer_refs: vec![digest('1')],
            vmstate_layer_refs: vec![digest('2')],
            disk_layer_refs: vec![digest('3')],
            restore_contract: RestoreContractV1 {
                schema: SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA.to_string(),
                restore_protocol: "ato-restore/v1".to_string(),
                steps: vec!["network_reconnect".to_string()],
            },
            capture_policy,
            capture_provenance: SnapshotCaptureProvenance::default(),
            sanitization_attestation: SanitizationAttestationV1 {
                schema: SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA.to_string(),
                steps: vec!["session_id_regenerate".to_string()],
            },
            secret_scan_attestation: SecretScanAttestationV1 {
                schema: SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA.to_string(),
                scanner_identity: "ato-secret-scan/1.0".to_string(),
                policy_identity: "default/v1".to_string(),
                scanned_layers: vec!["memory".to_string(), "vmstate".to_string()],
                verdict: "clean".to_string(),
            },
        }
    }

    /// A deterministic fake lifecycle. It hands out a fixed `running` manifest and
    /// a Session handle distinct from any candidate, and records what the loop did.
    struct FakeLifecycle {
        manifest: SnapshotManifestV1,
        outcomes: Vec<VerificationOutcome>,
        captures: u32,
        restores: u32,
        terminates: u32,
        destroys: u32,
        executed_argv: Vec<Vec<String>>,
        destroyed_sessions: Vec<DisposableSessionHandle>,
        restore_fails: bool,
        terminate_fails: bool,
        destroy_fails: bool,
        execute_delay: Duration,
        /// Optionally flip cancellation when the command is executed, to model a
        /// cancellation that races an otherwise-nominal exit.
        cancel_on_execute: Option<AcceptanceCancellation>,
        /// Panic from inside `execute_exact_argv` to model a seam method unwinding
        /// the acceptance loop while a disposable Session is live.
        panic_on_execute: bool,
    }

    impl FakeLifecycle {
        fn new(outcomes: Vec<VerificationOutcome>) -> Self {
            Self {
                manifest: running_manifest(),
                outcomes,
                captures: 0,
                restores: 0,
                terminates: 0,
                destroys: 0,
                executed_argv: Vec::new(),
                destroyed_sessions: Vec::new(),
                restore_fails: false,
                terminate_fails: false,
                destroy_fails: false,
                execute_delay: Duration::ZERO,
                cancel_on_execute: None,
                panic_on_execute: false,
            }
        }

        fn with_manifest(manifest: SnapshotManifestV1) -> Self {
            let mut fake = Self::new(Vec::new());
            fake.manifest = manifest;
            fake
        }
    }

    impl DisposableAcceptanceLifecycle for FakeLifecycle {
        fn capture_candidate(&mut self, _attempt: u32) -> Result<CandidateSnapshot, String> {
            self.captures += 1;
            Ok(CandidateSnapshot {
                manifest: self.manifest.clone(),
            })
        }

        fn create_disposable_session(
            &mut self,
            _candidate: &CandidateSnapshot,
        ) -> Result<DisposableSessionHandle, String> {
            Ok(DisposableSessionHandle {
                opaque_id: format!("disposable-session-{}", self.captures),
            })
        }

        fn restore_candidate(
            &mut self,
            _session: &DisposableSessionHandle,
            _candidate: &CandidateSnapshot,
        ) -> Result<(), String> {
            self.restores += 1;
            if self.restore_fails {
                Err("restore failed".to_string())
            } else {
                Ok(())
            }
        }

        fn execute_exact_argv(
            &mut self,
            _session: &DisposableSessionHandle,
            argv: &[String],
            _timeout: Duration,
            _cancellation: &AcceptanceCancellation,
        ) -> Result<VerificationOutcome, String> {
            self.executed_argv.push(argv.to_vec());
            if self.execute_delay > Duration::ZERO {
                std::thread::sleep(self.execute_delay);
            }
            if let Some(cancellation) = &self.cancel_on_execute {
                cancellation.cancel();
            }
            if self.panic_on_execute {
                panic!("seam method unwinds while a disposable Session is live");
            }
            Ok(self.outcomes.remove(0))
        }

        fn terminate_process_tree(
            &mut self,
            _session: &DisposableSessionHandle,
        ) -> Result<(), String> {
            self.terminates += 1;
            if self.terminate_fails {
                Err("terminate failed".to_string())
            } else {
                Ok(())
            }
        }

        fn destroy_disposable_session(
            &mut self,
            session: DisposableSessionHandle,
        ) -> Result<(), String> {
            self.destroys += 1;
            self.destroyed_sessions.push(session);
            if self.destroy_fails {
                Err("destroy failed".to_string())
            } else {
                Ok(())
            }
        }
    }

    fn config(maximum_attempts: u32) -> AcceptanceConfig {
        AcceptanceConfig {
            seal_at_argv: vec![
                "npm".to_string(),
                "run".to_string(),
                "verify-ready".to_string(),
            ],
            verification_timeout: Duration::from_secs(1),
            total_deadline: Duration::from_secs(10),
            maximum_attempts,
        }
    }

    fn eligible() -> SnapshotEligibility {
        SnapshotEligibility {
            external_state_required_by_live_workload: false,
        }
    }

    // --- AC: exit 0 is the ONLY success; exact argv preserved ---
    #[test]
    fn only_exit_zero_accepts_and_exact_argv_is_preserved() {
        // A non-zero exit is rejected; the next attempt's exit 0 accepts.
        let mut lifecycle = FakeLifecycle::new(vec![
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(0),
        ]);
        let result = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(2),
            &Default::default(),
        )
        .expect("second attempt (exit 0) accepts");

        assert_eq!(lifecycle.executed_argv, vec![config(2).seal_at_argv; 2]);
        assert_eq!(result.receipt.attempts[0].outcome, "nonzero-exit");
        assert_eq!(result.receipt.attempts[1].outcome, "accepted");
        assert!(result.snapshot.is_accepted());

        // A signal-terminated command (surfaced as a non-zero code) is rejected.
        let mut signalled = FakeLifecycle::new(vec![VerificationOutcome::Exited(137)]);
        let error = RunningSnapshotAcceptance::accept(
            &mut signalled,
            eligible(),
            &config(1),
            &Default::default(),
        )
        .expect_err("a signalled (non-zero) command never accepts");
        let AcceptanceError::Exhausted { receipt } = error else {
            panic!("expected exhausted");
        };
        assert_eq!(receipt.attempts[0].outcome, "nonzero-exit");
    }

    // --- AC: a signal-terminated process is REJECTED, never accepted ---
    // exit-0-only is structural: a `Signalled` outcome has no exit code, so it can
    // never be `Exited(0)` and can never reach the sole accept arm.
    #[test]
    fn signalled_outcome_is_rejected_and_destroys_session() {
        // SIGKILL (9): a signal is never success.
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Signalled(9)]);
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(1),
            &Default::default(),
        )
        .expect_err("a signalled process never accepts");

        let AcceptanceError::Exhausted { receipt } = error else {
            panic!("expected exhausted");
        };
        assert_eq!(receipt.attempts.len(), 1);
        assert_eq!(receipt.attempts[0].outcome, "signalled");
        assert!(
            receipt.accepted_snapshot_id.is_none(),
            "a signalled outcome must never be accepted"
        );
        // Reject paths still tear the disposable Session down (cleanup).
        assert_eq!(lifecycle.destroys, 1);
        assert_eq!(lifecycle.destroyed_sessions.len(), 1);
    }

    // --- AC: a lost / undecodable child is REJECTED, never accepted ---
    // Like `Signalled`, `Lost` carries no exit code and cannot be `Exited(0)`.
    #[test]
    fn lost_outcome_is_rejected_and_destroys_session() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Lost]);
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(1),
            &Default::default(),
        )
        .expect_err("a lost child never accepts");

        let AcceptanceError::Exhausted { receipt } = error else {
            panic!("expected exhausted");
        };
        assert_eq!(receipt.attempts.len(), 1);
        assert_eq!(receipt.attempts[0].outcome, "lost");
        assert!(
            receipt.accepted_snapshot_id.is_none(),
            "a lost outcome must never be accepted"
        );
        // Reject paths still tear the disposable Session down (cleanup).
        assert_eq!(lifecycle.destroys, 1);
        assert_eq!(lifecycle.destroyed_sessions.len(), 1);
    }

    // --- AC: verification side effects do not alter accepted bytes;
    //         candidate + accepted bytes immutable ---
    #[test]
    fn accepted_bytes_are_the_unchanged_candidate_bytes() {
        let manifest = running_manifest();
        // The address the candidate commits to, computed independently of the run.
        let expected_id = manifest.snapshot_id().expect("valid snapshot id");
        let mut lifecycle = FakeLifecycle::with_manifest(manifest.clone());
        lifecycle.outcomes = vec![VerificationOutcome::Exited(0)];

        let result = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(1),
            &Default::default(),
        )
        .expect("running candidate accepts");

        // The accepted record pins exactly the candidate's content address: the
        // disposable-restore + command produced no change to the accepted bytes.
        assert_eq!(result.snapshot.snapshot_id, expected_id);
        assert_eq!(
            result.receipt.accepted_snapshot_id.as_deref(),
            Some(expected_id.as_str())
        );
        // The fake's source manifest is byte-identical after the run: verification
        // never mutated the candidate.
        assert_eq!(lifecycle.manifest, manifest);
        // The verification wrote only to a disposable Session that was destroyed,
        // and that Session is not the candidate.
        assert_eq!(lifecycle.destroyed_sessions.len(), 1);
        assert!(
            lifecycle.destroyed_sessions[0]
                .opaque_id
                .starts_with("disposable-session-")
        );
        assert_eq!(lifecycle.restores, 1);
    }

    // --- AC: cleanup on success, failure, timeout, and cancellation ---
    #[test]
    fn session_is_always_destroyed_across_outcomes() {
        // success
        let mut ok = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        RunningSnapshotAcceptance::accept(&mut ok, eligible(), &config(1), &Default::default())
            .unwrap();
        assert_eq!(ok.destroys, 1);

        // non-zero failure
        let mut fail = FakeLifecycle::new(vec![VerificationOutcome::Exited(2)]);
        RunningSnapshotAcceptance::accept(&mut fail, eligible(), &config(1), &Default::default())
            .unwrap_err();
        assert_eq!(fail.destroys, 1);
        assert_eq!(fail.terminates, 0); // a clean non-zero exit needs no tree kill

        // timeout: full process tree terminated AND session destroyed
        let mut timed = FakeLifecycle::new(vec![VerificationOutcome::TimedOut]);
        RunningSnapshotAcceptance::accept(&mut timed, eligible(), &config(1), &Default::default())
            .unwrap_err();
        assert_eq!(timed.terminates, 1);
        assert_eq!(timed.destroys, 1);
        assert_eq!(timed.destroyed_sessions.len(), 1);

        // cancellation racing a nominal exit: tree terminated AND session destroyed
        let cancellation = AcceptanceCancellation::default();
        let mut cancelled = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        cancelled.cancel_on_execute = Some(cancellation.clone());
        let error = RunningSnapshotAcceptance::accept(
            &mut cancelled,
            eligible(),
            &config(1),
            &cancellation,
        )
        .unwrap_err();
        assert_eq!(error, AcceptanceError::Cancelled);
        assert_eq!(cancelled.terminates, 1);
        assert_eq!(cancelled.destroys, 1);
    }

    // --- AC: cleanup runs even when process-tree termination itself fails ---
    #[test]
    fn termination_failure_still_destroys_and_surfaces_cleanup() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::TimedOut]);
        lifecycle.terminate_fails = true;
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(1),
            &Default::default(),
        )
        .unwrap_err();

        assert_eq!(error, AcceptanceError::Cleanup);
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1); // destroy attempted despite terminate failure
    }

    // --- AC: a panic mid-verification still tears the disposable Session down ---
    // In PR-2 the Session handle owns a real firecracker microVM + overlay, so an
    // unwind between create and destroy would leak a live VM. The RAII guard's Drop
    // is the safety net: destroy MUST still run when a seam method panics.
    #[test]
    fn panic_during_execute_still_destroys_session() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let mut lifecycle = FakeLifecycle::new(Vec::new());
        lifecycle.panic_on_execute = true;

        // Silence the default panic hook's backtrace print for this expected panic.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            RunningSnapshotAcceptance::accept(
                &mut lifecycle,
                eligible(),
                &config(1),
                &Default::default(),
            )
        }));
        std::panic::set_hook(previous_hook);

        assert!(
            unwound.is_err(),
            "the execute panic must unwind out of accept, not be swallowed"
        );
        // The guard's Drop ran on unwind: the process tree was terminated and the
        // disposable Session was destroyed exactly once, so no live VM is leaked.
        assert_eq!(lifecycle.destroys, 1, "destroy must run on the panic path");
        assert_eq!(lifecycle.destroyed_sessions.len(), 1);
        assert_eq!(
            lifecycle.terminates, 1,
            "the process tree must be terminated before destroy on the panic path"
        );
    }

    // --- AC: recapture retries are bounded (by count AND deadline) and receipted ---
    #[test]
    fn recapture_retries_are_bounded_and_receipted() {
        // Bounded by maximum_attempts: three non-zero exits ⇒ three receipted
        // attempts, then exhaustion (no unbounded looping).
        let mut by_count = FakeLifecycle::new(vec![
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(1),
        ]);
        let error = RunningSnapshotAcceptance::accept(
            &mut by_count,
            eligible(),
            &config(3),
            &Default::default(),
        )
        .unwrap_err();
        let AcceptanceError::Exhausted { receipt } = error else {
            panic!("expected exhausted");
        };
        assert_eq!(by_count.captures, 3);
        assert_eq!(receipt.attempts.len(), 3);
        assert_eq!(receipt.maximum_attempts, 3);
        for (i, attempt) in receipt.attempts.iter().enumerate() {
            assert_eq!(attempt.attempt, i as u32 + 1);
            assert_eq!(attempt.outcome, "nonzero-exit");
            assert!(attempt.candidate_snapshot_id.is_some());
        }

        // Bounded by deadline: a per-attempt delay past a tiny total deadline stops
        // recapture even though the attempt budget is not exhausted.
        let mut by_deadline = FakeLifecycle::new(vec![VerificationOutcome::Exited(1)]);
        by_deadline.execute_delay = Duration::from_millis(5);
        let deadline_config = AcceptanceConfig {
            verification_timeout: Duration::from_millis(1),
            total_deadline: Duration::from_millis(1),
            maximum_attempts: 100,
            ..config(1)
        };
        RunningSnapshotAcceptance::accept(
            &mut by_deadline,
            eligible(),
            &deadline_config,
            &Default::default(),
        )
        .unwrap_err();
        assert!(
            by_deadline.captures < 100,
            "deadline must bound recapture below the attempt cap, got {} captures",
            by_deadline.captures
        );
    }

    // --- AC: External State for a live workload fails eligibility CLOSED ---
    #[test]
    fn external_state_live_workload_fails_closed_before_any_capture() {
        let mut lifecycle = FakeLifecycle::new(Vec::new());
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            SnapshotEligibility {
                external_state_required_by_live_workload: true,
            },
            &config(1),
            &Default::default(),
        )
        .unwrap_err();

        assert_eq!(error, AcceptanceError::ExternalStateRequiresWorkloadIdle);
        // Fails closed with NO capture, restore, or Session created — no chance to
        // bind production secrets / user state during acceptance.
        assert_eq!(lifecycle.captures, 0);
        assert_eq!(lifecycle.restores, 0);
        assert_eq!(lifecycle.destroys, 0);
    }

    // --- AC: the secret scan is recorded as an ATTESTATION, not proof of absence ---
    #[test]
    fn receipt_records_secret_scan_as_attestation_not_proof() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let result = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(1),
            &Default::default(),
        )
        .unwrap();

        // Capture policy + both attestations are recorded on acceptance.
        assert_eq!(result.receipt.capture_policy, CapturePolicyV1::Running);
        let sanitization = result
            .receipt
            .sanitization_attestation
            .expect("sanitization attestation recorded");
        assert_eq!(
            sanitization.steps,
            vec!["session_id_regenerate".to_string()]
        );

        let secret_scan = result
            .receipt
            .secret_scan_attestation
            .expect("secret-scan attestation recorded");
        // It is an attestation that a scan ran (scanner + policy + redacted
        // verdict), never a boolean "no secrets present" proof of absence.
        assert_eq!(secret_scan.scanner_identity, "ato-secret-scan/1.0");
        assert_eq!(secret_scan.policy_identity, "default/v1");
        assert_eq!(secret_scan.verdict, "clean");

        // The type-level contract: the attestation is a redacted verdict, and the
        // module never treats "clean" as a proof of absence — there is no boolean
        // `secrets_absent` field to mistake for one.
        let json = serde_json::to_string(&secret_scan).unwrap();
        assert!(!json.contains("proof"));
        assert!(!json.contains("secrets_absent"));
    }

    // --- Capture-policy gate: a non-`running` candidate is rejected explicitly ---
    #[test]
    fn non_running_candidate_manifest_is_rejected_with_unsupported_policy() {
        let mut lifecycle =
            FakeLifecycle::with_manifest(manifest_with_policy(CapturePolicyV1::WorkloadIdle));
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(1),
            &Default::default(),
        )
        .unwrap_err();

        assert_eq!(error, AcceptanceError::UnsupportedCapturePolicy);
        // Rejected on the candidate manifest itself, before any Session work.
        assert_eq!(lifecycle.restores, 0);
        assert_eq!(lifecycle.destroys, 0);
    }

    // --- Config validation: empty/NUL argv, zero timeout, zero attempts ---
    #[test]
    fn invalid_configurations_fail_closed() {
        let mut lifecycle = FakeLifecycle::new(Vec::new());
        let bad = [
            AcceptanceConfig {
                seal_at_argv: Vec::new(),
                ..config(1)
            },
            AcceptanceConfig {
                seal_at_argv: vec!["ok".to_string(), "bad\0arg".to_string()],
                ..config(1)
            },
            AcceptanceConfig {
                verification_timeout: Duration::ZERO,
                ..config(1)
            },
            AcceptanceConfig {
                maximum_attempts: 0,
                ..config(1)
            },
        ];
        for cfg in bad {
            let error = RunningSnapshotAcceptance::accept(
                &mut lifecycle,
                eligible(),
                &cfg,
                &Default::default(),
            )
            .unwrap_err();
            assert!(matches!(error, AcceptanceError::InvalidConfig(_)));
        }
        // No capture happens for any invalid config.
        assert_eq!(lifecycle.captures, 0);
    }

    // --- Pre-cancellation creates no resources ---
    #[test]
    fn pre_cancelled_run_creates_no_resources() {
        let cancellation = AcceptanceCancellation::default();
        cancellation.cancel();
        let mut lifecycle = FakeLifecycle::new(Vec::new());
        let error = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligible(),
            &config(1),
            &cancellation,
        )
        .unwrap_err();
        assert_eq!(error, AcceptanceError::Cancelled);
        assert_eq!(lifecycle.captures, 0);
    }
}
