//! Submission-wizard **HOLD phase** orchestration (ato-wizard PR-2, slice 1).
//!
//! This is the pure, KVM-free state machine that drives a builder's interactive
//! `hold → capture → accept` loop for the submission wizard. It is deliberately
//! free of live-VM, network, and api transport: every side effect enters through
//! an injected **seam** (a small local trait or the existing #1088 acceptance
//! seam), so the whole orchestration is unit-testable with fakes — mirroring the
//! `snapshot::acceptance` FakeLifecycle/FakeClock pattern.
//!
//! Design references (recovered arch doc §4.2 + `docs/contracts/SUBMISSION_WIZARD_WIRE_V1.md`):
//!
//! - **ADR-001** — capture = builder-resident hold phase + submitter-triggered
//!   capture + source-VM resume; acceptance runs through #1088.
//! - **ADR-007 / SSOT §3, §5** — capture is refused until the control channel
//!   reports `pause_permitted` (the api sets it only after the proxy's
//!   `quiesced { inflight: 0 }` ack). No capture before the quiesced ack. The
//!   hold deadline is fail-closed: a capture is never forced past it.
//! - **ADR-008** — `capture_epoch` is a monotonic command cursor (adopted from
//!   the [`ControlResponse`]), NOT part of FENCING-4. The machine ENFORCES this
//!   monotonicity on capture, not merely on polling: a `Capture` directive whose
//!   epoch is `<=` the last epoch already captured is ignored — a stale or
//!   duplicate command never re-drives capture. In particular, after an ADR-012
//!   source-available acceptance failure returns to holding, a replayed command
//!   carrying the same (or an older) epoch cannot trigger a second capture; only
//!   a strictly-newer epoch proceeds.
//! - **ADR-012** — after capture, `accepting_source_available` (resume ok →
//!   acceptance failure returns to holding, re-capture possible) vs
//!   `accepting_source_lost` (resume failed → acceptance failure ends the attempt,
//!   no re-capture).
//!
//! The **Firecracker-concrete** capture IO lives behind the [`CaptureAction`]
//! seam (in prod: `FirecrackerBackend::capture_running_candidate`,
//! pause→snapshot/create→resume keeping the guest alive — verified on real
//! hardware in a follow-up). The #1088 acceptance runs through the EXISTING
//! [`DisposableAcceptanceLifecycle`] trait and [`RunningSnapshotAcceptance::accept`].
//!
//! **External state stays deferred (warm-cache-only slice):** eligibility enters
//! through [`EligibilitySource`], which must fail closed for any capsule that
//! requires External State or restore-time secret bindings (#1090). Wiring the
//! real [`VerifiedRunningSnapshotEligibility::analyze_execution_contract`] over a
//! finalized `ExecutionContractEnvelopeV1` in `produce_build` is a later PR-2
//! slice; this slice constructs no production eligibility and no
//! `TrustedProductionStateRef` / `VerifiedCaptureTopology`.
//!
//! **Dead-code allow (scoped to this module):** `snapshot-builder` is a *binary*
//! crate, so `pub` items count as dead unless reached from `fn main`. The
//! production consumer — the live boot-to-hold session in
//! `process_interactive_capture_job` — is a later PR-2 slice, so nothing in the
//! non-test binary constructs a [`HoldPhase`] yet. The orchestration is exercised
//! in full by this module's own KVM-free unit tests. The allow is intentionally
//! module-scoped (not crate-wide) and removed when the live wiring lands.
#![allow(dead_code)]

use std::time::Duration;

use snapshot::acceptance::{
    AcceptanceConfig, AcceptanceFailure, DisposableAcceptanceLifecycle, FatalInternalError,
    MonotonicClock, RunningSnapshotAcceptance, VerifiedRunningSnapshotEligibility,
};

use crate::wizard_wire::{
    CandidateReportRequest, ControlDirective, ControlResponse, Fencing4, TerminalAckReason,
    WizardFailureStage,
};

/// Default hold TTL (USER DECISION): 30 minutes, with explicit extend via the
/// [`ExtendPolicy`] seam.
pub const DEFAULT_HOLD_TTL: Duration = Duration::from_secs(30 * 60);

/// The control-poll source (SSOT §3.3). Yields a [`ControlResponse`] carrying the
/// directive (`hold | capture | discard`), the authoritative `server_capture_epoch`
/// (adopted as the observed command cursor), and — critically — `pause_permitted`
/// (ADR-007 causality). In prod this is the builder's control-poll HTTP client;
/// tests script a fixed sequence.
pub trait ControlSource {
    /// Poll the control channel, reporting the highest epoch observed so far.
    fn poll(&mut self, observed_capture_epoch: u64) -> ControlResponse;
}

/// The Firecracker-concrete capture seam. In prod this pauses the live held guest,
/// snapshots it, and resumes the source (keeping the guest alive), producing the
/// candidate that becomes the published Snapshot. A capture that fails before seal
/// yields a [`CaptureError`]; a capture whose source could not be resumed yields a
/// [`HeldCapture`] with `source_lost = true` (ADR-012).
pub trait CaptureAction {
    /// Capture an immutable candidate for `capture_epoch` from the live held guest.
    fn capture(&mut self, capture_epoch: u64) -> Result<HeldCapture, CaptureError>;
}

/// The eligibility seam (#1090, fail-closed). In prod this analyzes the finalized
/// Execution Contract and fails closed when the live workload requires External
/// State or restore-time secret bindings; this slice keeps that wiring deferred.
/// [`HoldPhase`] consults it at entry (never entering the hold loop when
/// ineligible) and once per candidate to seed [`RunningSnapshotAcceptance::accept`],
/// which consumes the proof by value.
pub trait EligibilitySource {
    /// Mint a fresh eligibility proof, or fail closed.
    fn eligibility(&mut self) -> Result<VerifiedRunningSnapshotEligibility, AcceptanceFailure>;
}

/// The hold-deadline extend seam (USER DECISION: explicit extend). Consulted when
/// the hold TTL is reached: `Some(extra)` extends the deadline by `extra` and
/// keeps holding; `None` ends the attempt fail-closed (no forced capture, SSOT §5).
pub trait ExtendPolicy {
    /// Called when the hold deadline is reached.
    fn on_deadline(&mut self) -> Option<Duration>;
}

/// An [`ExtendPolicy`] that never extends — the hold ends at the TTL.
pub struct NoExtend;

impl ExtendPolicy for NoExtend {
    fn on_deadline(&mut self) -> Option<Duration> {
        None
    }
}

/// One captured candidate from the live held guest (the report inputs the builder
/// would send on §3.6). `source_lost` records whether the source guest could be
/// resumed (ADR-012).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldCapture {
    /// The pre-minted candidate id the control channel delivered for this epoch.
    pub candidate_id: String,
    /// The canonical Execution Identity of the captured execution.
    pub execution_id: String,
    /// The sealed snapshot id for this candidate.
    pub snapshot_id: String,
    /// The candidate's durable artifact location.
    pub artifact_location: String,
    /// `true` ⇒ the source guest was lost during/after capture (resume failed,
    /// ADR-012 `accepting_source_lost`).
    pub source_lost: bool,
}

/// A capture that failed before producing a candidate (SSOT §3.6: no candidate
/// report). With the source alive the attempt returns to holding; if the source
/// was also lost, the attempt ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    /// `true` ⇒ the source guest is gone (ADR-012): the attempt ends rather than
    /// returning to holding.
    pub source_lost: bool,
    /// Diagnostic detail.
    pub message: String,
}

/// The terminal outcome of a hold phase, projected to the wizard terminal-ack
/// reason (SSOT §3.8) via [`Self::terminal_ack_reason`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldTermination {
    /// The candidate was accepted (#1088). This slice ends the attempt here;
    /// carries the §3.6 candidate report the builder would send.
    Accepted {
        /// The candidate report for the accepted capture.
        report: CandidateReportRequest,
    },
    /// The control channel directed `discard`.
    Discarded,
    /// The hold TTL/deadline was reached with no extend (orderly end).
    AttemptEnded,
    /// ADR-012 terminal branch: the source was lost AND the capture/acceptance
    /// failed — no re-capture.
    AcceptanceFailedSourceLost {
        /// Diagnostic detail (≤ 1800 chars once truncated by the ack builder).
        failure_reason: String,
    },
    /// Fail-closed refusal before ever entering capture — e.g. an ineligible
    /// capsule (External State / restore-time bindings required, #1090). The
    /// interactive attempt never reached a valid holding capture.
    FailedClosed {
        /// Diagnostic detail.
        failure_reason: String,
    },
}

impl HoldTermination {
    /// Project to the ONLY legal wizard job-terminal reasons (SSOT §3.8).
    /// `accepted` ends this slice's attempt as an orderly end (there is no
    /// job-terminal "accepted" reason: acceptance is a per-candidate endpoint in
    /// the full flow, §3.7).
    pub fn terminal_ack_reason(&self) -> TerminalAckReason {
        match self {
            HoldTermination::Accepted { .. } | HoldTermination::AttemptEnded => {
                TerminalAckReason::AttemptEnded
            }
            HoldTermination::Discarded => TerminalAckReason::Discarded,
            HoldTermination::AcceptanceFailedSourceLost { .. } => {
                TerminalAckReason::AcceptanceFailedSourceLost
            }
            HoldTermination::FailedClosed { .. } => TerminalAckReason::BuildFailed,
        }
    }

    /// Optional diagnostic refinement of the terminal reason (SSOT §2/§3.8).
    pub fn failure_stage(&self) -> Option<WizardFailureStage> {
        match self {
            HoldTermination::AcceptanceFailedSourceLost { .. } => {
                Some(WizardFailureStage::Acceptance)
            }
            HoldTermination::FailedClosed { .. } => Some(WizardFailureStage::Holding),
            _ => None,
        }
    }
}

/// The pure hold-phase driver. Generic only over the acceptance lifecycle `L`
/// (which [`RunningSnapshotAcceptance::accept`] takes as a `Sized` `impl`); every
/// other seam is a `&mut dyn` trait object to keep the type small.
pub struct HoldPhase<'a, L: DisposableAcceptanceLifecycle> {
    control: &'a mut dyn ControlSource,
    capture: &'a mut dyn CaptureAction,
    eligibility: &'a mut dyn EligibilitySource,
    extend: &'a mut dyn ExtendPolicy,
    lifecycle: &'a mut L,
    clock: &'a dyn MonotonicClock,
    cancellation: &'a snapshot::acceptance::AcceptanceCancellation,
    fencing: Fencing4,
    acceptance_config: AcceptanceConfig,
    hold_ttl: Duration,
}

impl<'a, L: DisposableAcceptanceLifecycle> HoldPhase<'a, L> {
    /// Assemble a hold phase from its seams. `fencing` is the attempt's FENCING-4
    /// identity (echoed into candidate reports); `acceptance_config` is the #1088
    /// config (exact `seal_at` argv + bounds); `hold_ttl` is the hold deadline
    /// budget ([`DEFAULT_HOLD_TTL`] by default).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        control: &'a mut dyn ControlSource,
        capture: &'a mut dyn CaptureAction,
        eligibility: &'a mut dyn EligibilitySource,
        extend: &'a mut dyn ExtendPolicy,
        lifecycle: &'a mut L,
        clock: &'a dyn MonotonicClock,
        cancellation: &'a snapshot::acceptance::AcceptanceCancellation,
        fencing: Fencing4,
        acceptance_config: AcceptanceConfig,
        hold_ttl: Duration,
    ) -> Self {
        Self {
            control,
            capture,
            eligibility,
            extend,
            lifecycle,
            clock,
            cancellation,
            fencing,
            acceptance_config,
            hold_ttl,
        }
    }

    /// Drive the hold loop to a terminal outcome.
    ///
    /// Order (ADR-001/007/012, SSOT §5):
    /// 1. **Entry eligibility gate** (#1090 fail-closed): if the capsule is
    ///    ineligible the loop is never entered — no poll, no capture.
    /// 2. Loop: the **fail-closed deadline gate** runs first (consulting
    ///    [`ExtendPolicy`]); then a control poll adopts the server epoch and
    ///    dispatches on the directive.
    /// 3. `capture` is **refused** unless `pause_permitted` (ADR-007) — never a
    ///    capture before the quiesced ack — and unless the epoch is strictly
    ///    newer than the last captured epoch (ADR-008 monotonicity: a stale or
    ///    duplicate command never re-drives capture, e.g. after a source-available
    ///    return to holding).
    /// 4. On capture, run the capture-action, then #1088 acceptance:
    ///    - accepted → terminal ([`HoldTermination::Accepted`]);
    ///    - acceptance failed + source available → return to holding (re-capture);
    ///    - source lost (capture failure or acceptance failure) → terminal
    ///      ([`HoldTermination::AcceptanceFailedSourceLost`], no re-capture).
    ///
    /// A [`FatalInternalError`] from the acceptance loop is surfaced (it is a
    /// genuinely receipt-less fault, distinct from a receipted rejection).
    pub fn run(&mut self) -> Result<HoldTermination, FatalInternalError> {
        // (1) Entry eligibility gate — fail CLOSED before any hold/poll/capture so
        // an ineligible capsule (External State / restore-time bindings, #1090)
        // can never reach a capture. Keep the first proof to seed the first
        // acceptance without re-analyzing.
        let mut pending_eligibility = match self.eligibility.eligibility() {
            Ok(proof) => Some(proof),
            Err(failure) => {
                return Ok(HoldTermination::FailedClosed {
                    failure_reason: failure.to_string(),
                });
            }
        };

        let mut observed_epoch: u64 = 0;
        // The highest epoch that has already driven a capture (ADR-008). Guards
        // against a stale/duplicate Capture command re-driving capture after a
        // source-available return to holding. `None` until the first capture.
        let mut last_captured_epoch: Option<u64> = None;
        let mut deadline = self.clock.now() + self.hold_ttl;

        loop {
            // (2) Fail-closed deadline gate FIRST: never force a capture past the
            // hold deadline (SSOT §5). On reaching it, consult the extend seam.
            if self.clock.now() >= deadline {
                match self.extend.on_deadline() {
                    Some(extra) => {
                        deadline = self.clock.now() + extra;
                        continue;
                    }
                    None => return Ok(HoldTermination::AttemptEnded),
                }
            }

            let response = self.control.poll(observed_epoch);
            // Adopt the authoritative server epoch as the observed command cursor
            // (ADR-008 / ControlResponse rules): monotonic, never part of fencing.
            observed_epoch = observed_epoch.max(response.server_capture_epoch);

            match response.directive {
                ControlDirective::Hold => continue,
                ControlDirective::Discard => return Ok(HoldTermination::Discarded),
                ControlDirective::Capture => {
                    // (3) ADR-007 causality: refuse capture until the quiesced ack
                    // has set pause_permitted. No capture before the quiesced ack.
                    if !response.pause_permitted {
                        continue;
                    }
                    let epoch = response.server_capture_epoch;

                    // (3a) ADR-008 capture-epoch monotonicity: the epoch is a
                    // monotonic command cursor, enforced on capture (not just on
                    // polling). A stale or duplicate Capture whose epoch is `<=`
                    // the last epoch already captured must never re-drive capture
                    // — in particular, after an ADR-012 source-available
                    // acceptance failure returns to holding, a replayed command
                    // with the same (or an older) epoch is ignored. Only a
                    // strictly-newer epoch proceeds. Checked AFTER the
                    // `pause_permitted` gate so an unpermitted directive never
                    // consumes an epoch (a later permitted retry of the same epoch
                    // must still capture).
                    if matches!(last_captured_epoch, Some(last) if epoch <= last) {
                        continue;
                    }

                    // (4) Firecracker-concrete capture for this epoch.
                    let held = match self.capture.capture(epoch) {
                        Ok(held) => held,
                        Err(err) => {
                            // No candidate report (SSOT §3.6). Source alive →
                            // return to holding; source lost → terminal (ADR-012).
                            if err.source_lost {
                                return Ok(HoldTermination::AcceptanceFailedSourceLost {
                                    failure_reason: err.message,
                                });
                            }
                            continue;
                        }
                    };
                    // Record the captured epoch (ADR-008): a later return to
                    // holding must not let a stale/duplicate command re-capture it.
                    last_captured_epoch = Some(epoch);

                    // Fresh eligibility proof to seed accept() (it consumes the
                    // proof by value); re-analysis on later captures fails closed.
                    let eligibility = match pending_eligibility.take() {
                        Some(proof) => proof,
                        None => match self.eligibility.eligibility() {
                            Ok(proof) => proof,
                            Err(failure) => {
                                return Ok(HoldTermination::FailedClosed {
                                    failure_reason: failure.to_string(),
                                });
                            }
                        },
                    };

                    // #1088 acceptance via the EXISTING disposable-restore lifecycle.
                    let run = RunningSnapshotAcceptance::accept(
                        &mut *self.lifecycle,
                        eligibility,
                        &self.acceptance_config,
                        self.cancellation,
                        self.clock,
                    )?;

                    if run.is_accepted() {
                        return Ok(HoldTermination::Accepted {
                            report: self.candidate_report(epoch, &held),
                        });
                    }

                    // Acceptance failed. ADR-012 branch on source availability.
                    if held.source_lost {
                        let reason = run
                            .failure()
                            .map(|f| f.to_string())
                            .unwrap_or_else(|| "acceptance rejected".to_string());
                        return Ok(HoldTermination::AcceptanceFailedSourceLost {
                            failure_reason: reason,
                        });
                    }
                    // accepting_source_available → return to holding, re-capture
                    // possible (ADR-012). Keep polling.
                }
            }
        }
    }

    /// Build the §3.6 candidate report for an accepted capture, echoing the
    /// attempt's FENCING-4 identity and the adopted `capture_epoch`.
    fn candidate_report(&self, capture_epoch: u64, held: &HeldCapture) -> CandidateReportRequest {
        CandidateReportRequest {
            submission_attempt_id: self.fencing.submission_attempt_id.clone(),
            worker_claim_id: self.fencing.worker_claim_id.clone(),
            capture_epoch,
            candidate_id: held.candidate_id.clone(),
            execution_id: held.execution_id.clone(),
            snapshot_id: held.snapshot_id.clone(),
            artifact_location: held.artifact_location.clone(),
            source_lost: held.source_lost,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    use capsule::execution_contract::{ContentDigest, ExecutionId};
    use capsule::snapshot_manifest::CapturePolicyV1;
    use capsule::snapshot_manifest::{
        PortabilityTier, RestoreContractV1, SNAPSHOT_COMPATIBILITY_V1_SCHEMA,
        SNAPSHOT_MANIFEST_V1_SCHEMA, SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA,
        SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA, SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA,
        SanitizationAttestationV1, SecretScanAttestationV1, SnapshotBackendKind,
        SnapshotCaptureProvenance, SnapshotCompatibilityContractV1, SnapshotManifestV1,
    };
    use snapshot::acceptance::{
        AcceptanceBudget, AcceptanceCancellation, CandidateSnapshot, DisposableSessionHandle,
        VerificationOutcome,
    };

    use super::*;

    // ── shared fixtures (mirror snapshot::acceptance test helpers) ──────────

    fn digest(fill: char) -> ContentDigest {
        ContentDigest::try_from(format!("blake3:{}", fill.to_string().repeat(64)))
            .expect("valid content digest")
    }

    fn exec_id() -> ExecutionId {
        ExecutionId::new(format!("blake3:{}", "a".repeat(64))).expect("valid execution id")
    }

    /// A `running` manifest whose Execution Identity matches the eligibility proof
    /// the fakes seed (accept binds every candidate to the proof's identity).
    fn running_manifest() -> SnapshotManifestV1 {
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
            capture_policy: CapturePolicyV1::Running,
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

    /// A controllable monotonic clock, mirroring `snapshot::acceptance`'s FakeClock.
    #[derive(Clone)]
    struct FakeClock {
        base: Instant,
        elapsed_nanos: Arc<AtomicU64>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                elapsed_nanos: Arc::new(AtomicU64::new(0)),
            }
        }

        fn advance(&self, by: Duration) {
            self.elapsed_nanos
                .fetch_add(by.as_nanos() as u64, Ordering::SeqCst);
        }
    }

    impl MonotonicClock for FakeClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_nanos(self.elapsed_nanos.load(Ordering::SeqCst))
        }
    }

    /// A deterministic acceptance lifecycle, mirroring `snapshot::acceptance`'s
    /// FakeLifecycle: one fixed `running` manifest, a scripted verification outcome,
    /// and call counters.
    struct FakeLifecycle {
        manifest: SnapshotManifestV1,
        outcomes: Vec<VerificationOutcome>,
        captures: u32,
        executes: u32,
    }

    impl FakeLifecycle {
        fn new(outcomes: Vec<VerificationOutcome>) -> Self {
            Self {
                manifest: running_manifest(),
                outcomes,
                captures: 0,
                executes: 0,
            }
        }
    }

    impl DisposableAcceptanceLifecycle for FakeLifecycle {
        fn capture_candidate(
            &mut self,
            _attempt: u32,
            _budget: &AcceptanceBudget,
        ) -> Result<CandidateSnapshot, String> {
            self.captures += 1;
            Ok(CandidateSnapshot {
                manifest: self.manifest.clone(),
            })
        }

        fn create_disposable_session(
            &mut self,
            _candidate: &CandidateSnapshot,
            _budget: &AcceptanceBudget,
        ) -> Result<DisposableSessionHandle, String> {
            Ok(DisposableSessionHandle {
                opaque_id: format!("disposable-{}", self.captures),
            })
        }

        fn restore_candidate(
            &mut self,
            _session: &DisposableSessionHandle,
            _candidate: &CandidateSnapshot,
            _budget: &AcceptanceBudget,
        ) -> Result<(), String> {
            Ok(())
        }

        fn execute_exact_argv(
            &mut self,
            _session: &DisposableSessionHandle,
            _argv: &[String],
            _timeout: Duration,
            _budget: &AcceptanceBudget,
        ) -> Result<VerificationOutcome, String> {
            self.executes += 1;
            Ok(self.outcomes.remove(0))
        }

        fn terminate_process_tree(
            &mut self,
            _session: &DisposableSessionHandle,
        ) -> Result<(), String> {
            Ok(())
        }

        fn destroy_disposable_session(
            &mut self,
            _session: DisposableSessionHandle,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    /// A scripted control-poll source. Returns `responses[i]`, clamping to the last
    /// entry once exhausted (so a trailing `hold` repeats), and optionally advances
    /// a shared clock on each poll (to drive the TTL test without sleeping).
    struct ScriptedControl {
        responses: Vec<ControlResponse>,
        idx: usize,
        polls: usize,
        clock: Option<FakeClock>,
        advance_per_poll: Duration,
    }

    impl ScriptedControl {
        fn new(responses: Vec<ControlResponse>) -> Self {
            Self {
                responses,
                idx: 0,
                polls: 0,
                clock: None,
                advance_per_poll: Duration::ZERO,
            }
        }

        fn advancing(
            clock: FakeClock,
            per_poll: Duration,
            responses: Vec<ControlResponse>,
        ) -> Self {
            Self {
                responses,
                idx: 0,
                polls: 0,
                clock: Some(clock),
                advance_per_poll: per_poll,
            }
        }
    }

    impl ControlSource for ScriptedControl {
        fn poll(&mut self, _observed_capture_epoch: u64) -> ControlResponse {
            self.polls += 1;
            if let Some(clock) = &self.clock {
                clock.advance(self.advance_per_poll);
            }
            let i = self.idx.min(self.responses.len() - 1);
            self.idx += 1;
            self.responses[i].clone()
        }
    }

    /// A scripted capture-action: records call count and yields a configured
    /// result. `source_lost` rides the produced [`HeldCapture`].
    struct ScriptedCapture {
        result: Result<HeldCapture, CaptureError>,
        calls: u32,
    }

    impl ScriptedCapture {
        fn ok(candidate_id: &str, source_lost: bool) -> Self {
            Self {
                result: Ok(HeldCapture {
                    candidate_id: candidate_id.to_string(),
                    execution_id: format!("blake3:{}", "a".repeat(64)),
                    snapshot_id: format!("blake3:{}", "d".repeat(64)),
                    artifact_location: "cas://held/candidate".to_string(),
                    source_lost,
                }),
                calls: 0,
            }
        }
    }

    impl CaptureAction for ScriptedCapture {
        fn capture(&mut self, _capture_epoch: u64) -> Result<HeldCapture, CaptureError> {
            self.calls += 1;
            self.result.clone()
        }
    }

    /// Eligibility fakes: `Ok` mints `for_test` (matches the manifest identity);
    /// `external_state` runs the #1090-shaped `analyze_for_test(true, …)` which
    /// fails closed.
    struct OkEligibility;
    impl EligibilitySource for OkEligibility {
        fn eligibility(&mut self) -> Result<VerifiedRunningSnapshotEligibility, AcceptanceFailure> {
            Ok(VerifiedRunningSnapshotEligibility::for_test(exec_id()))
        }
    }

    struct ExternalStateEligibility;
    impl EligibilitySource for ExternalStateEligibility {
        fn eligibility(&mut self) -> Result<VerifiedRunningSnapshotEligibility, AcceptanceFailure> {
            // #1090 fail-closed: a live workload that requires restore-time bindings
            // is ineligible for a running capture.
            VerifiedRunningSnapshotEligibility::analyze_for_test(true, exec_id())
        }
    }

    fn hold(pause_permitted: bool) -> ControlResponse {
        ControlResponse {
            directive: ControlDirective::Hold,
            server_capture_epoch: 0,
            candidate_id: None,
            hold_expires_at: Some("2026-01-01T00:00:00Z".to_string()),
            pause_permitted,
        }
    }

    fn capture(epoch: u64, candidate_id: &str, pause_permitted: bool) -> ControlResponse {
        ControlResponse {
            directive: ControlDirective::Capture,
            server_capture_epoch: epoch,
            candidate_id: Some(candidate_id.to_string()),
            hold_expires_at: None,
            pause_permitted,
        }
    }

    fn discard() -> ControlResponse {
        ControlResponse {
            directive: ControlDirective::Discard,
            server_capture_epoch: 0,
            candidate_id: None,
            hold_expires_at: None,
            pause_permitted: false,
        }
    }

    fn fencing() -> Fencing4 {
        Fencing4 {
            job_id: "job_hold".to_string(),
            submission_attempt_id: "subatt_hold".to_string(),
            worker_claim_id: "claim_hold".to_string(),
            lease_token: "tok".to_string(),
        }
    }

    fn config() -> AcceptanceConfig {
        AcceptanceConfig {
            seal_at_argv: vec!["true".to_string()],
            verification_timeout: Duration::from_secs(30),
            total_deadline: Duration::from_secs(300),
            maximum_attempts: 1,
        }
    }

    /// Assemble a hold phase over the given seams and run it. Returns the terminal
    /// plus the seams' call counts for assertions.
    #[allow(clippy::too_many_arguments)]
    fn run_hold(
        control: &mut dyn ControlSource,
        capture: &mut ScriptedCapture,
        eligibility: &mut dyn EligibilitySource,
        extend: &mut dyn ExtendPolicy,
        lifecycle: &mut FakeLifecycle,
        clock: &FakeClock,
        cancellation: &AcceptanceCancellation,
        hold_ttl: Duration,
    ) -> HoldTermination {
        let mut phase = HoldPhase::new(
            control,
            capture,
            eligibility,
            extend,
            lifecycle,
            clock,
            cancellation,
            fencing(),
            config(),
            hold_ttl,
        );
        phase.run().expect("no fatal internal error")
    }

    // ── (i) no capture before pause_permitted (ADR-007) ─────────────────────
    #[test]
    fn refuses_capture_until_pause_permitted() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // A capture directive WITHOUT permission, then WITH permission.
        let mut control = ScriptedControl::new(vec![
            capture(1, "cand_1", false),
            capture(1, "cand_1", true),
        ]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        // The first (unpermitted) poll did NOT capture; capture ran exactly once,
        // only after pause_permitted, and it was accepted.
        assert_eq!(cap.calls, 1, "capture must run only once pause_permitted");
        assert!(control.polls >= 2, "the unpermitted directive kept holding");
        assert!(matches!(outcome, HoldTermination::Accepted { .. }));
    }

    // ── (ii) capture → candidate → accept (exit 0) → terminal ack ───────────
    #[test]
    fn capture_then_accept_on_exit_zero() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::new(vec![capture(1, "cand_1", true)]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(cap.calls, 1);
        assert_eq!(
            lifecycle.executes, 1,
            "acceptance ran the seal_at argv once"
        );
        assert_eq!(
            outcome.terminal_ack_reason(),
            TerminalAckReason::AttemptEnded,
            "accepted ends the attempt (no job-terminal `accepted` reason)"
        );
        match outcome {
            HoldTermination::Accepted { report } => {
                assert_eq!(report.candidate_id, "cand_1");
                assert_eq!(report.capture_epoch, 1);
                assert_eq!(report.submission_attempt_id, "subatt_hold");
                assert_eq!(report.worker_claim_id, "claim_hold");
                assert!(!report.source_lost);
                report.validate().expect("candidate report is wire-valid");
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    // ── (iii) TTL/deadline reached → attempt_ended, no capture ──────────────
    #[test]
    fn deadline_ends_attempt_without_capture() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Hold forever; each poll advances the clock by 10 min. TTL = 30 min, so
        // the deadline gate ends the attempt after a few holds — never capturing.
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(10 * 60),
            vec![hold(true)],
        );
        let mut cap = ScriptedCapture::ok("cand_never", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(cap.calls, 0, "no capture past the deadline (fail-closed)");
        assert_eq!(lifecycle.executes, 0);
        assert!(matches!(outcome, HoldTermination::AttemptEnded));
        assert_eq!(
            outcome.terminal_ack_reason(),
            TerminalAckReason::AttemptEnded
        );
    }

    // ── discard directive → terminal ack reason discarded ───────────────────
    #[test]
    fn discard_directive_ends_attempt() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::new(vec![hold(true), discard()]);
        let mut cap = ScriptedCapture::ok("cand_never", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(cap.calls, 0);
        assert!(matches!(outcome, HoldTermination::Discarded));
        assert_eq!(outcome.terminal_ack_reason(), TerminalAckReason::Discarded);
    }

    // ── (iv) acceptance failure + source available → back to holding ────────
    #[test]
    fn acceptance_failure_with_source_available_returns_to_holding() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Capture (permitted, source alive), acceptance rejects (exit 1); the phase
        // must return to holding — the NEXT directive (discard) then ends it.
        let mut control = ScriptedControl::new(vec![capture(1, "cand_1", true), discard()]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(1)]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(cap.calls, 1, "captured once");
        assert_eq!(lifecycle.executes, 1, "acceptance ran once, then rejected");
        assert!(
            matches!(outcome, HoldTermination::Discarded),
            "returned to holding and ended on the next discard, got {outcome:?}"
        );
    }

    // ── (v) source_lost → terminal accepting_source_lost, no re-capture ─────
    #[test]
    fn source_lost_after_failed_acceptance_is_terminal() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Even though a second capture directive follows, a source-lost + rejected
        // capture is terminal: no re-capture.
        let mut control =
            ScriptedControl::new(vec![capture(1, "cand_1", true), capture(2, "cand_2", true)]);
        let mut cap = ScriptedCapture::ok("cand_1", true); // source_lost = true
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(1)]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(cap.calls, 1, "no re-capture after source lost");
        assert!(matches!(
            outcome,
            HoldTermination::AcceptanceFailedSourceLost { .. }
        ));
        assert_eq!(
            outcome.terminal_ack_reason(),
            TerminalAckReason::AcceptanceFailedSourceLost
        );
        assert_eq!(
            outcome.failure_stage(),
            Some(WizardFailureStage::Acceptance)
        );
    }

    // ── (v-b) capture-epoch monotonicity (ADR-008): a stale/duplicate Capture ──
    //    after a source-available acceptance failure is ignored; a strictly
    //    newer epoch DOES drive a fresh capture.
    #[test]
    fn stale_capture_epoch_after_source_available_failure_is_ignored() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Epoch 1 captures (permitted, source alive) → acceptance rejects
        // (exit 1) → return to holding. A DUPLICATE Capture with the SAME epoch 1
        // must be IGNORED (no second capture). A strictly-greater epoch 2 then
        // DOES drive a second capture, which is accepted (exit 0).
        let mut control = ScriptedControl::new(vec![
            capture(1, "cand_1", true),
            capture(1, "cand_1", true), // stale/duplicate epoch → ignored
            capture(2, "cand_2", true), // newer epoch → captures
        ]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(0),
        ]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        // Exactly two captures — epoch 1 and epoch 2. The duplicate epoch-1
        // directive between them drove NO capture.
        assert_eq!(
            cap.calls, 2,
            "duplicate/stale epoch ignored; only a strictly-newer epoch captures"
        );
        assert_eq!(
            lifecycle.executes, 2,
            "acceptance ran once per distinct capture"
        );
        assert!(matches!(outcome, HoldTermination::Accepted { .. }));
    }

    // ── (vi) external-state capsule → eligibility fails closed, never captures ─
    #[test]
    fn external_state_capsule_fails_closed_before_capture() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Even with a capture directive queued, eligibility fails closed at entry.
        let mut control = ScriptedControl::new(vec![capture(1, "cand_1", true)]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = ExternalStateEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(cap.calls, 0, "ineligible capsule never enters capture");
        assert_eq!(lifecycle.captures, 0);
        assert!(matches!(outcome, HoldTermination::FailedClosed { .. }));
        assert_eq!(
            outcome.terminal_ack_reason(),
            TerminalAckReason::BuildFailed
        );
    }

    // ── explicit extend pushes the deadline (USER DECISION: 30-min TTL + extend) ─
    #[test]
    fn explicit_extend_pushes_the_deadline_then_ends() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Hold forever, advancing 20 min per poll; TTL = 30 min. The extend seam
        // grants ONE 30-min extension, then declines — so the attempt ends on the
        // second deadline hit, still without capturing.
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(20 * 60),
            vec![hold(true)],
        );
        let mut cap = ScriptedCapture::ok("cand_never", false);
        let mut elig = OkEligibility;

        struct ExtendOnce {
            remaining: u32,
        }
        impl ExtendPolicy for ExtendOnce {
            fn on_deadline(&mut self) -> Option<Duration> {
                if self.remaining > 0 {
                    self.remaining -= 1;
                    Some(Duration::from_secs(30 * 60))
                } else {
                    None
                }
            }
        }
        let mut extend = ExtendOnce { remaining: 1 };
        let mut lifecycle = FakeLifecycle::new(vec![]);

        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(extend.remaining, 0, "the one extension was consumed");
        assert_eq!(cap.calls, 0);
        assert!(matches!(outcome, HoldTermination::AttemptEnded));
    }
}
