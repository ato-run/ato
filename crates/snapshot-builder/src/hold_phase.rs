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
//! **Eligibility** enters through [`EligibilitySource`], which must fail closed
//! for any capsule that requires External State or restore-time secret bindings
//! (#1090). In prod that is [`crate::claim_eligibility::ClaimContractEligibility`],
//! which mints the proof from the Execution Contract the control plane pinned on
//! the claim — see its module doc for exactly which guarantee that is, and which
//! it deliberately is not.
//!
//! Every seam now has a production implementation, and
//! `process_interactive_capture_job` assembles them:
//! [`ControlSource`] → [`crate::wizard_api::ApiControlSource`] (control poll,
//! candidate/acceptance reports, and the lease renew that rides all three),
//! [`CaptureAction`] → [`crate::guest_capture::GuestCaptureAction`] (pause →
//! snapshot → resume → seal against a live held guest), and the acceptance
//! lifecycle → `snapshot::disposable_lifecycle::BackendDisposableLifecycle`. The
//! orchestration below stays pure and is still exercised in full by this
//! module's own KVM-free unit tests.
//!
//! **Dead-code allow (scoped to this module):** `snapshot-builder` is a *binary*
//! crate, so `pub` items count as dead unless reached from `fn main`. Several
//! items here exist for the seams' contracts (and are exercised by the tests
//! below) without being named from the wiring; the allow is module-scoped
//! rather than crate-wide.
#![allow(dead_code)]

use std::time::Duration;

use snapshot::acceptance::{
    AcceptanceBudget, AcceptanceConfig, AcceptanceFailure, CandidateSnapshot,
    DisposableAcceptanceLifecycle, DisposableSessionHandle, FatalInternalError, MonotonicClock,
    RunningSnapshotAcceptance, VerificationOutcome, VerifiedRunningSnapshotEligibility,
};

use crate::wizard_wire::{
    AcceptanceReceipt, AcceptanceReceiptSchema, AcceptanceStatus, CandidateAcceptanceRequest,
    CandidateReportRequest, ControlDirective, ControlResponse, Fencing4, TerminalAckReason,
    WizardFailureStage,
};

/// Default hold TTL (USER DECISION): 30 minutes, with explicit extend via the
/// [`ExtendPolicy`] seam.
pub const DEFAULT_HOLD_TTL: Duration = Duration::from_secs(30 * 60);

/// A control-poll fault that ENDS the hold locally, WITHOUT a terminal ack.
///
/// Every fault on this channel resolves the same way (SSOT §3.8): a `409 fenced`
/// means the claim/lease is already dead server-side, and lease expiry is
/// SERVER-OWNED — the sweep moves the attempt to `expired` and an
/// expired-lease ack is unsendable (FENCING-4 would `409` it). A malformed
/// control response, or a transport failure the client could not recover from
/// inside the lease window, leaves the lease in doubt, and a builder that
/// cannot prove its lease is alive must not assert a job-terminal state either.
/// So the rule is uniform and fail-closed: tear down locally, ack nothing, let
/// the server sweep own the terminal state — see
/// [`HoldTermination::TornDownWithoutAck`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFault {
    /// Diagnostic detail. Never carries the lease token (the api client's error
    /// types cannot hold it — see `crate::wizard_api`).
    pub message: String,
}

/// The control-poll source (SSOT §3.3). Yields a [`ControlResponse`] carrying the
/// directive (`hold | capture | discard`), the authoritative `server_capture_epoch`
/// (adopted as the observed command cursor), and — critically — `pause_permitted`
/// (ADR-007 causality). In prod this is
/// [`crate::wizard_api::ApiControlSource`]; tests script a fixed sequence.
pub trait ControlSource: LeaseKeepalive {
    /// Poll the control channel, reporting the highest epoch observed so far.
    ///
    /// Fallible on purpose: the production implementation is a network call
    /// against a leased claim, and there is no honest `ControlResponse` to
    /// invent for a fenced or malformed answer — a synthesized `discard` would
    /// send a `discarded` ack on a dead lease, and a synthesized `hold` would
    /// spin until the TTL and then ack. Both are lies; [`ControlFault`] is the
    /// truth.
    fn poll(&mut self, observed_capture_epoch: u64) -> Result<ControlResponse, ControlFault>;

    /// §3.6 — report the candidate this hold just sealed.
    ///
    /// On the SAME seam as the poll, and for the same reason the lease is
    /// ([`LeaseKeepalive`]'s doc): report and poll are two calls on ONE claim,
    /// fenced by one tuple and paced by one lease driver. Splitting them would
    /// give the hold two independent notions of whether that claim is still
    /// alive.
    ///
    /// The implementation supplies the epoch↔candidate pairing the server
    /// cross-checks (§1.2) from what the control channel actually delivered —
    /// [`HoldPhase`] never restates it, because a pairing restated by the party
    /// being checked proves nothing.
    fn report_candidate(&mut self, report: &CandidateReportRequest) -> Result<(), ControlFault>;

    /// §3.7 — report the acceptance outcome of the candidate last reported.
    ///
    /// NOT job-terminal: acceptance is a per-candidate endpoint, and with the
    /// source alive the attempt returns to holding either way.
    fn report_acceptance(
        &mut self,
        request: &CandidateAcceptanceRequest,
    ) -> Result<(), ControlFault>;
}

/// Keeping the claim's lease alive WITHOUT polling for a directive.
///
/// The hold loop renews its lease on the control poll, which is correct while
/// the loop is polling — and it is not polling for the two stretches that
/// dominate a real Step 4: the capture (pause + vmstate/memory/disk seal +
/// upload, tens of seconds to minutes) and the acceptance run (a disposable
/// restore plus the `seal_at` command, bounded by
/// `AcceptanceConfig::total_deadline`, which is minutes). Both routinely outlast
/// what a lease has left when the capture directive arrives — the renew cadence
/// is a third of the TTL, so the lease can be two thirds spent already. Without a
/// renew inside them, the loop comes back to a lease that expired while a
/// candidate was being captured AND accepted, and the fail-closed rule then
/// throws away both the candidate and the author's session.
///
/// So the long steps drive this seam. It is a SUPERTRAIT of [`ControlSource`]
/// rather than a separate seam because the lease and the control channel are the
/// same claim: the production [`crate::wizard_api::ApiControlSource`] owns the
/// one renew driver, and two seams over one driver would be two chances to renew
/// a lease the server already revoked.
///
/// `Err` means the same thing it means on a poll: STOP — fenced, expired, or
/// unprovable. It is never a hint to be retried by the capture backend.
pub trait LeaseKeepalive {
    /// Renew the claim's lease if the cadence says to, and fail closed if the
    /// lease is gone.
    fn keepalive(&mut self) -> Result<(), ControlFault>;
}

/// The hold's lease as handed to a long-running step.
///
/// Two jobs beyond forwarding to the control channel, both fail-closed:
///
/// - it REMEMBERS the first fault, so [`HoldPhase`] can end the hold on the
///   lease's terms no matter what the capture backend or the acceptance loop
///   decided to do with the `Err` it was handed (a backend that swallowed it and
///   returned a candidate anyway must not get that candidate reported); and
/// - once lost it stays lost — a later keepalive never "recovers", because the
///   claim it would be recovering into is already someone else's.
pub struct HoldLease<'c> {
    control: &'c mut dyn ControlSource,
    fault: Option<ControlFault>,
}

impl<'c> HoldLease<'c> {
    fn new(control: &'c mut dyn ControlSource) -> Self {
        HoldLease {
            control,
            fault: None,
        }
    }

    /// The first lease fault observed, if any.
    fn fault(&self) -> Option<&ControlFault> {
        self.fault.as_ref()
    }
}

impl LeaseKeepalive for HoldLease<'_> {
    fn keepalive(&mut self) -> Result<(), ControlFault> {
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        match self.control.keepalive() {
            Ok(()) => Ok(()),
            Err(fault) => {
                self.fault = Some(fault.clone());
                Err(fault)
            }
        }
    }
}

/// The Firecracker-concrete capture seam. In prod this pauses the live held guest,
/// snapshots it, and resumes the source (keeping the guest alive), producing the
/// candidate that becomes the published Snapshot. A capture that fails before seal
/// yields a [`CaptureError`]; a capture whose source could not be resumed yields a
/// [`HeldCapture`] with `source_lost = true` (ADR-012).
pub trait CaptureAction {
    /// Capture an immutable candidate for `capture_epoch` from the live held guest.
    ///
    /// `candidate_id` is the id the control channel pre-minted for THIS epoch. It
    /// is passed in rather than looked up because the capture seam has no view of
    /// the control channel, and §3.6 requires the reported candidate id to equal
    /// the delivered one (the server cross-checks epoch↔candidate 1:1). An
    /// implementation must echo it back in [`HeldCapture::candidate_id`].
    ///
    /// `lease` is the claim's lease, live for the duration of the capture. A
    /// capture is the longest single step of the hold and runs with no control
    /// poll in it, so the implementation MUST drive `lease` between its own
    /// phases (pause, seal, upload) or the lease dies under a candidate that is
    /// then unreportable. An `Err` from it is terminal for the hold — do not
    /// retry it, and do not treat a captured candidate as salvageable after one:
    /// [`HoldPhase`] ends the hold on that fault regardless of what is returned
    /// here.
    fn capture(
        &mut self,
        capture_epoch: u64,
        candidate_id: &str,
        lease: &mut dyn LeaseKeepalive,
    ) -> Result<HeldCapture, CaptureError>;
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
    /// The control channel faulted ([`ControlFault`]): the hold is over and the
    /// builder sends **NO** terminal ack. See the fault's doc for why every
    /// fault on that channel resolves this way.
    TornDownWithoutAck {
        /// Diagnostic detail — logged locally, never sent anywhere.
        failure_reason: String,
    },
}

impl HoldTermination {
    /// Project to the ONLY legal wizard job-terminal reasons (SSOT §3.8), or
    /// `None` when this outcome must send **no ack at all**.
    ///
    /// `accepted` ends this slice's attempt as an orderly end (there is no
    /// job-terminal "accepted" reason: acceptance is a per-candidate endpoint in
    /// the full flow, §3.7). The `None` arm is not an omission: §3.8 has no
    /// `lease_expired` reason because expiry is server-owned, so "torn down with
    /// a dead/doubtful lease" has no legal ack — the type says so rather than
    /// leaving a caller to remember it.
    pub fn terminal_ack_reason(&self) -> Option<TerminalAckReason> {
        match self {
            HoldTermination::Accepted { .. } | HoldTermination::AttemptEnded => {
                Some(TerminalAckReason::AttemptEnded)
            }
            HoldTermination::Discarded => Some(TerminalAckReason::Discarded),
            HoldTermination::AcceptanceFailedSourceLost { .. } => {
                Some(TerminalAckReason::AcceptanceFailedSourceLost)
            }
            HoldTermination::FailedClosed { .. } => Some(TerminalAckReason::BuildFailed),
            HoldTermination::TornDownWithoutAck { .. } => None,
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

/// The #1088 acceptance lifecycle with the hold's lease kept alive across it.
///
/// The acceptance run is the second stretch that outlives a lease window: it is
/// bounded by `AcceptanceConfig::total_deadline` (minutes) and
/// [`RunningSnapshotAcceptance::accept`] is one blocking call, so the hold loop
/// cannot renew around it. Wrapping the lifecycle puts the renew exactly where
/// the run already pauses between phases, without a background thread and
/// without touching `snapshot::acceptance`.
///
/// Fail-closed shape, and the split matters:
///
/// - the PRODUCTIVE phases (capture / create / restore / execute) keepalive
///   first and REFUSE once the lease is lost — a verification that runs on a
///   claim this builder no longer holds is work nobody can act on, and its
///   verdict must never become an ack; while
/// - the TEARDOWN phases (terminate / destroy) always delegate, untouched. They
///   release a real microVM and its overlay, they are the one thing that must
///   still happen when the lease is gone, and a keepalive there could only turn
///   a clean teardown into a leak.
struct LeaseKeptLifecycle<'l, 'c, L: DisposableAcceptanceLifecycle + ?Sized> {
    inner: &'l mut L,
    lease: &'l mut HoldLease<'c>,
}

impl<L: DisposableAcceptanceLifecycle + ?Sized> LeaseKeptLifecycle<'_, '_, L> {
    /// The gate every productive phase runs first. The seam's error type is a
    /// `String`, so the lease diagnostic travels as the phase's failure — the
    /// authoritative copy is the one [`HoldLease`] remembered.
    fn lease_alive(&mut self) -> Result<(), String> {
        self.lease
            .keepalive()
            .map_err(|fault| format!("hold lease lost during acceptance: {}", fault.message))
    }
}

impl<L: DisposableAcceptanceLifecycle + ?Sized> DisposableAcceptanceLifecycle
    for LeaseKeptLifecycle<'_, '_, L>
{
    fn capture_candidate(
        &mut self,
        attempt: u32,
        budget: &AcceptanceBudget,
    ) -> Result<CandidateSnapshot, String> {
        self.lease_alive()?;
        self.inner.capture_candidate(attempt, budget)
    }

    fn create_disposable_session(
        &mut self,
        candidate: &CandidateSnapshot,
        budget: &AcceptanceBudget,
    ) -> Result<DisposableSessionHandle, String> {
        self.lease_alive()?;
        self.inner.create_disposable_session(candidate, budget)
    }

    fn restore_candidate(
        &mut self,
        session: &DisposableSessionHandle,
        candidate: &CandidateSnapshot,
        budget: &AcceptanceBudget,
    ) -> Result<(), String> {
        self.lease_alive()?;
        self.inner.restore_candidate(session, candidate, budget)
    }

    fn execute_exact_argv(
        &mut self,
        session: &DisposableSessionHandle,
        argv: &[String],
        timeout: Duration,
        budget: &AcceptanceBudget,
    ) -> Result<VerificationOutcome, String> {
        self.lease_alive()?;
        self.inner
            .execute_exact_argv(session, argv, timeout, budget)
    }

    fn terminate_process_tree(&mut self, session: &DisposableSessionHandle) -> Result<(), String> {
        self.inner.terminate_process_tree(session)
    }

    fn destroy_disposable_session(
        &mut self,
        session: DisposableSessionHandle,
    ) -> Result<(), String> {
        self.inner.destroy_disposable_session(session)
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
    /// 4. On capture, run the capture-action, then #1088 acceptance — both with
    ///    the claim's lease kept alive across them ([`LeaseKeepalive`]), and both
    ///    ending the hold as [`HoldTermination::TornDownWithoutAck`] the moment
    ///    the lease is lost, ahead of whatever they themselves returned:
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

            // A control fault ends the hold LOCALLY with no ack (see
            // `ControlFault`): the lease is dead or in doubt, so there is no
            // job-terminal claim this builder is entitled to make.
            let response = match self.control.poll(observed_epoch) {
                Ok(response) => response,
                Err(fault) => {
                    return Ok(HoldTermination::TornDownWithoutAck {
                        failure_reason: fault.message,
                    });
                }
            };
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

                    // The lease, live across BOTH long steps below. Neither polls,
                    // and together they run for minutes — long enough to outlive
                    // the lease window the capture directive arrived in — so this
                    // is what keeps the claim (and therefore the candidate report
                    // and the terminal ack) alive through them.
                    // (3b) §3.6 needs the candidate id the control channel minted
                    // for this epoch, and the capture seam cannot see the control
                    // channel. A `Capture` without one is a contract violation:
                    // capturing anyway would produce a candidate that can never be
                    // reported (the server cross-checks epoch↔candidate 1:1), so
                    // this fails closed instead of burning a capture.
                    // `ApiControlSource` already rejects it at the wire via
                    // `ControlResponse::validate`; this is the same refusal for
                    // any other `ControlSource`.
                    let Some(candidate_id) = response.candidate_id.clone() else {
                        return Ok(HoldTermination::FailedClosed {
                            failure_reason: format!(
                                "control delivered `capture` for epoch {epoch} with no \
                                 candidate_id: the candidate could never be reported"
                            ),
                        });
                    };

                    // (4) Firecracker-concrete capture for this epoch. The lease
                    // is scoped to the step so the control seam is free again
                    // afterwards — the §3.6 report rides the same seam.
                    let captured = {
                        let mut lease = HoldLease::new(&mut *self.control);
                        let captured = self.capture.capture(epoch, &candidate_id, &mut lease);
                        // The lease's verdict outranks the capture's. A backend
                        // that was told the lease is gone and produced a
                        // candidate anyway has produced one nobody can report:
                        // every call that would carry it is a 409, and §3.8 has
                        // no ack for a dead lease.
                        if let Some(fault) = lease.fault() {
                            return Ok(HoldTermination::TornDownWithoutAck {
                                failure_reason: fault.message.clone(),
                            });
                        }
                        captured
                    };
                    let held = match captured {
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

                    // (5) §3.6 BEFORE the acceptance run, not after it. Three
                    // reasons, and the first two are correctness:
                    //
                    // - §3.7 refuses an outcome for a candidate that was never
                    //   reported ("only a reported (captured) candidate can
                    //   carry an acceptance outcome"), so a report deferred
                    //   until acceptance finished would make the REJECTED branch
                    //   unreportable — the one branch the author most needs to
                    //   see;
                    // - the report is what un-quiesces the attempt server-side,
                    //   and until it lands the author's preview stays drained
                    //   for the whole (minutes-long) acceptance run;
                    // - the api models that window as its own state
                    //   (`validating`), which only exists if this call is what
                    //   opens it.
                    let report = self.candidate_report(epoch, &held);
                    if let Err(fault) = self.control.report_candidate(&report) {
                        // No ack, on the same rule as any other fault on this
                        // channel: a report that could not land means the server
                        // does not know this candidate exists, and the reasons a
                        // report fails (fenced, superseded epoch, an exchange
                        // that broke the contract) are the reasons a builder may
                        // not assert a job-terminal state either.
                        return Ok(HoldTermination::TornDownWithoutAck {
                            failure_reason: format!("candidate report: {}", fault.message),
                        });
                    }

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

                    // #1088 acceptance via the EXISTING disposable-restore
                    // lifecycle, wrapped so the same lease is renewed between its
                    // phases (see `LeaseKeptLifecycle`) — the run's own
                    // `total_deadline` is minutes, which is longer than a lease
                    // window, so an unwrapped run routinely finishes on a lease
                    // that died under it.
                    let run = {
                        let mut lease = HoldLease::new(&mut *self.control);
                        let run = RunningSnapshotAcceptance::accept(
                            &mut LeaseKeptLifecycle {
                                inner: &mut *self.lifecycle,
                                lease: &mut lease,
                            },
                            eligibility,
                            &self.acceptance_config,
                            self.cancellation,
                            self.clock,
                        )?;
                        // Again the lease's verdict first: a run that was refused
                        // phase-by-phase because the lease died reports as a
                        // rejection, and a rejection is an ACK — exactly the claim
                        // a builder with no lease may not make.
                        if let Some(fault) = lease.fault() {
                            return Ok(HoldTermination::TornDownWithoutAck {
                                failure_reason: fault.message.clone(),
                            });
                        }
                        run
                    };

                    // (6) §3.7 — the verdict for THIS candidate, accepted or
                    // rejected. Sent on both branches: a rejected candidate that
                    // is never told to the server leaves the author's wizard
                    // showing a validation that is still running, when in fact it
                    // finished and failed.
                    let acceptance = match acceptance_request(&self.fencing, epoch, &run) {
                        Ok(request) => request,
                        Err(reason) => {
                            // The run happened; its receipt could not be put on
                            // the wire. Ending the attempt fail-closed is the
                            // honest outcome — the alternative is holding on with
                            // a candidate whose verdict nobody will ever hear.
                            return Ok(HoldTermination::FailedClosed {
                                failure_reason: reason,
                            });
                        }
                    };
                    if let Err(fault) = self.control.report_acceptance(&acceptance) {
                        return Ok(HoldTermination::TornDownWithoutAck {
                            failure_reason: format!("candidate acceptance: {}", fault.message),
                        });
                    }

                    if run.is_accepted() {
                        return Ok(HoldTermination::Accepted { report });
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

    /// Build the §3.6 candidate report for a sealed capture, echoing the
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

/// Build the §3.7 acceptance body for a finished acceptance run.
///
/// The receipt travels VERBATIM: §3.7's envelope is deliberately opaque
/// (`receipt_schema` + an object whose payload the wire does not pin), so the
/// run's own `AcceptanceReceiptV1` is serialized as-is rather than projected
/// onto a hand-picked subset. A projection is a second definition of what was
/// verified, and the one thing the receipt exists to be is the record of what
/// actually ran.
///
/// `Err` when the receipt cannot be represented on the wire — which the §3.7
/// envelope's "is an object" rule makes a real (if remote) possibility, and a
/// silently-empty receipt would be an accepted candidate with no evidence.
fn acceptance_request(
    fencing: &Fencing4,
    capture_epoch: u64,
    run: &snapshot::acceptance::AcceptanceRun,
) -> Result<CandidateAcceptanceRequest, String> {
    let (status, acceptance_receipt, failure_reason) = if run.is_accepted() {
        let value = serde_json::to_value(&run.receipt)
            .map_err(|e| format!("serialize the acceptance receipt: {e}"))?;
        let serde_json::Value::Object(receipt) = value else {
            return Err("acceptance receipt is not a JSON object".to_string());
        };
        (
            AcceptanceStatus::Accepted,
            Some(AcceptanceReceipt {
                receipt_schema: AcceptanceReceiptSchema,
                receipt,
            }),
            None,
        )
    } else {
        (
            AcceptanceStatus::Rejected,
            None,
            Some(
                run.failure()
                    .map(|failure| failure.to_string())
                    .unwrap_or_else(|| "acceptance rejected".to_string()),
            ),
        )
    };
    let request = CandidateAcceptanceRequest {
        submission_attempt_id: fencing.submission_attempt_id.clone(),
        worker_claim_id: fencing.worker_claim_id.clone(),
        capture_epoch,
        status,
        acceptance_receipt,
        failure_reason,
    };
    // The §3.7 required-by-refinement rules are the api's too, so a body that
    // cannot pass them is caught here rather than spent as a round trip that
    // comes back 400 with the candidate's verdict still untold.
    request.validate()?;
    Ok(request)
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
    ///
    /// It also counts keepalives and can start failing them after `n` — standing
    /// in for the real thing a keepalive reports: a lease that was fenced or
    /// expired while the hold was busy in a step that does not poll.
    struct ScriptedControl {
        responses: Vec<ControlResponse>,
        idx: usize,
        polls: usize,
        clock: Option<FakeClock>,
        advance_per_poll: Duration,
        keepalives: usize,
        lose_lease_after: Option<usize>,
        /// Every §3.6/§3.7 call, in the order it was made — the ordering
        /// between them is a wire rule, so it is recorded rather than counted.
        wire: Vec<WireCall>,
        refuse_report: bool,
        refuse_acceptance: bool,
    }

    /// A recorded builder→api call on the reporting half of the seam.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum WireCall {
        Candidate(CandidateReportRequest),
        Acceptance(CandidateAcceptanceRequest),
    }

    impl ScriptedControl {
        fn new(responses: Vec<ControlResponse>) -> Self {
            Self {
                responses,
                idx: 0,
                polls: 0,
                clock: None,
                advance_per_poll: Duration::ZERO,
                keepalives: 0,
                lose_lease_after: None,
                wire: Vec::new(),
                refuse_report: false,
                refuse_acceptance: false,
            }
        }

        fn advancing(
            clock: FakeClock,
            per_poll: Duration,
            responses: Vec<ControlResponse>,
        ) -> Self {
            Self {
                clock: Some(clock),
                advance_per_poll: per_poll,
                ..Self::new(responses)
            }
        }

        /// The lease survives `n` keepalives and is gone from the `n+1`th on.
        fn losing_the_lease_after(mut self, n: usize) -> Self {
            self.lose_lease_after = Some(n);
            self
        }

        /// The §3.6 report is refused — a superseded epoch, or a claim that died
        /// while the capture was sealing.
        fn refusing_the_candidate_report(mut self) -> Self {
            self.refuse_report = true;
            self
        }

        /// The §3.7 acceptance is refused.
        fn refusing_the_acceptance_report(mut self) -> Self {
            self.refuse_acceptance = true;
            self
        }

        fn candidate_reports(&self) -> Vec<&CandidateReportRequest> {
            self.wire
                .iter()
                .filter_map(|call| match call {
                    WireCall::Candidate(report) => Some(report),
                    WireCall::Acceptance(_) => None,
                })
                .collect()
        }

        fn acceptance_reports(&self) -> Vec<&CandidateAcceptanceRequest> {
            self.wire
                .iter()
                .filter_map(|call| match call {
                    WireCall::Acceptance(request) => Some(request),
                    WireCall::Candidate(_) => None,
                })
                .collect()
        }
    }

    impl LeaseKeepalive for ScriptedControl {
        fn keepalive(&mut self) -> Result<(), ControlFault> {
            self.keepalives += 1;
            match self.lose_lease_after {
                Some(n) if self.keepalives > n => Err(ControlFault {
                    message: "lease expired: the observed lease deadline passed".to_string(),
                }),
                _ => Ok(()),
            }
        }
    }

    impl ControlSource for ScriptedControl {
        fn poll(&mut self, _observed_capture_epoch: u64) -> Result<ControlResponse, ControlFault> {
            self.polls += 1;
            if let Some(clock) = &self.clock {
                clock.advance(self.advance_per_poll);
            }
            let i = self.idx.min(self.responses.len() - 1);
            self.idx += 1;
            Ok(self.responses[i].clone())
        }

        fn report_candidate(
            &mut self,
            report: &CandidateReportRequest,
        ) -> Result<(), ControlFault> {
            self.wire.push(WireCall::Candidate(report.clone()));
            if self.refuse_report {
                return Err(ControlFault {
                    message: "fenced: superseded capture epoch".to_string(),
                });
            }
            Ok(())
        }

        fn report_acceptance(
            &mut self,
            request: &CandidateAcceptanceRequest,
        ) -> Result<(), ControlFault> {
            self.wire.push(WireCall::Acceptance(request.clone()));
            if self.refuse_acceptance {
                return Err(ControlFault {
                    message: "fenced: claim is no longer active".to_string(),
                });
            }
            Ok(())
        }
    }

    /// A control source that serves `head` and then faults — the production
    /// shape of a lease that died mid-hold (`409 fenced`) or a control response
    /// that failed its own contract.
    struct FaultingControl {
        head: Vec<ControlResponse>,
        idx: usize,
    }

    impl LeaseKeepalive for FaultingControl {
        fn keepalive(&mut self) -> Result<(), ControlFault> {
            Ok(())
        }
    }

    impl ControlSource for FaultingControl {
        fn poll(&mut self, _observed_capture_epoch: u64) -> Result<ControlResponse, ControlFault> {
            let next = self.head.get(self.idx).cloned();
            self.idx += 1;
            match next {
                Some(response) => Ok(response),
                None => Err(ControlFault {
                    message: "fenced: claim is no longer active".to_string(),
                }),
            }
        }

        fn report_candidate(
            &mut self,
            _report: &CandidateReportRequest,
        ) -> Result<(), ControlFault> {
            Ok(())
        }

        fn report_acceptance(
            &mut self,
            _request: &CandidateAcceptanceRequest,
        ) -> Result<(), ControlFault> {
            Ok(())
        }
    }

    /// A scripted capture-action: records call count and yields a configured
    /// result. `source_lost` rides the produced [`HeldCapture`].
    ///
    /// `lease_drives` stands in for the real backend's pause/seal/upload phases:
    /// a production capture runs for minutes with no control poll in it, so it
    /// drives the lease between its own steps. The fake drives it that many times
    /// and — like a backend that is mid-seal when the answer comes back — keeps
    /// going after an `Err`, which is precisely why `HoldPhase` may not rely on
    /// the capture's own return value to notice a dead lease.
    struct ScriptedCapture {
        result: Result<HeldCapture, CaptureError>,
        calls: u32,
        lease_drives: u32,
        seen_candidate_ids: Vec<String>,
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
                lease_drives: 0,
                seen_candidate_ids: Vec::new(),
            }
        }

        /// A capture long enough to need `n` keepalives — a real one.
        fn driving_the_lease(mut self, n: u32) -> Self {
            self.lease_drives = n;
            self
        }
    }

    impl CaptureAction for ScriptedCapture {
        fn capture(
            &mut self,
            _capture_epoch: u64,
            candidate_id: &str,
            lease: &mut dyn LeaseKeepalive,
        ) -> Result<HeldCapture, CaptureError> {
            self.calls += 1;
            // Record what the phase actually handed down, so a test can prove the
            // control channel's candidate id reaches the seam (§3.6 requires the
            // reported id to equal the delivered one).
            self.seen_candidate_ids.push(candidate_id.to_string());
            for _ in 0..self.lease_drives {
                let _ = lease.keepalive();
            }
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

    /// A `capture` directive that names no candidate — the contract violation
    /// §3.6 makes unreportable.
    fn capture_without_candidate(epoch: u64) -> ControlResponse {
        ControlResponse {
            directive: ControlDirective::Capture,
            server_capture_epoch: epoch,
            candidate_id: None,
            hold_expires_at: None,
            pause_permitted: true,
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
            lease_token: crate::wizard_wire::LeaseToken::new("tok".to_string()),
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
    fn a_capture_with_no_candidate_id_fails_closed_without_capturing() {
        // §3.6: the reported candidate id must equal the one the control channel
        // delivered for the epoch, and the server cross-checks epoch↔candidate
        // 1:1. A `capture` naming none can therefore never be reported — pausing
        // the guest and sealing bytes for it would burn a capture (and an epoch)
        // to produce an artifact nobody can accept.
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::new(vec![capture_without_candidate(1)]);
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

        assert!(
            matches!(outcome, HoldTermination::FailedClosed { .. }),
            "a candidate-less capture must end the hold fail-closed"
        );
        assert_eq!(cap.calls, 0, "the guest must never be paused for it");
    }

    #[test]
    fn the_control_channels_candidate_id_reaches_the_capture_seam() {
        // The seam has no view of the control channel, so if the phase did not
        // hand this down, an implementation could only invent an id — and every
        // report would be rejected by the server's epoch↔candidate cross-check.
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::new(vec![capture(1, "cand_from_server", true)]);
        let mut cap = ScriptedCapture::ok("cand_from_server", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);

        let _ = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(
            cap.seen_candidate_ids,
            vec!["cand_from_server".to_string()],
            "the seam must receive the id the control channel delivered"
        );
    }

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
            Some(TerminalAckReason::AttemptEnded),
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

    // ── §3.6 / §3.7 reporting ───────────────────────────────────────────────

    /// The candidate is reported BEFORE the acceptance run, and its verdict
    /// after — in that order, on the wire.
    ///
    /// The order is not cosmetic. §3.7 refuses an outcome for a candidate that
    /// was never reported, and until §3.6 lands the attempt is still quiesced
    /// server-side — so a report deferred until after acceptance would leave the
    /// author's preview drained for the whole (minutes-long) run and make the
    /// rejected branch unreportable entirely.
    #[test]
    fn the_candidate_is_reported_before_its_acceptance_verdict() {
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

        assert!(matches!(outcome, HoldTermination::Accepted { .. }));
        assert!(
            matches!(
                control.wire.as_slice(),
                [WireCall::Candidate(_), WireCall::Acceptance(_)]
            ),
            "expected exactly one §3.6 then one §3.7, got {:?}",
            control.wire
        );
        let report = control.candidate_reports()[0];
        assert_eq!(report.candidate_id, "cand_1");
        assert_eq!(report.capture_epoch, 1);
        let acceptance = control.acceptance_reports()[0];
        assert_eq!(acceptance.status, AcceptanceStatus::Accepted);
        assert_eq!(acceptance.capture_epoch, 1);
        let receipt = acceptance
            .acceptance_receipt
            .as_ref()
            .expect("an accepted candidate carries its receipt");
        assert!(
            !receipt.receipt.is_empty(),
            "the receipt travels verbatim, not as an empty envelope"
        );
        acceptance
            .validate()
            .expect("acceptance body is wire-valid");
    }

    /// A candidate the verifier REJECTED is still reported, and its rejection is
    /// told to the server before the hold goes back to holding.
    ///
    /// The author is watching a wizard that shows `validating`. Returning to the
    /// hold without saying anything leaves that spinner running against a
    /// verification that already finished and failed.
    #[test]
    fn a_rejected_candidate_reports_its_rejection_and_keeps_holding() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Capture once, then hold; the TTL ends the attempt after the rejection.
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(11 * 60),
            vec![capture(1, "cand_1", true), hold(false)],
        );
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(7)]);

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

        assert!(
            matches!(outcome, HoldTermination::AttemptEnded),
            "a rejection with the source alive returns to holding, not to a terminal state: {outcome:?}"
        );
        assert_eq!(control.candidate_reports().len(), 1);
        let acceptance = control.acceptance_reports();
        assert_eq!(acceptance.len(), 1);
        assert_eq!(acceptance[0].status, AcceptanceStatus::Rejected);
        assert!(
            acceptance[0].acceptance_receipt.is_none(),
            "§3.7: a rejection carries no receipt"
        );
        assert!(
            acceptance[0]
                .failure_reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty()),
            "a rejection says why"
        );
    }

    /// A §3.6 report the server refuses ends the hold with NO ack — and with NO
    /// acceptance run.
    ///
    /// Verifying a candidate the server does not know about could only produce a
    /// verdict about nothing: §3.7 names the candidate in its PATH, so there is
    /// no endpoint the outcome could be sent to. Spending a disposable restore
    /// on it would burn minutes of the author's hold for a result that can never
    /// leave the process.
    #[test]
    fn a_refused_candidate_report_ends_the_hold_before_acceptance_and_without_an_ack() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control =
            ScriptedControl::new(vec![capture(1, "cand_1", true)]).refusing_the_candidate_report();
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

        assert!(
            matches!(outcome, HoldTermination::TornDownWithoutAck { .. }),
            "expected a torn-down hold, got {outcome:?}"
        );
        assert_eq!(
            outcome.terminal_ack_reason(),
            None,
            "a builder that could not report its candidate may not assert a job-terminal state"
        );
        assert_eq!(
            lifecycle.executes, 0,
            "no seal_at argv may run for a candidate the server refused"
        );
        assert!(
            control.acceptance_reports().is_empty(),
            "and no verdict may be sent about it"
        );
    }

    /// A §3.7 report the server refuses ends the hold the same way — no ack.
    #[test]
    fn a_refused_acceptance_report_tears_down_without_an_ack() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control =
            ScriptedControl::new(vec![capture(1, "cand_1", true)]).refusing_the_acceptance_report();
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

        assert_eq!(outcome.terminal_ack_reason(), None, "{outcome:?}");
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
            Some(TerminalAckReason::AttemptEnded)
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
        assert_eq!(
            outcome.terminal_ack_reason(),
            Some(TerminalAckReason::Discarded)
        );
    }

    // ── control fault → torn down LOCALLY, no terminal ack (§3.8) ───────────
    #[test]
    fn control_fault_tears_down_without_a_terminal_ack() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // One good hold, then the claim is fenced out from under the builder.
        let mut control = FaultingControl {
            head: vec![hold(true)],
            idx: 0,
        };
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

        assert_eq!(cap.calls, 0, "a fenced claim never captures");
        assert!(
            matches!(outcome, HoldTermination::TornDownWithoutAck { .. }),
            "got {outcome:?}"
        );
        // The whole point: lease expiry is server-owned, so there is NO legal
        // terminal ack to send here (§3.8 has no `lease_expired` reason).
        assert_eq!(outcome.terminal_ack_reason(), None);
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
            Some(TerminalAckReason::AcceptanceFailedSourceLost)
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
            Some(TerminalAckReason::BuildFailed)
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

    // ── the lease survives the two steps that never poll ────────────────────

    #[test]
    fn the_lease_is_kept_alive_across_the_capture_and_the_acceptance() {
        // The property the whole hold depends on. The lease is renewed on the
        // control poll, and the two longest steps of a real Step 4 do not poll:
        // the capture (pause + seal + upload) and the acceptance run (a
        // disposable restore + the seal_at command, bounded by a `total_deadline`
        // measured in minutes). A capture directive can arrive with two thirds of
        // the lease window already spent, so a hold that only renews on the poll
        // comes back from a SUCCESSFUL capture-and-accept to an expired lease —
        // and then throws the candidate, the §3.6 report and the author's
        // 30-minute session away.
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::new(vec![capture(1, "cand_1", true)]);
        let mut cap = ScriptedCapture::ok("cand_1", false).driving_the_lease(3);
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

        assert!(matches!(outcome, HoldTermination::Accepted { .. }));
        // 3 from the capture + one per PRODUCTIVE acceptance phase (capture
        // candidate, create session, restore, execute). The teardown phases
        // deliberately do not keepalive.
        assert_eq!(
            control.keepalives, 7,
            "the capture drove 3 and each productive acceptance phase drove one"
        );
    }

    #[test]
    fn a_lease_lost_during_the_capture_tears_down_without_an_ack() {
        // Fail-closed, and the capture's own return value does not get a vote: a
        // backend that was told mid-seal that the lease is gone and finished the
        // seal anyway has produced a candidate nobody can report — every call
        // that would carry it is a 409, and §3.8 has no ack for a dead lease.
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Each poll advances 10 minutes against a 30-minute TTL, so a hold that
        // did NOT stop on the lease still terminates (as `AttemptEnded`) instead
        // of looping — the assertion below has to distinguish the two outcomes,
        // not hang waiting for one.
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(10 * 60),
            vec![capture(1, "cand_1", true)],
        )
        .losing_the_lease_after(1);
        // Capture drives the lease twice and returns a candidate regardless.
        let mut cap = ScriptedCapture::ok("cand_1", false).driving_the_lease(2);
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

        assert_eq!(
            lifecycle.captures, 0,
            "acceptance never runs on a claim the builder no longer holds"
        );
        match &outcome {
            HoldTermination::TornDownWithoutAck { failure_reason } => {
                assert!(failure_reason.contains("lease expired"), "{failure_reason}");
            }
            other => panic!("expected TornDownWithoutAck, got {other:?}"),
        }
        // Expiry is server-owned: there is no ack this builder may send.
        assert_eq!(outcome.terminal_ack_reason(), None);
    }

    #[test]
    fn a_lease_lost_during_the_acceptance_tears_down_without_an_ack() {
        // Same rule one step later. A run refused phase-by-phase because the
        // lease died reports as a REJECTION, and a rejection is an ack — exactly
        // the job-terminal claim a builder that cannot prove its lease may not
        // make. The lease's verdict has to outrank the run's.
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // The lease survives the capture's single keepalive and is gone by the
        // time the acceptance run asks. The advancing clock is there for the same
        // reason as above: a hold that ignored the lease must still terminate, so
        // the assertion is on WHICH outcome, not on whether one arrives.
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(10 * 60),
            vec![capture(1, "cand_1", true)],
        )
        .losing_the_lease_after(1);
        let mut cap = ScriptedCapture::ok("cand_1", false).driving_the_lease(1);
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

        assert_eq!(cap.calls, 1, "the capture itself completed");
        assert_eq!(
            lifecycle.captures, 0,
            "the productive phases are refused, not merely reported on"
        );
        assert_eq!(lifecycle.executes, 0, "the seal_at command never ran");
        match &outcome {
            HoldTermination::TornDownWithoutAck { failure_reason } => {
                assert!(failure_reason.contains("lease expired"), "{failure_reason}");
            }
            other => panic!("expected TornDownWithoutAck, got {other:?}"),
        }
        assert_eq!(outcome.terminal_ack_reason(), None);
    }
}
