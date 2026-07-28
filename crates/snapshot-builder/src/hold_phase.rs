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
//! # The hold does not verify
//!
//! [`HoldPhase`] owns nothing that can reach `backend.restore`, and that is a
//! correctness property rather than tidiness. Acceptance restores the captured
//! candidate into a DISPOSABLE guest, while the Firecracker backend admits one
//! VMM per network identity: the tap name, the host and guest IPs and the
//! per-capsule vsock path are all per-slot constants, and the slot lock is held
//! for the whole hold. Driven from inside the loop — as it used to be — the
//! restore took the lock its own hold was holding and failed
//! `single-session backend busy` on every real builder, which is why the
//! interactive acceptance loop had never once completed on hardware.
//!
//! So the loop ENDS on a successful capture, returning
//! [`HoldOutcome::CapturedPendingVerification`]. The caller releases the guest —
//! killing the VMM, running `net_down()`, dropping the lock — and only then may
//! call [`verify_captured_candidate`], which demands the
//! [`crate::guest_capture::ReleasedHold`] that release mints. The ordering is
//! therefore checked by the compiler.
//!
//! Two consequences, both deliberate:
//!
//! * the author's preview is down for the length of one cold restore plus
//!   `seal_at`. The relay answers 503 with `Retry-After` instead of vanishing
//!   (`HoldIngress::gate_for_verification`), and the builder reports
//!   `WizardStage::Accepting`, so the wizard can say what is happening.
//! * a REJECTED candidate ends the attempt instead of returning to holding.
//!   ADR-012 `accepting_source_available` re-capture and RFC §8.2 repeated
//!   captures need a guest that is still alive, which is exactly what this
//!   ordering gives up. Restoring them is the `VacatedHold` follow-up: yield the
//!   slot rather than tear the hold down, and resume after a rejection. No new
//!   §3.8 reason is introduced — `AttemptEnded` is what `Accepted` already
//!   projects to, and the per-candidate verdict still reaches the author on §3.7.
//!
//! **Dead-code allow (scoped to this module):** `snapshot-builder` is a *binary*
//! crate, so `pub` items count as dead unless reached from `fn main`. Several
//! items here exist for the seams' contracts (and are exercised by the tests
//! below) without being named from the wiring; the allow is module-scoped
//! rather than crate-wide.
#![allow(dead_code)]

use std::time::{Duration, Instant};

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

/// #1160 — how many capture attempts ONE hold may spend.
///
/// A capture that fails with the source alive returns to holding, and the
/// control channel keeps delivering `capture` until a candidate is reported.
/// Unbounded, that pair is an amplifier: every poll drove a fresh pause + full
/// memory/vmstate seal, each writing a *new* content-addressed memory blob into
/// the job CAS (the bytes differ per capture, so nothing dedupes). Measured on
/// staging: 356 full snapshots in 15 minutes, and the capsule chooses its own
/// memory size, so the submitter picks the multiplier.
///
/// Three is the budget because the failures worth retrying are transient
/// (a stalled upload, a momentary disk or api hiccup) and those clear inside one
/// or two retries; a fourth attempt is not diagnosis, it is the amplifier.
pub const MAX_CAPTURE_ATTEMPTS: u32 = 3;

/// #1160 — the absolute wall the capture sequence may not cross, measured from
/// the FIRST attempt of the hold.
///
/// The attempt cap already bounds the count; this bounds the *duration*, and the
/// two fail closed independently. A capture that hangs for a long time in the
/// backend (a stuck upload retrying its own backoff, an unresponsive VMM) burns
/// wall-clock rather than attempts, and without this the hold would keep a slot,
/// a tap and a live VM occupied until the 30-minute TTL for a capture sequence
/// that has already proved it is not converging.
pub const CAPTURE_RETRY_WINDOW: Duration = Duration::from_secs(10 * 60);

/// #1160 — the delay before the first retry, doubled for each retry after it
/// (15s, then 30s).
///
/// It is spent by REFUSING captures while the loop keeps polling, not by
/// sleeping: the poll is what renews the lease (§3.2), and a hold that slept
/// through its backoff would come back to a claim it no longer holds.
pub const CAPTURE_RETRY_BACKOFF: Duration = Duration::from_secs(15);

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

/// #1160 — what the [`CaptureBudget`] says about capturing *right now*.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CaptureAdmission {
    /// Spend an attempt: run the capture.
    Go,
    /// A retry is due later. Keep holding (and keep polling, which is what keeps
    /// the lease alive) — do NOT capture.
    BackingOff,
    /// The budget is spent. The hold is over; carries the reason for the ack.
    Exhausted(String),
}

/// #1160 — the bounded capture retry budget for ONE hold.
///
/// Both bounds are checked by the single [`CaptureBudget::exhausted`] predicate,
/// deliberately: the loop consults it from two places (before spending an
/// attempt, and immediately after a failure so a spent budget ends the hold
/// without waiting for another directive), and two copies of "is the budget
/// gone?" is two chances to disagree.
#[derive(Debug, Clone)]
struct CaptureBudget {
    max_attempts: u32,
    window: Duration,
    backoff: Duration,
    /// Attempts SPENT. Incremented when an attempt is admitted, never when it
    /// completes — a capture that never returns a verdict has still spent one.
    attempts: u32,
    /// Set on the first admitted attempt, so a hold that never captures is never
    /// on a capture clock.
    window_closes_at: Option<Instant>,
    /// Set after a failure; until it passes, `Capture` directives are refused.
    next_attempt_at: Option<Instant>,
    /// The last capture failure, quoted into the terminal reason so the author
    /// is told what actually went wrong rather than only that it stopped.
    last_failure: Option<String>,
}

impl CaptureBudget {
    fn new(max_attempts: u32, window: Duration, backoff: Duration) -> Self {
        Self {
            max_attempts,
            window,
            backoff,
            attempts: 0,
            window_closes_at: None,
            next_attempt_at: None,
            last_failure: None,
        }
    }

    /// Why the budget is spent, or `None` while it still has room.
    ///
    /// THE gate. Both bounds live here so that removing either one is a single,
    /// visible edit — and so the mutation test that deletes the attempt cap has
    /// exactly one place to bite.
    fn exhausted(&self, now: Instant) -> Option<String> {
        let last = self
            .last_failure
            .as_deref()
            .map(|m| format!("; last failure: {m}"))
            .unwrap_or_default();
        if self.attempts >= self.max_attempts {
            return Some(format!(
                "capture budget spent: {} of {} attempts failed with the source \
                 still alive{last}",
                self.attempts, self.max_attempts
            ));
        }
        if self
            .window_closes_at
            .is_some_and(|closes_at| now >= closes_at)
        {
            return Some(format!(
                "capture retry window closed after {} attempt(s){last}",
                self.attempts
            ));
        }
        None
    }

    /// May a capture run now? Spends an attempt when it answers [`CaptureAdmission::Go`].
    fn admit(&mut self, now: Instant) -> CaptureAdmission {
        if let Some(reason) = self.exhausted(now) {
            return CaptureAdmission::Exhausted(reason);
        }
        if self.next_attempt_at.is_some_and(|due_at| now < due_at) {
            return CaptureAdmission::BackingOff;
        }
        // The window starts at the first attempt, not at hold entry: an author
        // who spends 20 minutes on their app before pressing capture has not
        // spent any of their retry window doing it.
        self.window_closes_at.get_or_insert(now + self.window);
        self.attempts += 1;
        CaptureAdmission::Go
    }

    /// Record a capture that failed with the source alive, and arm the backoff.
    fn record_failure(&mut self, now: Instant, message: String) {
        self.last_failure = Some(message);
        // 15s, 30s, 60s … — shifted by the attempts already spent. Saturating on
        // both the shift and the multiply so a large budget can never wrap the
        // delay round to zero, which would silently restore the amplifier.
        let doubling = self.attempts.saturating_sub(1).min(16);
        let delay = self.backoff.saturating_mul(1u32 << doubling);
        self.next_attempt_at = Some(now + delay);
    }

    /// Attempts spent so far.
    fn attempts_spent(&self) -> u32 {
        self.attempts
    }
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
    /// #1160 — every capture attempt this hold was allowed failed with the
    /// source ALIVE, or the retry window closed before one succeeded.
    ///
    /// Distinct from [`Self::AcceptanceFailedSourceLost`] on purpose: the guest
    /// survived every one of these failures, so ADR-012's terminal branch does
    /// not apply and the author's next move is a NEW attempt, not a lost source.
    /// It is a failure rather than an orderly [`Self::AttemptEnded`] because the
    /// author asked for a capture and did not get one — saying "attempt ended"
    /// would hide that.
    CaptureBudgetExhausted {
        /// How many attempts were spent.
        attempts: u32,
        /// Which bound was hit, and the last capture failure behind it.
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

/// What the hold loop produced.
///
/// The loop no longer verifies. A successful capture ENDS it and hands the
/// candidate back for verification, because acceptance restores a disposable
/// guest and the backend admits one VMM per network identity — see
/// [`crate::guest_capture::ReleasedHold`]. Splitting the outcome is what makes
/// "verify only after the guest is released" expressible in the type system
/// instead of in a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldOutcome {
    /// The hold ended without producing a candidate to verify.
    Terminal(HoldTermination),
    /// A candidate was captured and reported (§3.6). Acceptance has NOT run.
    CapturedPendingVerification(CapturedPendingVerification),
}

/// A sealed, reported candidate awaiting acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPendingVerification {
    /// The §3.6 report already sent for this candidate.
    pub report: CandidateReportRequest,
    /// The epoch that drove the capture (echoed into the §3.7 verdict).
    pub capture_epoch: u64,
    /// ADR-012: the source guest was already gone when it was captured.
    pub source_lost: bool,
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
            // #1160. `build_failed` is the only failure reason §3.8 offers that
            // does not claim the source was lost — and the source was NOT lost
            // here, which is the whole distinction. `failure_stage` carries what
            // `build_failed` alone would blur: the build succeeded and the hold
            // reached `holding`; it is the capture that could not be sealed.
            HoldTermination::CaptureBudgetExhausted { .. } => Some(TerminalAckReason::BuildFailed),
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
            HoldTermination::CaptureBudgetExhausted { .. } => Some(WizardFailureStage::CaptureSeal),
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

/// The pure hold-phase driver.
///
/// It deliberately owns NOTHING that can reach `backend.restore`. The
/// acceptance lifecycle used to live here, and that is precisely what made the
/// defect expressible: the loop called the disposable restore while its own
/// guest was still live, holding the slot. Verification moved out to
/// [`verify_captured_candidate`], which cannot be called without a
/// [`crate::guest_capture::ReleasedHold`].
pub struct HoldPhase<'a> {
    control: &'a mut dyn ControlSource,
    capture: &'a mut dyn CaptureAction,
    eligibility: &'a mut dyn EligibilitySource,
    extend: &'a mut dyn ExtendPolicy,
    clock: &'a dyn MonotonicClock,
    fencing: Fencing4,
    hold_ttl: Duration,
}

impl<'a> HoldPhase<'a> {
    /// Assemble a hold phase from its seams. `fencing` is the attempt's FENCING-4
    /// identity (echoed into candidate reports); `hold_ttl` is the hold deadline
    /// budget ([`DEFAULT_HOLD_TTL`] by default).
    ///
    /// The acceptance config, cancellation token and lifecycle are NOT taken:
    /// they belong to [`verify_captured_candidate`], which runs after the guest
    /// is released.
    pub fn new(
        control: &'a mut dyn ControlSource,
        capture: &'a mut dyn CaptureAction,
        eligibility: &'a mut dyn EligibilitySource,
        extend: &'a mut dyn ExtendPolicy,
        clock: &'a dyn MonotonicClock,
        fencing: Fencing4,
        hold_ttl: Duration,
    ) -> Self {
        Self {
            control,
            capture,
            eligibility,
            extend,
            clock,
            fencing,
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
    /// 4. On capture, run the capture-action with the claim's lease kept alive
    ///    across it ([`LeaseKeepalive`]), ending the hold as
    ///    [`HoldTermination::TornDownWithoutAck`] the moment the lease is lost,
    ///    ahead of whatever the capture itself returned. A capture FAILURE keeps
    ///    the author's preview alive and returns to holding unless the source is
    ///    lost (ADR-012), exactly as before.
    /// 5. A capture SUCCESS ends the loop with
    ///    [`HoldOutcome::CapturedPendingVerification`]. Acceptance does not run
    ///    here — see the type's doc and [`verify_captured_candidate`].
    pub fn run(&mut self) -> Result<HoldOutcome, FatalInternalError> {
        // (1) Entry eligibility gate — fail CLOSED before any hold/poll/capture so
        // an ineligible capsule (External State / restore-time bindings, #1090)
        // can never reach a capture.
        //
        // The proof is discarded rather than carried to acceptance: acceptance no
        // longer runs in this loop, and `ClaimContractEligibility::eligibility`
        // is a pure re-analysis of the pinned contract, so minting a fresh proof
        // at verification time costs one analysis and removes a value that would
        // otherwise have to outlive the guest that justified it.
        if let Err(failure) = self.eligibility.eligibility() {
            return Ok(HoldOutcome::Terminal(HoldTermination::FailedClosed {
                failure_reason: failure.to_string(),
            }));
        }

        let mut observed_epoch: u64 = 0;
        let mut deadline = self.clock.now() + self.hold_ttl;
        // #1160 — the bounded capture retry budget for this hold.
        let mut budget = CaptureBudget::new(
            MAX_CAPTURE_ATTEMPTS,
            CAPTURE_RETRY_WINDOW,
            CAPTURE_RETRY_BACKOFF,
        );

        loop {
            // (2) Fail-closed deadline gate FIRST: never force a capture past the
            // hold deadline (SSOT §5). On reaching it, consult the extend seam.
            if self.clock.now() >= deadline {
                match self.extend.on_deadline() {
                    Some(extra) => {
                        deadline = self.clock.now() + extra;
                        continue;
                    }
                    None => return Ok(HoldOutcome::Terminal(HoldTermination::AttemptEnded)),
                }
            }

            // A control fault ends the hold LOCALLY with no ack (see
            // `ControlFault`): the lease is dead or in doubt, so there is no
            // job-terminal claim this builder is entitled to make.
            let response = match self.control.poll(observed_epoch) {
                Ok(response) => response,
                Err(fault) => {
                    return Ok(HoldOutcome::Terminal(HoldTermination::TornDownWithoutAck {
                        failure_reason: fault.message,
                    }));
                }
            };
            // Adopt the authoritative server epoch as the observed command cursor
            // (ADR-008 / ControlResponse rules): monotonic, never part of fencing.
            observed_epoch = observed_epoch.max(response.server_capture_epoch);

            match response.directive {
                ControlDirective::Hold => continue,
                ControlDirective::Discard => {
                    return Ok(HoldOutcome::Terminal(HoldTermination::Discarded));
                }
                ControlDirective::Capture => {
                    // (3) ADR-007 causality: refuse capture until the quiesced ack
                    // has set pause_permitted. No capture before the quiesced ack.
                    if !response.pause_permitted {
                        continue;
                    }
                    let epoch = response.server_capture_epoch;

                    // (3a) ADR-008 capture-epoch monotonicity used to be enforced
                    // here, guarding against a stale or duplicate Capture
                    // re-driving capture after an ADR-012 source-available
                    // acceptance failure returned the loop to holding.
                    //
                    // REMOVED with that path, not forgotten. A successful capture
                    // now ENDS this loop, so no directive after it is ever
                    // consumed and the guard had no reachable case — it read a
                    // value the code below could only write on its way out.
                    // Keeping an unreachable comparison would have been a
                    // standing invitation to reason about a state the loop cannot
                    // be in.
                    //
                    // The `VacatedHold` follow-up restores return-to-holding, and
                    // MUST restore this guard with it: once a second capture is
                    // possible again, a replayed epoch can drive one.

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
                        return Ok(HoldOutcome::Terminal(HoldTermination::FailedClosed {
                            failure_reason: format!(
                                "control delivered `capture` for epoch {epoch} with no \
                                 candidate_id: the candidate could never be reported"
                            ),
                        }));
                    };

                    // (3c) #1160 — the capture budget, checked AFTER the two
                    // fail-closed refusals above and BEFORE any guest work.
                    //
                    // Its place in the order is the point: a directive refused
                    // for missing `pause_permitted` or a missing candidate id
                    // never reached a capture, so it must not spend one — the
                    // budget counts captures the guest actually paid for, not
                    // directives the server sent.
                    match budget.admit(self.clock.now()) {
                        CaptureAdmission::Go => {}
                        // A retry is armed but not yet due. Fall back to holding
                        // — the next poll renews the lease, and the deadline gate
                        // at the top of the loop still owns the hold TTL, so a
                        // backoff can never outlive the hold it is inside.
                        CaptureAdmission::BackingOff => continue,
                        CaptureAdmission::Exhausted(failure_reason) => {
                            return Ok(HoldOutcome::Terminal(
                                HoldTermination::CaptureBudgetExhausted {
                                    attempts: budget.attempts_spent(),
                                    failure_reason,
                                },
                            ));
                        }
                    }

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
                            return Ok(HoldOutcome::Terminal(
                                HoldTermination::TornDownWithoutAck {
                                    failure_reason: fault.message.clone(),
                                },
                            ));
                        }
                        captured
                    };
                    let held = match captured {
                        Ok(held) => held,
                        Err(err) => {
                            // No candidate report (SSOT §3.6). Source alive →
                            // return to holding; source lost → terminal (ADR-012).
                            //
                            // The ADR-012 branch is checked FIRST and is not
                            // charged to the retry budget's exhaustion message:
                            // a lost guest ends the attempt for a reason of its
                            // own, and reporting it as a spent capture budget
                            // would tell the author to try again against a guest
                            // that no longer exists.
                            if err.source_lost {
                                return Ok(HoldOutcome::Terminal(
                                    HoldTermination::AcceptanceFailedSourceLost {
                                        failure_reason: err.message,
                                    },
                                ));
                            }
                            // #1160 — arm the backoff, then end the hold HERE if
                            // that was the last attempt. Waiting for another
                            // `capture` directive to discover it would leave the
                            // decision to the control channel, which is the party
                            // that was driving the amplification: the failing
                            // hold must terminate on its own budget.
                            let now = self.clock.now();
                            budget.record_failure(now, err.message);
                            if let Some(failure_reason) = budget.exhausted(now) {
                                return Ok(HoldOutcome::Terminal(
                                    HoldTermination::CaptureBudgetExhausted {
                                        attempts: budget.attempts_spent(),
                                        failure_reason,
                                    },
                                ));
                            }
                            continue;
                        }
                    };
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
                        return Ok(HoldOutcome::Terminal(HoldTermination::TornDownWithoutAck {
                            failure_reason: format!("candidate report: {}", fault.message),
                        }));
                    }

                    // (6) The hold ends HERE, with the candidate sealed, uploaded
                    // and reported but NOT yet verified.
                    //
                    // Acceptance restores the candidate into a disposable guest,
                    // and the backend admits one VMM per network identity — so it
                    // cannot run while this loop's guest is live. Returning the
                    // candidate instead of verifying it in place is what makes
                    // that orderable: the caller releases the guest, and only a
                    // `ReleasedHold` opens `verify_captured_candidate`.
                    return Ok(HoldOutcome::CapturedPendingVerification(
                        CapturedPendingVerification {
                            report,
                            capture_epoch: epoch,
                            source_lost: held.source_lost,
                        },
                    ));
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
/// Verify a captured candidate, AFTER the held guest has been released.
///
/// This is the acceptance run that used to live inside the hold loop. It moved
/// out because it restores a disposable guest, and the Firecracker backend
/// admits one VMM per network identity: run from inside the loop it took the
/// same slot lock its own hold was holding and failed
/// `single-session backend busy` — every time, on every real builder.
///
/// The `_released` token is the whole point. It can only come from
/// [`GuestCaptureAction::release`], which kills and reaps the VMM, runs
/// `net_down()` and drops the slot lock, so "verify only after the guest is
/// gone" is checked by the compiler rather than remembered by a reader. See
/// [`ReleasedHold`] for the three failures beyond the lock that ordering also
/// prevents — including the one that does NOT fail loudly, where readiness
/// probes the held guest and accepts a candidate it never touched.
///
/// Deliberate semantic change: a REJECTED candidate ends the attempt
/// ([`HoldTermination::AttemptEnded`]) instead of returning to holding.
/// ADR-012 `accepting_source_available` re-capture needs a guest that is still
/// alive, which is exactly what this ordering gives up. The branch was
/// unreachable in practice anyway (every acceptance failed on the slot lock
/// before reaching it). Restoring it is the `VacatedHold` follow-up: yield the
/// slot instead of tearing the hold down, and resume after a rejection.
///
/// No new §3.8 reason: `AttemptEnded` is the same terminal reason `Accepted`
/// projects to, and the per-candidate verdict already reached the author on §3.7.
#[allow(clippy::too_many_arguments)]
pub fn verify_captured_candidate(
    control: &mut dyn ControlSource,
    lifecycle: &mut impl DisposableAcceptanceLifecycle,
    eligibility: &mut dyn EligibilitySource,
    config: &AcceptanceConfig,
    cancellation: &snapshot::acceptance::AcceptanceCancellation,
    clock: &dyn MonotonicClock,
    fencing: &Fencing4,
    pending: CapturedPendingVerification,
    _released: &crate::guest_capture::ReleasedHold,
) -> Result<HoldTermination, FatalInternalError> {
    // A fresh proof: `accept()` consumes one by value, and re-analysing the
    // pinned contract fails closed if anything about it stopped qualifying.
    let proof = match eligibility.eligibility() {
        Ok(proof) => proof,
        Err(failure) => {
            return Ok(HoldTermination::FailedClosed {
                failure_reason: failure.to_string(),
            });
        }
    };

    // The lease is still kept alive across the run: acceptance's own
    // `total_deadline` is minutes, longer than a lease window, so an unwrapped
    // run routinely finishes on a lease that died under it.
    let run = {
        let mut lease = HoldLease::new(control);
        let run = RunningSnapshotAcceptance::accept(
            &mut LeaseKeptLifecycle {
                inner: lifecycle,
                lease: &mut lease,
            },
            proof,
            config,
            cancellation,
            clock,
        )?;
        // The lease's verdict outranks the run's: a run refused phase-by-phase
        // because the lease died reports as a rejection, and a rejection is an
        // ACK — the one claim a builder with no lease may not make.
        if let Some(fault) = lease.fault() {
            return Ok(HoldTermination::TornDownWithoutAck {
                failure_reason: fault.message.clone(),
            });
        }
        run
    };
    if let Some(failure) = run.failure() {
        // The control-plane candidate row intentionally stores no free-form
        // rejection text. Keep one bounded, non-secret diagnostic at the
        // builder boundary so a real disposable-restore rejection is
        // distinguishable from the generic terminal `AttemptEnded` ack.
        eprintln!(
            "[builder] interactive_capture attempt {} candidate {} acceptance rejected: \
             {failure}; attempts={:?}",
            fencing.submission_attempt_id, pending.report.candidate_id, run.receipt.attempts
        );
    }

    // §3.7 — the verdict for THIS candidate, accepted or rejected. Sent on both
    // branches: a rejected candidate nobody is told about leaves the author's
    // wizard showing a validation that is still running when it already failed.
    let acceptance = match acceptance_request(fencing, pending.capture_epoch, &run) {
        Ok(request) => request,
        Err(reason) => {
            return Ok(HoldTermination::FailedClosed {
                failure_reason: reason,
            });
        }
    };
    if let Err(fault) = control.report_acceptance(&acceptance) {
        return Ok(HoldTermination::TornDownWithoutAck {
            failure_reason: format!("candidate acceptance: {}", fault.message),
        });
    }

    if run.is_accepted() {
        return Ok(HoldTermination::Accepted {
            report: pending.report,
        });
    }

    // ADR-012: a lost source is still its own terminal reason, because the
    // author cannot retry from a guest that is gone.
    if pending.source_lost {
        return Ok(HoldTermination::AcceptanceFailedSourceLost {
            failure_reason: run
                .failure()
                .map(|f| f.to_string())
                .unwrap_or_else(|| "acceptance rejected".to_string()),
        });
    }
    // Rejected with the source nominally available. There is no guest to return
    // to — this function was only reachable because it was released — so the
    // attempt ends. The §3.7 verdict above already carried the reason.
    Ok(HoldTermination::AttemptEnded)
}

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

    use std::cell::Cell;
    use std::rc::Rc;

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

    /// A lifecycle that models the ONE behaviour the real Firecracker backend
    /// has and [`FakeLifecycle`] does not: it REFUSES a restore while a VMM is
    /// live on the same network identity.
    ///
    /// `boot_and_hold` holds `work_root/{netns|tap}.lock` for the whole hold and
    /// `restore` acquires the same path, so on real hardware a restore driven
    /// from inside the hold loop returns `single-session backend busy`. Because
    /// `FakeLifecycle::restore_candidate` returns `Ok(())` unconditionally, it
    /// cannot express that rule — which is exactly why every hold test passed on
    /// a tree whose production ordering was wrong.
    struct SingleSlotLifecycle {
        inner: FakeLifecycle,
        guest_live: Rc<Cell<bool>>,
    }

    impl DisposableAcceptanceLifecycle for SingleSlotLifecycle {
        fn capture_candidate(
            &mut self,
            attempt: u32,
            budget: &AcceptanceBudget,
        ) -> Result<CandidateSnapshot, String> {
            self.inner.capture_candidate(attempt, budget)
        }

        fn create_disposable_session(
            &mut self,
            candidate: &CandidateSnapshot,
            budget: &AcceptanceBudget,
        ) -> Result<DisposableSessionHandle, String> {
            self.inner.create_disposable_session(candidate, budget)
        }

        fn restore_candidate(
            &mut self,
            session: &DisposableSessionHandle,
            candidate: &CandidateSnapshot,
            budget: &AcceptanceBudget,
        ) -> Result<(), String> {
            if self.guest_live.get() {
                return Err(
                    "single-session backend busy: tap 'fctap0' is held by another session"
                        .to_string(),
                );
            }
            self.inner.restore_candidate(session, candidate, budget)
        }

        fn execute_exact_argv(
            &mut self,
            session: &DisposableSessionHandle,
            argv: &[String],
            timeout: Duration,
            budget: &AcceptanceBudget,
        ) -> Result<VerificationOutcome, String> {
            self.inner
                .execute_exact_argv(session, argv, timeout, budget)
        }

        fn terminate_process_tree(
            &mut self,
            session: &DisposableSessionHandle,
        ) -> Result<(), String> {
            self.inner.terminate_process_tree(session)
        }

        fn destroy_disposable_session(
            &mut self,
            session: DisposableSessionHandle,
        ) -> Result<(), String> {
            self.inner.destroy_disposable_session(session)
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
        /// #1160 — results consumed IN ORDER before falling back to `result`,
        /// so a test can script "fails twice, then succeeds" and see which
        /// attempt the budget actually let through.
        head: Vec<Result<HeldCapture, CaptureError>>,
        calls: u32,
        lease_drives: u32,
        seen_candidate_ids: Vec<String>,
        /// The clock, advanced by however long a capture is scripted to take —
        /// how a test spends the retry WINDOW without spending real minutes.
        clock: Option<FakeClock>,
        takes: Duration,
        /// When each attempt STARTED (needs `clock`). The gap between two of
        /// these is the backoff, measured rather than assumed.
        call_instants: Vec<Instant>,
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
                head: Vec::new(),
                calls: 0,
                lease_drives: 0,
                seen_candidate_ids: Vec::new(),
                clock: None,
                takes: Duration::ZERO,
                call_instants: Vec::new(),
            }
        }

        /// #1160 — a capture that always fails with the GUEST STILL ALIVE: the
        /// exact shape that used to return to holding forever (a seal that
        /// worked and an upload that did not).
        fn always_failing(message: &str) -> Self {
            Self {
                result: Err(CaptureError {
                    source_lost: false,
                    message: message.to_string(),
                }),
                ..Self::ok("unused", false)
            }
        }

        /// #1160 — fails `n` times with the source alive, then succeeds.
        fn failing_then_ok(n: usize, candidate_id: &str) -> Self {
            let ok = Self::ok(candidate_id, false);
            Self {
                head: (0..n)
                    .map(|i| {
                        Err(CaptureError {
                            source_lost: false,
                            message: format!("upload stalled ({})", i + 1),
                        })
                    })
                    .collect(),
                ..ok
            }
        }

        /// A capture long enough to need `n` keepalives — a real one.
        fn driving_the_lease(mut self, n: u32) -> Self {
            self.lease_drives = n;
            self
        }

        /// #1160 — each attempt burns `takes` of the shared clock.
        fn taking(mut self, clock: FakeClock, takes: Duration) -> Self {
            self.clock = Some(clock);
            self.takes = takes;
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
            if let Some(clock) = &self.clock {
                self.call_instants.push(clock.now());
                clock.advance(self.takes);
            }
            if self.head.is_empty() {
                self.result.clone()
            } else {
                self.head.remove(0)
            }
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

    /// Drive a hold and, if it captured, verify the candidate — mirroring the
    /// production ordering in `main.rs` exactly, including the release between
    /// the two.
    ///
    /// The release is what the fix is about, so the helper performs it rather
    /// than letting each test remember to: `guest_live` flips to `false` at the
    /// same point `capture.release()` happens for real. A helper that verified
    /// without flipping it would reproduce the bug in the harness and hide it
    /// again.
    #[allow(clippy::too_many_arguments)]
    fn run_hold_then_verify(
        control: &mut dyn ControlSource,
        capture: &mut ScriptedCapture,
        eligibility: &mut dyn EligibilitySource,
        extend: &mut dyn ExtendPolicy,
        lifecycle: &mut impl DisposableAcceptanceLifecycle,
        clock: &FakeClock,
        cancellation: &AcceptanceCancellation,
        hold_ttl: Duration,
        guest_live: Option<&Rc<Cell<bool>>>,
    ) -> HoldTermination {
        let outcome = {
            let mut phase = HoldPhase::new(
                control,
                capture,
                eligibility,
                extend,
                clock,
                fencing(),
                hold_ttl,
            );
            phase.run().expect("no fatal internal error")
        };
        match outcome {
            HoldOutcome::Terminal(termination) => termination,
            HoldOutcome::CapturedPendingVerification(pending) => {
                // THE RELEASE. Everything the token stands for happens here.
                if let Some(flag) = guest_live {
                    flag.set(false);
                }
                verify_captured_candidate(
                    control,
                    lifecycle,
                    eligibility,
                    &config(),
                    cancellation,
                    clock,
                    &fencing(),
                    pending,
                    &crate::guest_capture::ReleasedHold::for_test(),
                )
                .expect("no fatal internal error")
            }
        }
    }

    /// Back-compat shim for the tests that do not care about the slot rule.
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
        run_hold_then_verify(
            control,
            capture,
            eligibility,
            extend,
            lifecycle,
            clock,
            cancellation,
            hold_ttl,
            None,
        )
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
    fn a_rejected_candidate_reports_its_rejection_before_the_attempt_ends() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Capture once; the rejection ends the attempt directly. The trailing
        // `hold` directive is never consumed.
        //
        // RENAMED: this used to be `..._and_keeps_holding` and reached
        // `AttemptEnded` via the TTL. It now reaches it directly, so the old
        // name and message described behaviour the code no longer has while the
        // assertions still passed — the shape of test that hides the next
        // regression. What it actually pins is the §3.7 contract: a rejection is
        // reported, with a reason and no receipt.
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
            "a rejection with the source alive ends the attempt: {outcome:?}"
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
    fn acceptance_failure_with_source_available_ends_the_attempt() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Capture (permitted, source alive), acceptance rejects (exit 1). The
        // second directive is scripted but must never be reached: the hold is
        // over once the candidate is captured.
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
        // CHANGED, deliberately. This used to assert `Discarded` — the loop
        // returned to holding after a rejection and ended on the next
        // directive (ADR-012 `accepting_source_available`).
        //
        // Verification now runs after the guest is released, because acceptance
        // restores a second guest and the backend admits one VMM per network
        // identity. There is no live guest to return to, so a rejection ends the
        // attempt. The author still gets the verdict on §3.7; what they lose is
        // re-capture, which is the `VacatedHold` follow-up.
        //
        // The old behaviour was unreachable in production anyway: every
        // acceptance failed `single-session backend busy` before it could reject
        // for a reason the author caused.
        assert!(
            matches!(outcome, HoldTermination::AttemptEnded),
            "a rejection ends the attempt, got {outcome:?}"
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

    // ── #1160 the bounded capture retry budget ──────────────────────────────
    //
    // The defect these pin down: a capture that failed with the source ALIVE
    // returned to holding, the control channel kept delivering `capture` until a
    // candidate was reported, and nothing in between counted or waited. Every
    // poll drove a fresh pause + full memory seal — 356 snapshots in 15 minutes
    // on staging, at a memory size the SUBMITTER picks.
    //
    // Production defaults are what is exercised: `HoldPhase::run` reads
    // `MAX_CAPTURE_ATTEMPTS` / `CAPTURE_RETRY_WINDOW` / `CAPTURE_RETRY_BACKOFF`
    // directly and no fixture can inject a different bound, so a test that goes
    // green here is a statement about the builder that ships.

    /// The load-bearing one: three attempts, then the hold is over.
    ///
    /// The control channel offers `capture` FOREVER here (a `ScriptedControl`
    /// clamps to its last response), and the hold TTL is the full 30 minutes, so
    /// nothing but the budget itself can end this run. Deleting the attempt cap
    /// leaves it polling for the whole simulated TTL and failing on the count.
    #[test]
    fn a_capture_that_keeps_failing_is_bounded_at_three_attempts() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // The production cadence: a control poll every 2s (HOLD_CONTROL_POLL_INTERVAL).
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(2),
            vec![capture(1, "cand_1", true)],
        );
        let mut cap = ScriptedCapture::always_failing("artifact upload to R2 failed")
            .taking(clock.clone(), Duration::ZERO);
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

        // The literal 3, not `MAX_CAPTURE_ATTEMPTS`: asserting against the
        // constant would stay green if the constant itself were raised, and the
        // NUMBER is the mitigation.
        assert_eq!(
            cap.calls, 3,
            "a hold spends at most three capture attempts; got {}",
            cap.calls
        );
        assert_eq!(MAX_CAPTURE_ATTEMPTS, 3, "the shipped bound is three");
        let HoldTermination::CaptureBudgetExhausted {
            attempts,
            failure_reason,
        } = &outcome
        else {
            panic!("expected a spent capture budget, got {outcome:?}");
        };
        assert_eq!(*attempts, 3);
        assert!(
            failure_reason.contains("capture budget spent"),
            "{failure_reason}"
        );
        // The author is told what actually broke, not just that it stopped.
        assert!(
            failure_reason.contains("artifact upload to R2 failed"),
            "the last capture failure must survive into the terminal reason: \
             {failure_reason}"
        );
        // §3.8: a real failure ack, refined to the stage that failed. NOT
        // `attempt_ended` — the author asked for a capture and did not get one.
        assert_eq!(
            outcome.terminal_ack_reason(),
            Some(TerminalAckReason::BuildFailed)
        );
        assert_eq!(
            outcome.failure_stage(),
            Some(WizardFailureStage::CaptureSeal)
        );
        // No candidate was ever reported (SSOT §3.6: a failed capture reports
        // nothing), so nothing downstream can publish one of these.
        assert!(control.candidate_reports().is_empty());
        assert_eq!(lifecycle.executes, 0, "acceptance never ran");
    }

    /// The gap between two attempts is the backoff, and it is spent POLLING.
    ///
    /// Sleeping through it would be the obvious implementation and the wrong
    /// one: the poll is what renews the lease (§3.2), so a hold that slept 15s
    /// (then 30s) would come back to a claim it no longer holds — turning a
    /// retryable capture failure into a torn-down attempt.
    #[test]
    fn the_backoff_is_spent_holding_and_polling_not_capturing() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(2),
            vec![capture(1, "cand_1", true)],
        );
        let mut cap =
            ScriptedCapture::always_failing("seal failed").taking(clock.clone(), Duration::ZERO);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![]);

        run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
        );

        assert_eq!(cap.call_instants.len(), 3);
        let first_gap = cap.call_instants[1] - cap.call_instants[0];
        let second_gap = cap.call_instants[2] - cap.call_instants[1];
        assert!(
            first_gap >= CAPTURE_RETRY_BACKOFF,
            "first retry waited {first_gap:?}, expected at least {CAPTURE_RETRY_BACKOFF:?}"
        );
        assert!(
            second_gap >= CAPTURE_RETRY_BACKOFF * 2,
            "the backoff doubles: second retry waited {second_gap:?}, expected at \
             least {:?}",
            CAPTURE_RETRY_BACKOFF * 2
        );
        // Polls kept happening across those gaps — that is the lease staying
        // alive. Three captures against ~23 polls, not one poll per capture.
        assert!(
            control.polls > cap.calls as usize * 3,
            "the backoff must be spent polling ({} polls for {} captures)",
            control.polls,
            cap.calls
        );
    }

    /// The budget is a BOUND, not a ban: the last attempt may still succeed, and
    /// when it does the hold proceeds exactly as an unretried one would.
    #[test]
    fn a_retry_inside_the_budget_can_still_capture_and_accept() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(2),
            vec![capture(1, "cand_1", true)],
        );
        // Two transient failures, then the third attempt seals.
        let mut cap =
            ScriptedCapture::failing_then_ok(2, "cand_1").taking(clock.clone(), Duration::ZERO);
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

        assert_eq!(cap.calls, 3, "the third attempt is allowed to succeed");
        assert_eq!(
            control.candidate_reports().len(),
            1,
            "exactly the successful capture is reported (§3.6)"
        );
        assert!(
            matches!(outcome, HoldTermination::Accepted { .. }),
            "got {outcome:?}"
        );
    }

    /// The window closes even with attempts left.
    ///
    /// Two independent bounds, and this is the one the attempt cap cannot cover:
    /// a capture that hangs for minutes in the backend burns wall-clock instead
    /// of attempts, holding a slot, a tap and a live VM the whole time.
    #[test]
    fn the_retry_window_closes_even_with_attempts_left() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(2),
            vec![capture(1, "cand_1", true)],
        );
        // Each attempt grinds for six minutes before failing, so the second one
        // ends past the ten-minute window with a third attempt still unspent.
        let mut cap = ScriptedCapture::always_failing("the upload hung")
            .taking(clock.clone(), Duration::from_secs(6 * 60));
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

        assert_eq!(
            cap.calls, 2,
            "the window closed with an attempt still unspent"
        );
        let HoldTermination::CaptureBudgetExhausted { failure_reason, .. } = &outcome else {
            panic!("expected a closed retry window, got {outcome:?}");
        };
        assert!(
            failure_reason.contains("retry window closed"),
            "{failure_reason}"
        );
    }

    /// ADR-012 outranks the budget: a LOST guest is not a spent retry allowance.
    ///
    /// The two failures are told apart because the author's next move differs.
    /// A spent budget leaves a live app they can capture again in a new attempt;
    /// a lost source does not, and reporting one as the other would send them
    /// back to a guest that no longer exists.
    #[test]
    fn a_lost_source_is_terminal_on_its_own_reason_not_the_capture_budget() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(2),
            vec![capture(1, "cand_1", true)],
        );
        let mut cap = ScriptedCapture {
            result: Err(CaptureError {
                source_lost: true,
                message: "the guest could not be resumed".to_string(),
            }),
            ..ScriptedCapture::ok("cand_1", false)
        };
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

        assert_eq!(cap.calls, 1, "a lost source never retries");
        assert!(
            matches!(outcome, HoldTermination::AcceptanceFailedSourceLost { .. }),
            "a lost guest keeps its own ADR-012 reason, got {outcome:?}"
        );
        assert_eq!(
            outcome.terminal_ack_reason(),
            Some(TerminalAckReason::AcceptanceFailedSourceLost)
        );
    }

    /// The hold deadline still outranks a pending retry.
    ///
    /// A backoff armed for later must never resurrect a hold whose TTL has
    /// passed: the deadline gate runs first on every iteration, so the retry is
    /// simply never reached.
    #[test]
    fn a_retry_armed_past_the_hold_deadline_is_never_taken() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::advancing(
            clock.clone(),
            Duration::from_secs(2),
            vec![capture(1, "cand_1", true)],
        );
        let mut cap =
            ScriptedCapture::always_failing("seal failed").taking(clock.clone(), Duration::ZERO);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let mut lifecycle = FakeLifecycle::new(vec![]);

        // A 10s hold: the first failure arms a 15s backoff that the hold cannot
        // outlive.
        let outcome = run_hold(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            Duration::from_secs(10),
        );

        assert_eq!(cap.calls, 1, "no retry after the hold deadline");
        assert!(
            matches!(outcome, HoldTermination::AttemptEnded),
            "the hold ends on its own deadline, got {outcome:?}"
        );
    }

    /// The budget unit itself, so the two bounds can be read in one place.
    #[test]
    fn the_capture_budget_bounds_count_and_duration_independently() {
        let base = Instant::now();
        let window = Duration::from_secs(600);
        let backoff = Duration::from_secs(15);

        // (a) the count. Three admissions, then exhausted — with the clock
        // frozen, so only the count can be doing it.
        let mut budget = CaptureBudget::new(3, window, backoff);
        for i in 1..=3 {
            assert_eq!(budget.admit(base), CaptureAdmission::Go, "attempt {i}");
            // Failures are what arm the backoff, so clear it to isolate the count.
            budget.record_failure(base, format!("failure {i}"));
            budget.next_attempt_at = None;
        }
        assert_eq!(budget.attempts_spent(), 3);
        assert!(matches!(budget.admit(base), CaptureAdmission::Exhausted(_)));

        // (b) the duration, with attempts to spare.
        let mut budget = CaptureBudget::new(3, window, backoff);
        assert_eq!(budget.admit(base), CaptureAdmission::Go);
        budget.record_failure(base, "failure".to_string());
        assert!(
            budget
                .exhausted(base + window - Duration::from_secs(1))
                .is_none(),
            "inside the window"
        );
        assert!(
            budget.exhausted(base + window).is_some(),
            "the window closes ON its deadline, with one attempt still unspent"
        );

        // (c) the backoff refuses without spending anything.
        let mut budget = CaptureBudget::new(3, window, backoff);
        assert_eq!(budget.admit(base), CaptureAdmission::Go);
        budget.record_failure(base, "failure".to_string());
        assert_eq!(budget.admit(base), CaptureAdmission::BackingOff);
        assert_eq!(
            budget.attempts_spent(),
            1,
            "a refused directive costs no attempt"
        );
        assert_eq!(budget.admit(base + backoff), CaptureAdmission::Go);
    }

    /// A directive refused BEFORE any guest work costs no attempt.
    ///
    /// `pause_permitted == false` (ADR-007) and a missing candidate id (§3.6)
    /// both return to holding without touching the guest. Charging them would
    /// let a server that sends malformed directives burn the author's retries.
    #[test]
    fn a_directive_refused_before_the_guest_costs_no_attempt() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Ten refusals (no pause_permitted) ahead of the real capture: if any of
        // them were charged, the budget would be gone before the guest is
        // touched and the capture below would never run.
        let mut responses: Vec<ControlResponse> =
            (0..10).map(|_| capture(1, "cand_1", false)).collect();
        responses.push(capture(1, "cand_1", true));
        let mut control =
            ScriptedControl::advancing(clock.clone(), Duration::from_secs(2), responses);
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
        assert!(
            matches!(outcome, HoldTermination::Accepted { .. }),
            "got {outcome:?}"
        );
    }

    // ── (v-b) capture-epoch monotonicity (ADR-008): a stale/duplicate Capture ──
    //    after a source-available acceptance failure is ignored; a strictly
    //    newer epoch DOES drive a fresh capture.
    #[test]
    fn a_later_capture_directive_cannot_re_drive_a_hold_that_already_captured() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        // Epoch 1 captures. The duplicate epoch-1 and the newer epoch-2
        // directives behind it are scripted precisely so that consuming either
        // would show up as a second capture.
        let mut control = ScriptedControl::new(vec![
            capture(1, "cand_1", true),
            capture(1, "cand_1", true),
            capture(2, "cand_2", true),
        ]);
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

        // CHANGED, deliberately. This used to assert two captures across a
        // return-to-holding, exercising the ADR-008 epoch guard. That path is
        // gone: the loop ends on the first successful capture because the guest
        // must be released before acceptance can restore anything.
        //
        // The property that MATTERS survives and is what is asserted now — a
        // capture happens at most once per hold — but it is enforced by the loop
        // ending rather than by the epoch comparison. The guard itself is kept
        // and documented as unreachable, for the `VacatedHold` follow-up.
        assert_eq!(
            cap.calls, 1,
            "a hold captures at most once; later directives are never consumed"
        );
        assert_eq!(lifecycle.executes, 1, "acceptance ran once");
        assert!(
            matches!(outcome, HoldTermination::AttemptEnded),
            "the rejected candidate ends the attempt, got {outcome:?}"
        );
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

    // ── the slot rule: verify only after the guest is released ──────────────
    //
    // These are the regression tests for the defect where the hold loop drove
    // the disposable restore while its own guest still held the slot. They need
    // no hardware: `SingleSlotLifecycle` models the one rule the real backend
    // enforces and `FakeLifecycle` cannot express.

    /// A candidate is verified only after the held guest is released.
    ///
    /// FAILS on the pre-fix tree and passes after, which is the whole point.
    /// Before the fix `run()` called `restore_candidate` from inside the loop
    /// with the guest still live, so the restore errs `single-session backend
    /// busy`, the run rejects, §3.7 reports `Rejected`, and the loop continues
    /// to its TTL and returns `AttemptEnded` — both assertions below fail.
    #[test]
    fn a_candidate_is_verified_only_after_the_held_guest_is_released() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::new(vec![capture(1, "cand_1", true)]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        // The guest really is live while the hold loop runs.
        let guest_live = Rc::new(Cell::new(true));
        let mut lifecycle = SingleSlotLifecycle {
            inner: FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]),
            guest_live: Rc::clone(&guest_live),
        };

        let outcome = run_hold_then_verify(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
            Some(&guest_live),
        );

        assert!(
            matches!(outcome, HoldTermination::Accepted { .. }),
            "expected an accepted candidate, got {outcome:?}"
        );
        let acceptance = control.acceptance_reports()[0];
        assert_eq!(
            acceptance.status,
            AcceptanceStatus::Accepted,
            "the §3.7 verdict must say accepted, not merely the local outcome"
        );
        assert!(
            !guest_live.get(),
            "the guest must have been released before the restore"
        );
    }

    /// A restore attempted while the guest is live is refused — the failure the
    /// production tree exhibits today.
    ///
    /// Pins the harness itself. If `SingleSlotLifecycle` ever stopped modelling
    /// the slot rule, the test above would pass for the wrong reason and the
    /// regression would be invisible again.
    #[test]
    fn a_restore_while_the_guest_is_live_is_refused() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control = ScriptedControl::new(vec![capture(1, "cand_1", true)]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let guest_live = Rc::new(Cell::new(true));
        let mut lifecycle = SingleSlotLifecycle {
            inner: FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]),
            guest_live: Rc::clone(&guest_live),
        };

        // Deliberately do NOT release: pass `None` so the helper leaves the
        // guest live across the verify, reproducing the pre-fix ordering.
        let outcome = run_hold_then_verify(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
            None,
        );

        assert!(
            matches!(outcome, HoldTermination::AttemptEnded),
            "a refused restore rejects the candidate, got {outcome:?}"
        );
        let acceptance = control.acceptance_reports()[0];
        assert_eq!(
            acceptance.status,
            AcceptanceStatus::Rejected,
            "a restore that could not take the slot must reject, never accept"
        );
    }

    /// A successful capture ends the hold loop; no second capture is attempted.
    ///
    /// Pins the deliberate semantic change (a rejection no longer returns to
    /// holding) so a future reader sees it as a decision rather than assuming
    /// re-capture still works. Restoring re-capture is the `VacatedHold`
    /// follow-up.
    #[test]
    fn a_capture_ends_the_hold_loop_and_no_second_capture_is_attempted() {
        let clock = FakeClock::new();
        let cancel = AcceptanceCancellation::default();
        let mut control =
            ScriptedControl::new(vec![capture(1, "cand_1", true), capture(2, "cand_2", true)]);
        let mut cap = ScriptedCapture::ok("cand_1", false);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let guest_live = Rc::new(Cell::new(true));
        // Rejected by the author's own command, not by the slot.
        let mut lifecycle = SingleSlotLifecycle {
            inner: FakeLifecycle::new(vec![VerificationOutcome::Exited(1)]),
            guest_live: Rc::clone(&guest_live),
        };

        let outcome = run_hold_then_verify(
            &mut control,
            &mut cap,
            &mut elig,
            &mut extend,
            &mut lifecycle,
            &clock,
            &cancel,
            DEFAULT_HOLD_TTL,
            Some(&guest_live),
        );

        assert!(
            matches!(outcome, HoldTermination::AttemptEnded),
            "a rejected candidate ends the attempt, got {outcome:?}"
        );
        assert_eq!(
            cap.calls, 1,
            "the loop must not return to holding and capture again"
        );
        let acceptance = control.acceptance_reports()[0];
        assert_eq!(
            acceptance.status,
            AcceptanceStatus::Rejected,
            "the author still receives the per-candidate verdict on §3.7"
        );
    }

    // ── KVM: the PRODUCTION ordering, on real hardware ──────────────────────
    //
    // The KVM-free tests above prove the ORDERING. This one proves the ordering
    // holds when the guest is a real Firecracker VM, driven through the same
    // objects `main.rs` uses — `GuestCaptureAction` -> `HoldPhase` ->
    // `ReleasedHold` -> `verify_captured_candidate` -> `BackendDisposableLifecycle`.
    //
    // It deliberately does NOT call `backend.restore()` directly. A test that
    // did would re-prove what `fc_kvm_hold_candidate_restores_and_serves`
    // already proves (that the backend restores a candidate after a release) and
    // would say nothing about the ownership move that is the actual fix.
    //
    // Every collision-capable resource is taken from the environment so the
    // caller can make them run-unique; the harness script refuses to start if
    // any of them collides with a live service. Nothing here kills a process it
    // did not start or deletes a resource it did not create.

    /// The Execution Identity this E2E's candidate is sealed and verified under.
    ///
    /// One constant for both the seal and the acceptance side: they must agree,
    /// and two literals would let them drift into an identity mismatch that
    /// reads as an acceptance failure.
    #[cfg(test)]
    const E2E_CAPSULE_HASH: &str = "blake3:e2e-acceptance";

    #[cfg(test)]
    const E2E_EXECUTION_ID: &str =
        "blake3:e2e000000000000000000000000000000000000000000000000000000000acce";

    /// Env knob or skip: these tests must be inert on a machine without the
    /// isolated fixture wired up, including CI.
    #[cfg(test)]
    fn kvm_env(name: &str) -> Option<String> {
        match std::env::var(name) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => {
                eprintln!("SKIP: {name} not set");
                None
            }
        }
    }

    /// Is anything answering on `addr` right now?
    #[cfg(test)]
    fn addr_answers(addr: &str) -> bool {
        use std::net::TcpStream;
        let Ok(sock) = addr.parse() else { return false };
        TcpStream::connect_timeout(&sock, Duration::from_millis(400)).is_ok()
    }

    /// The full production path on a real guest, with the restored guest's
    /// identity PROVEN rather than assumed.
    ///
    /// The attribution argument, in order:
    ///
    /// 1. before release, the held guest answers — so the address is live;
    /// 2. after release, the held VMM pid is gone, the slot lock is gone, the
    ///    vsock UDS is gone, and the address REFUSES repeatedly — so nothing is
    ///    serving there;
    /// 3. after `verify_captured_candidate` restores, the address answers again
    ///    and echoes a fresh 128-bit request-scoped nonce.
    ///
    /// Step 2 is what makes step 3 attributable. A nonce baked into the guest
    /// could not do this: restore resumes identical memory, so the held guest
    /// and the restored guest would answer it the same way. Only "dead in the
    /// gap, alive after" distinguishes them.
    #[test]
    #[ignore]
    fn fc_kvm_production_hold_release_verify_attributes_the_restored_guest() {
        let Some(rootfs_path) = kvm_env("ATO_FC_TEST_ROOTFS") else {
            return;
        };
        let Some(guest_ip) = kvm_env("ATO_FC_GUEST_IP") else {
            return;
        };
        if !snapshot::FirecrackerBackend::kvm_present() {
            eprintln!("SKIP: /dev/kvm absent");
            return;
        }
        let rootfs = std::fs::read(&rootfs_path).expect("read ATO_FC_TEST_ROOTFS");
        let guest_addr = format!("{guest_ip}:8080");

        let backend = snapshot::FirecrackerBackend::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            capsulefs::CasStore::open(dir.path().join("cas")).expect("open the run-scoped CAS");

        // ── boot and hold, exactly as `process_interactive_capture_job` does ──
        let t_hold = Instant::now();
        let guest = backend
            .boot_and_hold(snapshot::BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: E2E_CAPSULE_HASH.to_string(),
                runner_class: None,
                surface_requirement: None,
                layers: snapshot::BuildLayers {
                    rootfs,
                    runtime: None,
                    dependency: None,
                    app: None,
                    vmstate: Vec::new(),
                    memory: Vec::new(),
                },
                restore_contract: snapshot::RestoreContract {
                    ports: vec![8080],
                    healthcheck: Some("/health".to_string()),
                    expected_ready_ms: Some(8000),
                    ..Default::default()
                },
                sanitizer_contract: snapshot::SanitizerContract::default(),
                declared_secret_markers: vec![],
                // Sealed under `exec_id()` — the SAME identity `OkEligibility`
                // proves. Acceptance refuses a candidate whose Execution
                // Identity differs from the verified eligibility proof
                // ("Snapshot candidate Execution Identity does not match the
                // verified eligibility proof"), so a bespoke constant here
                // rejected every run. Deriving both from one function makes the
                // agreement structural instead of a thing to remember.
                //
                // REQUIRED, not optional decoration: `GuestCaptureAction::capture`
                // refuses a sealed candidate whose manifest cannot name its
                // Execution Identity ("sealed candidate has no execution_id"),
                // because §3.6 has nowhere to put one it would have to invent.
                // Passing `None` here makes every capture fail with
                // `source_lost = false`, which returns the loop to holding and —
                // since the control script re-issues the same directive — spins
                // it through a fresh full snapshot per iteration until the
                // 30-minute TTL. Measured: 356 snapshots in one run.
                execution_id: Some(exec_id().as_str().to_string()),
                supervisor: None,
            })
            .expect("boot and hold");
        eprintln!("### E2E hold_ready_ms={}", t_hold.elapsed().as_millis());

        let vmm_pid = guest
            .vmm_pid()
            .expect("a held guest owns a firecracker pid");
        let lock_path = backend.slot_lock_path();
        assert!(
            addr_answers(&guest_addr),
            "the held guest must answer before anything else is asserted"
        );

        // ── capture through the production seams ─────────────────────────────
        let captured: crate::guest_capture::CapturedCandidateCell =
            Rc::new(std::cell::RefCell::new(None));
        let mut capture_action = crate::guest_capture::GuestCaptureAction::new(
            guest,
            crate::guest_capture::CaptureContext {
                job_id: "e2e".to_string(),
                jobdir: dir.path().to_path_buf(),
            },
            Rc::clone(&captured),
        );
        let mut control = ScriptedControl::new(vec![capture(1, "cand_e2e", true)]);
        let mut elig = OkEligibility;
        let mut extend = NoExtend;
        let clock = snapshot::acceptance::SystemClock;

        let t_capture = Instant::now();
        let outcome = {
            let mut phase = HoldPhase::new(
                &mut control,
                &mut capture_action,
                &mut elig,
                &mut extend,
                &clock,
                fencing(),
                DEFAULT_HOLD_TTL,
            );
            phase.run().expect("no fatal internal error")
        };
        eprintln!("### E2E capture_ms={}", t_capture.elapsed().as_millis());

        let HoldOutcome::CapturedPendingVerification(pending) = outcome else {
            panic!("expected a captured candidate, got {outcome:?}");
        };

        // ── release, then PROVE nothing is serving ───────────────────────────
        let t_release = Instant::now();
        let released = capture_action.release();
        eprintln!("### E2E release_ms={}", t_release.elapsed().as_millis());

        let mut pid_gone = false;
        for _ in 0..100 {
            if !std::path::Path::new(&format!("/proc/{vmm_pid}")).exists() {
                pid_gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            pid_gone,
            "### E2E FAIL held vmm pid {vmm_pid} still present"
        );
        eprintln!("### E2E held_pid_gone=true pid={vmm_pid}");

        assert!(
            !lock_path.exists(),
            "### E2E FAIL slot lock still held: {}",
            lock_path.display()
        );
        eprintln!("### E2E lock_released=true path={}", lock_path.display());

        // The vsock UDS is per-CAPSULE and deterministic, so a second VMM on
        // this slot would have unlinked the live hold's socket out from under
        // it. Assert the release took it down.
        //
        // MEASURED, and not what one would assume: `release()` does NOT unlink
        // the socket file. It kills and reaps the VMM, runs `net_down()` and
        // drops the slot lock — the socket is left on disk with nothing behind
        // it. So the invariant asserted here is the one that actually matters
        // for a following restore: nothing is LISTENING. A stale path with no
        // listener is inert, and the restore unlinks it before Firecracker
        // recreates it.
        //
        // The leaked file is still a leak — a hold that ends with no restore
        // after it leaves the path behind for good — but it belongs to the
        // resource-ownership follow-up, not to this ordering fix, and asserting
        // absence here would fail on correct behaviour.
        let vsock = snapshot::firecracker_vsock_uds_path_for_capsule(E2E_CAPSULE_HASH);
        let vsock_listening = std::os::unix::net::UnixStream::connect(&vsock).is_ok();
        assert!(
            !vsock_listening,
            "### E2E FAIL something is still listening on the hold's vsock UDS: {}",
            vsock.display()
        );
        eprintln!(
            "### E2E vsock_no_listener=true file_remains={} path={}",
            vsock.exists(),
            vsock.display()
        );

        // Repeatedly, not once: a single refusal could be a transient bind race.
        for attempt in 0..5 {
            assert!(
                !addr_answers(&guest_addr),
                "### E2E FAIL {guest_addr} still answered after release (probe {attempt})"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("### E2E pre_restore_connect_failed=true probes=5");

        // ── verify: the production entry point, gated on the token ───────────
        let mut lifecycle = snapshot::disposable_lifecycle::BackendDisposableLifecycle {
            backend: &backend,
            store: &store,
            candidate: crate::guest_capture::HeldCandidateSource::new(
                Rc::clone(&captured),
                &backend,
                exec_id(),
            ),
            overlay_root: dir.path().join("acceptance-overlay"),
            session: None,
            last_candidate: None,
        };
        let cancellation = snapshot::acceptance::AcceptanceCancellation::default();

        // A fresh 128-bit value per run. `seal_at` runs HOST-side, so this is the
        // command that reaches into the restored guest and demands it back.
        let nonce = kvm_env("ATO_E2E_NONCE").unwrap_or_else(|| "0".repeat(32));
        let probe = format!(
            "curl -fsS --max-time 20 'http://{guest_addr}/echo-nonce?value={nonce}' \
             | grep -Fxq '{nonce}'"
        );
        let acceptance_config = AcceptanceConfig {
            seal_at_argv: vec!["/bin/sh".to_string(), "-c".to_string(), probe],
            verification_timeout: Duration::from_secs(60),
            total_deadline: Duration::from_secs(600),
            maximum_attempts: 1,
        };

        let t_verify = Instant::now();
        let termination = verify_captured_candidate(
            &mut control,
            &mut lifecycle,
            &mut elig,
            &acceptance_config,
            &cancellation,
            &clock,
            &fencing(),
            pending,
            &released,
        )
        .expect("no fatal internal error");
        eprintln!("### E2E verify_ms={}", t_verify.elapsed().as_millis());

        // The seal_at command IS the nonce check, so an accepted candidate is
        // proof the restored guest echoed it. A readiness pass alone would not
        // be: the acceptance run reports `Accepted` only when the command exits 0.
        match &termination {
            HoldTermination::Accepted { .. } => {
                eprintln!("### E2E acceptance=accepted nonce_matched=true");
            }
            other => {
                // The §3.7 verdict carries WHY. Without it a rejection reads as
                // "acceptance failed" and every cause looks alike.
                let why = control
                    .acceptance_reports()
                    .first()
                    .and_then(|r| r.failure_reason.clone())
                    .unwrap_or_else(|| "<no failure_reason on the §3.7 verdict>".to_string());
                panic!("### E2E FAIL acceptance did not accept: {other:?} reason={why}");
            }
        }

        let acceptance = control.acceptance_reports();
        assert_eq!(acceptance.len(), 1, "### E2E FAIL missing §3.7 verdict");
        assert_eq!(
            acceptance[0].status,
            AcceptanceStatus::Accepted,
            "### E2E FAIL §3.7 verdict was not accepted"
        );
        eprintln!("### E2E ok=true");
    }
}
