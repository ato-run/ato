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
//! any non-zero exit, signal, or timeout rejects, and every non-accepted outcome
//! that reached command execution MUST terminate the full verification process
//! tree before the Session is destroyed (RFC §6.3 / §8.4).
//!
//! This module is **Gate-0 style**: pure, deterministic orchestration with two
//! injectable seams — the IO seam ([`DisposableAcceptanceLifecycle`]) and a
//! monotonic [`MonotonicClock`] so deadline behavior is deterministic under test
//! (no wall-clock sleeps). It performs no live builder / runner / firecracker
//! work — that wiring is PR-2.
//!
//! **Immutability model.** The candidate is held by value as an immutable
//! [`SnapshotManifestV1`]; its content address ([`SnapshotManifestV1::snapshot_id`])
//! is derived once and is the address that is accepted. The disposable Session
//! is a *separate* [`DisposableSessionHandle`] that the lifecycle restores into,
//! runs the command in, and then destroys. Because every verification method
//! borrows the candidate immutably and mutates only the Session overlay, the
//! accepted bytes are the candidate bytes **unchanged**.
//!
//! **Fail-closed eligibility.** A `running` capture of a Capsule whose live
//! workload requires External State is ineligible (RFC §8.3). Eligibility is
//! carried as a proof-token ([`VerifiedRunningSnapshotEligibility`]) that only a
//! verified-Execution-Contract analysis (#1090) can mint — never a caller-supplied
//! bool — so a production caller structurally cannot drive an ineligible workload
//! into running capture.
//!
//! **Always-receipted.** [`RunningSnapshotAcceptance::accept`] returns an
//! [`AcceptanceRun`] on every terminal outcome — accept, reject, cancel, cleanup
//! failure, validation failure — each carrying an [`AcceptanceReceiptV1`] that
//! identifies the disposable-restore verifier. `Err` is reserved for a truly
//! unreceiptable internal fault.
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

use capsule::execution_contract::{
    ExecutionContractEnvelopeV1, ExecutionContractError, ExecutionId,
};
use capsule::snapshot_manifest::{
    AcceptanceStatus, CapturePolicyV1, SanitizationAttestationV1, SecretScanAttestationV1,
    SnapshotCatalogRecord, SnapshotId, SnapshotManifestV1,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema id of the acceptance receipt wire format.
pub const ACCEPTANCE_RECEIPT_V1_SCHEMA: &str = "ato.snapshot.acceptance-receipt/v1";
/// Stable identity of the disposable-restore verifier that produced a receipt.
pub const DISPOSABLE_RESTORE_VERIFIER_IDENTITY: &str = "ato.snapshot.disposable-restore-verifier";
/// Version of the verifier, pinned to this crate's version.
pub const DISPOSABLE_RESTORE_VERIFIER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// An injectable monotonic clock. The acceptance loop reads time **only** through
/// this seam so deadline truncation and expiry are deterministic under test
/// without real sleeps. Production uses [`SystemClock`].
pub trait MonotonicClock {
    fn now(&self) -> Instant;
}

/// The real monotonic clock: [`Instant::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

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

/// A **proof** that a `running` Snapshot capture is eligible — i.e. the Capsule's
/// live workload does **not** require External State (RFC §8.3).
///
/// This replaces the earlier caller-supplied `bool`: a bool is a *calling
/// convention* a PR-2 caller could get wrong (pass `false` and wrongly proceed),
/// exactly the failure class the `VerifiedExecutionId` fix closed. The proof has
/// **no** struct-literal constructor (its `execution_id` field is private), no
/// `From<bool>` / `new`, and is not `Deserialize`: the single sanctioned
/// production path is [`VerifiedRunningSnapshotEligibility::analyze_execution_contract`]
/// (#1090), which verifies the Execution Contract and fails closed when the live
/// workload requires External State. There is deliberately no constructor that
/// takes a raw [`ExecutionId`] alone, a bare bool alone, or an unverified
/// contract — so a caller structurally cannot drive an ineligible workload into
/// running capture.
///
/// The proof also **binds** the verified [`ExecutionId`] it was proven against
/// (its single, private field). `accept` reads that identity to seed the receipt
/// and to enforce, per candidate, that every captured candidate belongs to the
/// *same* Execution Identity the proof was obtained for — so a proof analyzed
/// against Execution Contract A can never accept a candidate of Identity B, and a
/// candidate cannot drift identity across retries.
///
/// ```compile_fail
/// use snapshot::acceptance::VerifiedRunningSnapshotEligibility;
/// // The proof has no public constructor and its `execution_id` field is
/// // private: a caller outside the module cannot mint eligibility with a struct
/// // literal (E0451), whatever value it tries to supply.
/// let _proof = VerifiedRunningSnapshotEligibility {
///     execution_id: unreachable!(),
/// };
/// ```
#[derive(Debug)]
pub struct VerifiedRunningSnapshotEligibility {
    /// The verified Execution Identity this proof was minted against. Private, so
    /// only a sanctioned constructor (test stand-ins now; #1090 later) can set it.
    execution_id: ExecutionId,
}

impl VerifiedRunningSnapshotEligibility {
    /// The verified Execution Identity this eligibility is bound to. Read by
    /// [`RunningSnapshotAcceptance::accept`] to seed the receipt and to reject any
    /// candidate whose own `execution_id` differs from it.
    fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    /// The sanctioned **production** constructor (#1090): mint a running-capture
    /// eligibility proof from a verified Execution Contract, in a **single,
    /// indivisible** step that both proves identity and analyzes the
    /// External-State requirement.
    ///
    /// This is deliberately the *only* production way to obtain a
    /// [`VerifiedRunningSnapshotEligibility`]. It takes a proof-carrying
    /// [`ExecutionContractEnvelopeV1`] — never a raw [`ExecutionId`], never a bare
    /// bool, never an *unverified* bare [`ExecutionContractV1`](capsule::execution_contract::ExecutionContractV1)
    /// — and in one call:
    ///
    /// 1. **Verifies the Execution Contract** — [`ExecutionContractEnvelopeV1::verified_execution_id`]
    ///    recomputes the canonical hash of the embedded contract and fails closed
    ///    (returning [`AcceptanceFailure::ExecutionContractVerificationFailed`]) if
    ///    it disagrees with the stored `execution_id`. The proof is *recomputed*
    ///    here, not trusted.
    /// 2. **Analyzes the restore-time-binding requirement** of that same verified
    ///    contract via [`crate::external_state::requires_restore_time_bindings_for_live_workload`]
    ///    — declared External State **or** declared restore-time secret bindings.
    /// 3. **Fails closed** with [`AcceptanceFailure::ExternalStateRequiresWorkloadIdle`]
    ///    when the live workload requires External State or restore-time secret
    ///    bindings — a `running` capture of such a Capsule is ineligible (RFC §8.3);
    ///    it must use `workload_idle` (#1093), never a secret-bearing running
    ///    fallback.
    /// 4. On success, **binds the proof's `execution_id` from the SAME verified
    ///    contract** — the id proven in step 1, not any caller-supplied value — so
    ///    the eligibility can only ever accept candidates of the exact identity it
    ///    was analyzed against.
    ///
    /// Because the id is bound from the *verified* contract and the analysis reads
    /// that *same* contract, there is no seam between "which contract was proven"
    /// and "which contract was analyzed": a caller cannot verify one contract and
    /// smuggle a different id or a different external-state shape.
    ///
    /// There is no bare-bool (or raw-id) constructor path — the only production
    /// constructor takes a verified envelope:
    ///
    /// ```compile_fail
    /// use snapshot::acceptance::VerifiedRunningSnapshotEligibility;
    /// // A bare bool is not a `&ExecutionContractEnvelopeV1`: this is a type error,
    /// // so an ineligible workload cannot be waved through with `false`.
    /// let _ = VerifiedRunningSnapshotEligibility::analyze_execution_contract(false);
    /// ```
    pub fn analyze_execution_contract(
        envelope: &ExecutionContractEnvelopeV1,
    ) -> Result<Self, AcceptanceFailure> {
        // (1) Verify the Execution Contract: recompute the canonical hash and
        // match it against the stored id, fail closed on any disagreement. This
        // yields a proof-carrying VerifiedExecutionId over the embedded contract.
        let verified = envelope
            .verified_execution_id()
            .map_err(AcceptanceFailure::ExecutionContractVerificationFailed)?;
        // (2)+(3) Analyze the SAME verified contract's restore-time-binding
        // requirement and fail CLOSED when a live workload requires External State
        // OR declared restore-time secret bindings.
        if crate::external_state::requires_restore_time_bindings_for_live_workload(
            &envelope.execution_contract,
        ) {
            return Err(AcceptanceFailure::ExternalStateRequiresWorkloadIdle);
        }
        // (4) Bind the proof id from the SAME verified contract.
        Ok(Self {
            execution_id: verified.as_execution_id().clone(),
        })
    }

    /// TEST-ONLY: mint a proof unconditionally for the given verified Execution
    /// Identity, standing in for #1090's analysis when exercising the accept path.
    ///
    /// Gated behind `cfg(test)` for this crate's own unit tests **and** the
    /// non-default `test-support` cargo feature so sibling crates (notably the
    /// `snapshot-builder` HoldPhase harness, PR-2) can seed `accept()` in their
    /// own KVM-free tests without a production eligibility constructor. The
    /// feature is never enabled by a normal `cargo build`, so this stays out of
    /// every shipped/library build (dev-dependency-only activation).
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(execution_id: ExecutionId) -> Self {
        Self { execution_id }
    }

    /// TEST-ONLY stand-in for the #1090 verified-Execution-Contract analysis that
    /// will be the *only* real constructor. Fails **closed** when the live
    /// workload requires External State or restore-time secret bindings (RFC §8.3);
    /// otherwise mints the proof bound to the verified Execution Identity.
    ///
    /// Same `cfg(any(test, feature = "test-support"))` gate as [`Self::for_test`]:
    /// available to this crate's tests and to sibling test harnesses that opt in
    /// to the `test-support` feature, never to a normal build.
    #[cfg(any(test, feature = "test-support"))]
    pub fn analyze_for_test(
        restore_time_bindings_required_by_live_workload: bool,
        execution_id: ExecutionId,
    ) -> Result<Self, AcceptanceFailure> {
        if restore_time_bindings_required_by_live_workload {
            return Err(AcceptanceFailure::ExternalStateRequiresWorkloadIdle);
        }
        Ok(Self { execution_id })
    }
}

/// Bounds for one acceptance run: the exact `seal_at` argv, the per-attempt
/// verification timeout, the overall build deadline, and the maximum attempt
/// count. Retries are bounded by **both** the deadline and this count (RFC §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceConfig {
    /// `seal_at.command` as exact argv — executed with no implicit shell and with
    /// argument boundaries preserved (RFC §6.1). `argv[0]` (the program) must be
    /// non-empty; later arguments MAY be empty strings; no argument may contain a
    /// NUL.
    pub seal_at_argv: Vec<String>,
    /// Per-attempt verification timeout. Truncated to the remaining deadline
    /// budget so the total run never exceeds `total_deadline`.
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
    /// number. A signal is **never** success. Always rejects.
    Signalled(i32),
    /// The child was lost or produced no decodable exit status. **Never** success.
    Lost,
    /// The command exceeded its (deadline-truncated) verification timeout.
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

/// The shared per-run budget threaded into every lifecycle phase: an **absolute**
/// deadline plus the cancellation token, read through the injected clock. A PR-2
/// impl uses it to honor cancellation and the remaining deadline inside its own
/// capture / create / restore / execute work rather than blocking unbounded.
pub struct AcceptanceBudget<'a> {
    absolute_deadline: Instant,
    cancellation: &'a AcceptanceCancellation,
    clock: &'a dyn MonotonicClock,
}

impl AcceptanceBudget<'_> {
    /// Whether the run has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// The absolute instant past which no work should proceed.
    pub fn deadline(&self) -> Instant {
        self.absolute_deadline
    }

    /// Time left before the deadline, saturating to zero once exceeded.
    pub fn remaining(&self) -> Duration {
        self.absolute_deadline
            .saturating_duration_since(self.clock.now())
    }

    /// Whether the deadline has been reached or passed.
    pub fn is_expired(&self) -> bool {
        self.clock.now() >= self.absolute_deadline
    }
}

/// The typed outcome of one acceptance attempt. A closed enum with pinned
/// kebab-case wire spellings: an unknown outcome string fails deserialization
/// rather than round-tripping as an opaque value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcceptanceAttemptOutcome {
    /// Capturing the candidate failed.
    CaptureFailed,
    /// The captured candidate failed its own manifest invariants.
    CandidateValidationFailed,
    /// The candidate's `capture_policy` is not `running`.
    UnsupportedCapturePolicy,
    /// The candidate's Execution Identity did not match the eligibility proof's
    /// verified Execution Identity: rejected before any create/restore/execute.
    ExecutionIdentityMismatch,
    /// Creating the disposable Session failed.
    CreateSessionFailed,
    /// Restoring the candidate into the Session failed.
    RestoreFailed,
    /// The command exited 0 within the deadline: accepted.
    Accepted,
    /// The command exited 0, but only after the deadline: not an accept.
    DeadlineExceeded,
    /// The command exited non-zero.
    NonzeroExit,
    /// The command was terminated by a signal.
    Signalled,
    /// The child was lost / produced no decodable exit status.
    Lost,
    /// The command exceeded its verification timeout.
    Timeout,
    /// The run was cancelled around this attempt.
    Cancelled,
    /// The verification seam itself errored.
    VerificationError,
}

/// The final disposition label recorded on the run receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcceptanceOutcome {
    Accepted,
    Rejected,
}

/// One attempt's receipt: which candidate was verified, the typed outcome, and
/// whether the (always-attempted) teardown actually ran. The id is a typed
/// [`SnapshotId`], so a malformed address fails closed at deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceAttemptReceiptV1 {
    pub attempt: u32,
    pub candidate_snapshot_id: Option<SnapshotId>,
    pub outcome: AcceptanceAttemptOutcome,
    pub process_tree_terminated: bool,
    pub disposable_session_destroyed: bool,
}

/// The receipt for a whole acceptance run, produced on **every** terminal
/// outcome. Identifies the disposable-restore verifier (identity + version) and
/// the subordinate `execution_id`, records the capture policy and per-attempt
/// history (so bounded recapture retries are auditable), the final disposition,
/// and — on acceptance — the accepted address plus the sanitization and
/// **redacted** secret-scan attestations carried by the accepted manifest.
///
/// The secret-scan attestation is exactly that: an *attestation that a scan ran*
/// with a redacted verdict. It is **never** a proof of absence of secrets (RFC
/// §8 / §17.3).
///
/// **Schema enforced at deserialize.** `schema` is the wire-version discriminator,
/// so it is checked *at deserialize* via a custom [`Deserialize`] that runs
/// [`AcceptanceReceiptV1::validate`] (below) — a generic
/// `serde_json::from_str::<AcceptanceReceiptV1>()` of a wrong/unknown schema (or an
/// otherwise-inconsistent receipt) is **rejected**, never silently read as v1. The
/// tolerant (no `deny_unknown_fields`) decode is preserved by routing through a
/// private [`AcceptanceReceiptWireV1`] twin; only the schema/consumer-boundary
/// invariants are added on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceReceiptV1 {
    /// Always [`ACCEPTANCE_RECEIPT_V1_SCHEMA`].
    pub schema: String,
    /// Identity of the verifier that ran the disposable restore + command.
    pub verifier_identity: String,
    /// Verifier version (this crate's version).
    pub verifier_version: String,
    /// The verified Execution Identity this run is bound to, seeded from the
    /// eligibility proof at the start of the run (not from the first captured
    /// candidate) so it cannot drift across retries. `None` only on a legacy /
    /// hand-built receipt. Typed, so a malformed id fails closed.
    pub execution_id: Option<ExecutionId>,
    pub capture_policy: CapturePolicyV1,
    pub maximum_attempts: u32,
    pub attempts: Vec<AcceptanceAttemptReceiptV1>,
    /// The run's final disposition.
    pub outcome: AcceptanceOutcome,
    /// The accepted content address, if any. Typed [`SnapshotId`].
    pub accepted_snapshot_id: Option<SnapshotId>,
    /// Structural-sanitization attestation of the accepted candidate. `None` when
    /// nothing was accepted.
    pub sanitization_attestation: Option<SanitizationAttestationV1>,
    /// Redacted secret-scan attestation of the accepted candidate. `None` when
    /// nothing was accepted. Attestation only — never proof of absence.
    pub secret_scan_attestation: Option<SecretScanAttestationV1>,
}

/// The private wire twin of [`AcceptanceReceiptV1`]: a tolerant (no
/// `deny_unknown_fields`, preserving the deliberate envelope-style tolerance) raw
/// decode. It exists **only** as the input to [`AcceptanceReceiptV1`]'s custom
/// [`Deserialize`], which runs the consumer-boundary [`AcceptanceReceiptV1::validate`]
/// (schema discriminator + verifier + capture policy + accept/reject shape) before
/// yielding a public receipt. A generic consumer therefore cannot obtain an
/// `AcceptanceReceiptV1` whose `schema` was never checked.
#[derive(Deserialize)]
struct AcceptanceReceiptWireV1 {
    schema: String,
    verifier_identity: String,
    verifier_version: String,
    execution_id: Option<ExecutionId>,
    capture_policy: CapturePolicyV1,
    maximum_attempts: u32,
    attempts: Vec<AcceptanceAttemptReceiptV1>,
    outcome: AcceptanceOutcome,
    accepted_snapshot_id: Option<SnapshotId>,
    sanitization_attestation: Option<SanitizationAttestationV1>,
    secret_scan_attestation: Option<SecretScanAttestationV1>,
}

impl<'de> Deserialize<'de> for AcceptanceReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AcceptanceReceiptWireV1::deserialize(deserializer)?;
        let receipt = Self {
            schema: wire.schema,
            verifier_identity: wire.verifier_identity,
            verifier_version: wire.verifier_version,
            execution_id: wire.execution_id,
            capture_policy: wire.capture_policy,
            maximum_attempts: wire.maximum_attempts,
            attempts: wire.attempts,
            outcome: wire.outcome,
            accepted_snapshot_id: wire.accepted_snapshot_id,
            sanitization_attestation: wire.sanitization_attestation,
            secret_scan_attestation: wire.secret_scan_attestation,
        };
        // Wire-version dispatch + consumer boundary: the schema discriminator (and
        // the rest of the integrity check) is enforced HERE, so a wrong/unknown
        // schema can never be read as v1 through the raw path.
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

/// Why a deserialized [`AcceptanceReceiptV1`] failed its consumer-boundary
/// integrity check. Every variant means the receipt is untrustworthy and must be
/// rejected rather than acted on.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AcceptanceReceiptValidationError {
    /// `schema` is not [`ACCEPTANCE_RECEIPT_V1_SCHEMA`].
    #[error("acceptance receipt schema is not the supported v1 schema")]
    UnsupportedSchema,
    /// `verifier_identity` is not a supported verifier identity.
    #[error("acceptance receipt was produced by an unsupported / untrusted verifier")]
    UntrustedVerifier,
    /// `capture_policy` is not `running` (this acceptance path is running-only).
    #[error("acceptance receipt capture policy is not `running`")]
    UnsupportedCapturePolicy,
    /// The attempt numbers are not the contiguous sequence `1..=attempts.len()`.
    #[error("acceptance receipt attempt numbers are not monotonic from 1")]
    NonMonotonicAttempts,
    /// More attempts are recorded than `maximum_attempts` permits.
    #[error("acceptance receipt records more attempts than maximum_attempts")]
    TooManyAttempts,
    /// An accepted receipt is missing a field the accept path always fills.
    #[error("accepted receipt is missing required field: {0}")]
    AcceptedMissingField(&'static str),
    /// An accepted receipt's final attempt is not itself `Accepted`.
    #[error("accepted receipt's final attempt outcome is not `accepted`")]
    AcceptedFinalAttemptNotAccepted,
    /// `accepted_snapshot_id` does not equal the final attempt's candidate id.
    #[error("accepted_snapshot_id does not match the final attempt's candidate snapshot id")]
    AcceptedSnapshotIdMismatch,
    /// A rejected receipt carries an accepted-only field.
    #[error("rejected receipt carries an accepted-only field: {0}")]
    RejectedCarriesAcceptedField(&'static str),
}

impl AcceptanceReceiptV1 {
    /// The **mandatory consumer boundary** for a deserialized receipt. Typed
    /// fields already reject unknown outcome strings and malformed ids at
    /// deserialize, but `schema` / `verifier_identity` / `verifier_version` are
    /// free `String`s and the cross-field invariants (accepted ⇒ snapshot id +
    /// attestations + final-attempt agreement; rejected ⇒ none of those; attempts
    /// monotonic and bounded) are not expressible in the type. This is run
    /// automatically by the custom [`Deserialize`] (so a raw
    /// `serde_json::from_str` cannot bypass it), and remains callable directly; a
    /// receipt with an attacker-chosen schema, an untrusted verifier, or an
    /// inconsistent accept/reject shape fails closed here.
    pub fn validate(&self) -> Result<(), AcceptanceReceiptValidationError> {
        use AcceptanceReceiptValidationError as E;

        if self.schema != ACCEPTANCE_RECEIPT_V1_SCHEMA {
            return Err(E::UnsupportedSchema);
        }
        if self.verifier_identity != DISPOSABLE_RESTORE_VERIFIER_IDENTITY {
            return Err(E::UntrustedVerifier);
        }
        if self.capture_policy != CapturePolicyV1::Running {
            return Err(E::UnsupportedCapturePolicy);
        }

        // Attempt numbers are the contiguous sequence 1..=len, and never exceed
        // the declared bound.
        if self.attempts.len() as u64 > u64::from(self.maximum_attempts) {
            return Err(E::TooManyAttempts);
        }
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.attempt != index as u32 + 1 {
                return Err(E::NonMonotonicAttempts);
            }
        }

        match self.outcome {
            AcceptanceOutcome::Accepted => {
                if self.execution_id.is_none() {
                    return Err(E::AcceptedMissingField("execution_id"));
                }
                let accepted_id = self
                    .accepted_snapshot_id
                    .as_ref()
                    .ok_or(E::AcceptedMissingField("accepted_snapshot_id"))?;
                if self.sanitization_attestation.is_none() {
                    return Err(E::AcceptedMissingField("sanitization_attestation"));
                }
                if self.secret_scan_attestation.is_none() {
                    return Err(E::AcceptedMissingField("secret_scan_attestation"));
                }
                let final_attempt = self
                    .attempts
                    .last()
                    .ok_or(E::AcceptedMissingField("attempts"))?;
                if final_attempt.outcome != AcceptanceAttemptOutcome::Accepted {
                    return Err(E::AcceptedFinalAttemptNotAccepted);
                }
                match &final_attempt.candidate_snapshot_id {
                    Some(candidate_id) if candidate_id == accepted_id => {}
                    _ => return Err(E::AcceptedSnapshotIdMismatch),
                }
            }
            AcceptanceOutcome::Rejected => {
                if self.accepted_snapshot_id.is_some() {
                    return Err(E::RejectedCarriesAcceptedField("accepted_snapshot_id"));
                }
                if self.sanitization_attestation.is_some() {
                    return Err(E::RejectedCarriesAcceptedField("sanitization_attestation"));
                }
                if self.secret_scan_attestation.is_some() {
                    return Err(E::RejectedCarriesAcceptedField("secret_scan_attestation"));
                }
            }
        }

        Ok(())
    }
}

/// A fail-closed rejection reason. Every variant refuses to accept: a candidate
/// that cannot be proven acceptable never yields an accepted
/// [`SnapshotCatalogRecord`]. Unlike a hard error, a rejection is still receipted
/// (it rides in [`AcceptanceDisposition::Rejected`] alongside a receipt).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AcceptanceFailure {
    /// The live workload requires External State, so a `running` capture is
    /// ineligible. Raised by the #1090 eligibility analysis (RFC §8.3).
    #[error("running Snapshot is ineligible because the live workload requires External State")]
    ExternalStateRequiresWorkloadIdle,
    /// The Execution Contract failed verification: its stored `execution_id` did
    /// not match the canonical hash of the embedded contract, so no eligibility
    /// proof can be minted from it (fail closed — #1090). Carries the underlying
    /// [`ExecutionContractError`] as its `#[source]` so diagnostics reflect the real
    /// cause (invalid schema / non-canonical / id mismatch) while the public message
    /// stays general; fail-closed semantics are unchanged.
    #[error(
        "Execution Contract verification failed: stored execution_id is not the canonical hash"
    )]
    ExecutionContractVerificationFailed(#[source] ExecutionContractError),
    /// The candidate's own `capture_policy` is not `running`.
    #[error("unsupported capture policy: this acceptance path accepts `running` candidates only")]
    UnsupportedCapturePolicy,
    /// A captured candidate's Execution Identity did not match the eligibility
    /// proof's verified Execution Identity. Fail closed before any
    /// create/restore/execute so a proof for Identity A can never accept a
    /// candidate of Identity B (RFC §8.3).
    #[error("Snapshot candidate Execution Identity does not match the verified eligibility proof")]
    ExecutionIdentityMismatch,
    /// The acceptance configuration is malformed.
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
    /// No candidate was accepted within the configured attempts and deadline.
    #[error("Snapshot candidate was not accepted within the configured attempts and deadline")]
    Exhausted,
}

/// The disposition of an acceptance run: either an accepted catalog record or a
/// fail-closed rejection. Both arms are accompanied by a receipt in
/// [`AcceptanceRun`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptanceDisposition {
    Accepted(SnapshotCatalogRecord),
    Rejected(AcceptanceFailure),
}

/// The always-receipted result of an acceptance run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceRun {
    pub disposition: AcceptanceDisposition,
    pub receipt: AcceptanceReceiptV1,
}

impl AcceptanceRun {
    pub fn is_accepted(&self) -> bool {
        matches!(self.disposition, AcceptanceDisposition::Accepted(_))
    }

    /// The accepted catalog record, if the run accepted.
    pub fn accepted_record(&self) -> Option<&SnapshotCatalogRecord> {
        match &self.disposition {
            AcceptanceDisposition::Accepted(record) => Some(record),
            AcceptanceDisposition::Rejected(_) => None,
        }
    }

    /// The rejection reason, if the run rejected.
    pub fn failure(&self) -> Option<&AcceptanceFailure> {
        match &self.disposition {
            AcceptanceDisposition::Rejected(failure) => Some(failure),
            AcceptanceDisposition::Accepted(_) => None,
        }
    }
}

/// A truly unreceiptable internal fault. The pure acceptance loop always produces
/// a receipt, so this is not returned by [`RunningSnapshotAcceptance::accept`] in
/// #1102; it exists so a PR-2 worker boundary can surface a genuinely
/// receipt-less crash without conflating it with a receipted rejection.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unreceiptable internal acceptance fault: {0}")]
pub struct FatalInternalError(pub &'static str);

/// The single injectable IO seam between the pure acceptance loop and the real
/// world. A production impl (PR-2) captures on a live builder, restores into a
/// firecracker microVM with **no** production secret / user state / Ato identity
/// attached (empty/synthetic ephemeral state only, RFC §8.4), runs the exact
/// argv in that guest, and tears the Session's process tree and overlay down.
/// Every phase receives the [`AcceptanceBudget`] so it can honor cancellation and
/// the remaining deadline. Tests supply a deterministic fake.
pub trait DisposableAcceptanceLifecycle {
    /// Capture an immutable candidate Snapshot for this attempt. A fresh capture
    /// per attempt is what makes recapture retries meaningful.
    fn capture_candidate(
        &mut self,
        attempt: u32,
        budget: &AcceptanceBudget,
    ) -> Result<CandidateSnapshot, String>;

    /// Create an isolated disposable Session with **no** production secrets, user
    /// state, or Ato user identity attached. Implementations MUST treat that
    /// boundary as part of their security contract (RFC §8.4).
    fn create_disposable_session(
        &mut self,
        candidate: &CandidateSnapshot,
        budget: &AcceptanceBudget,
    ) -> Result<DisposableSessionHandle, String>;

    /// Restore the immutable candidate into the disposable Session's overlay. The
    /// candidate is borrowed immutably: the restore writes only to the Session.
    fn restore_candidate(
        &mut self,
        session: &DisposableSessionHandle,
        candidate: &CandidateSnapshot,
        budget: &AcceptanceBudget,
    ) -> Result<(), String>;

    /// Execute `seal_at.command` as exact argv in the disposable Session, bounded
    /// by `timeout` (already truncated to the remaining deadline) and cooperatively
    /// cancellable via `budget`. No implicit shell; argument boundaries are
    /// preserved.
    ///
    /// The returned [`VerificationOutcome`] MUST faithfully classify the wait
    /// status: a process that exited maps to [`VerificationOutcome::Exited`] with
    /// its real code; a signal-terminated process (`WIFSIGNALED`) to
    /// [`VerificationOutcome::Signalled`]; a lost / undecodable child to
    /// [`VerificationOutcome::Lost`]; a timeout to [`VerificationOutcome::TimedOut`].
    /// Mapping any non-clean wait status to `Exited(0)` is a **contract violation**.
    fn execute_exact_argv(
        &mut self,
        session: &DisposableSessionHandle,
        argv: &[String],
        timeout: Duration,
        budget: &AcceptanceBudget,
    ) -> Result<VerificationOutcome, String>;

    /// Terminate the **full** verification process tree in the Session (RFC §8.4).
    /// Called before destroy on every non-accepted outcome that ran the command.
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
/// [`DisposableSessionGuard::teardown`]; on any other exit `Drop` still terminates
/// the process tree and destroys the Session (RFC §8.4).
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
    fn restore_candidate(
        &mut self,
        candidate: &CandidateSnapshot,
        budget: &AcceptanceBudget,
    ) -> Result<(), String> {
        let session = self
            .session
            .as_ref()
            .expect("disposable Session handle is live until teardown");
        self.lifecycle.restore_candidate(session, candidate, budget)
    }

    /// Run `seal_at.command` as exact argv in the still-live Session.
    fn execute_exact_argv(
        &mut self,
        argv: &[String],
        timeout: Duration,
        budget: &AcceptanceBudget,
    ) -> Result<VerificationOutcome, String> {
        let session = self
            .session
            .as_ref()
            .expect("disposable Session handle is live until teardown");
        self.lifecycle
            .execute_exact_argv(session, argv, timeout, budget)
    }

    /// Disarm the guard and run the normal-path teardown: terminate the process
    /// tree when required, then **always** destroy the Session. Records the outcome
    /// on `receipt` and returns whether either step failed (mapped by the caller to
    /// [`AcceptanceFailure::Cleanup`]).
    fn teardown(
        mut self,
        terminate_required: bool,
        receipt: &mut AcceptanceAttemptReceiptV1,
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
        // live disposable microVM + overlay is never leaked.
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
    /// Session, or fail closed — always returning a receipted [`AcceptanceRun`].
    ///
    /// Ordering of the fail-closed gates:
    /// 1. Holding `eligibility` is itself the External-State gate (RFC §8.3).
    /// 2. Configuration is validated.
    /// 3. Per attempt (bounded by `maximum_attempts` **and** the absolute deadline):
    ///    before and after each phase, honor cancellation and the deadline (no
    ///    create, restore, or execute is started once the deadline has passed);
    ///    capture an immutable candidate; reject any candidate whose own
    ///    `capture_policy` is not `running` or whose Execution Identity differs
    ///    from the eligibility proof's; create a disposable Session; restore the
    ///    candidate; run the exact argv with a timeout truncated to the remaining
    ///    budget;
    ///    accept on and only on exit `0` within the deadline; and **always** tear
    ///    the Session down, terminating its process tree on every non-accepted
    ///    outcome that reached command execution.
    pub fn accept(
        lifecycle: &mut impl DisposableAcceptanceLifecycle,
        eligibility: VerifiedRunningSnapshotEligibility,
        config: &AcceptanceConfig,
        cancellation: &AcceptanceCancellation,
        clock: &dyn MonotonicClock,
    ) -> Result<AcceptanceRun, FatalInternalError> {
        // Holding `eligibility` IS the fail-closed External-State gate: the proof
        // can only be minted by the (future #1090) verified-Execution-Contract
        // analysis, never by a caller-supplied bool, so a Capsule whose live
        // workload requires External State can never reach this path (RFC §8.3).
        // The proof also carries the *verified Execution Identity*: every candidate
        // must match it, and the receipt is bound to it up front.
        let verified_execution_id = eligibility.execution_id().clone();

        let mut receipt = new_receipt(config);
        // Bind the receipt to the eligibility proof's Execution Identity at the
        // START — not from the first captured candidate — so the accepted-record /
        // receipt identity cannot drift across retries.
        receipt.execution_id = Some(verified_execution_id.clone());

        if let Err(failure) = validate_config(config) {
            return Ok(reject(receipt, failure));
        }

        let started = clock.now();
        let absolute_deadline = started + config.total_deadline;
        let budget = AcceptanceBudget {
            absolute_deadline,
            cancellation,
            clock,
        };

        for attempt in 1..=config.maximum_attempts {
            // Before each attempt: cancellation, then deadline. No new attempt
            // starts after the deadline (RFC §8.2).
            if budget.is_cancelled() {
                return Ok(reject(receipt, AcceptanceFailure::Cancelled));
            }
            if budget.is_expired() {
                break;
            }

            let candidate = match lifecycle.capture_candidate(attempt, &budget) {
                Ok(candidate) => candidate,
                Err(_) => {
                    receipt.attempts.push(attempt_receipt(
                        attempt,
                        None,
                        AcceptanceAttemptOutcome::CaptureFailed,
                    ));
                    continue;
                }
            };
            if candidate.manifest.validate().is_err() {
                receipt.attempts.push(attempt_receipt(
                    attempt,
                    None,
                    AcceptanceAttemptOutcome::CandidateValidationFailed,
                ));
                return Ok(reject(
                    receipt,
                    AcceptanceFailure::Lifecycle {
                        phase: "candidate-validation",
                    },
                ));
            }
            // Capture-policy gate on the CANDIDATE MANIFEST ITSELF. The RFC's first
            // slice accepts `running` only; we never rely solely on a host
            // advertising `[Running]` — the candidate must declare `running`.
            if candidate.manifest.capture_policy != CapturePolicyV1::Running {
                receipt.attempts.push(attempt_receipt(
                    attempt,
                    None,
                    AcceptanceAttemptOutcome::UnsupportedCapturePolicy,
                ));
                return Ok(reject(receipt, AcceptanceFailure::UnsupportedCapturePolicy));
            }
            // The accepted address is derived from the candidate bytes ONCE, up
            // front. Verification borrows the candidate immutably and touches only
            // the disposable Session, so this address is exactly what is accepted.
            let snapshot_id = match candidate.manifest.snapshot_id() {
                Ok(id) => id,
                Err(_) => {
                    receipt.attempts.push(attempt_receipt(
                        attempt,
                        None,
                        AcceptanceAttemptOutcome::CandidateValidationFailed,
                    ));
                    return Ok(reject(
                        receipt,
                        AcceptanceFailure::Lifecycle {
                            phase: "candidate-validation",
                        },
                    ));
                }
            };

            // Bind EVERY candidate to the eligibility proof's verified Execution
            // Identity, BEFORE creating / restoring / executing anything. A proof
            // analyzed against Execution Contract A must never accept a candidate of
            // Identity B, and across retries the candidate identity must not drift
            // from the receipted one. Fail CLOSED: no create, no restore, no
            // execute — just a typed rejection receipt.
            if candidate.manifest.execution_id != verified_execution_id {
                receipt.attempts.push(attempt_receipt(
                    attempt,
                    Some(snapshot_id.clone()),
                    AcceptanceAttemptOutcome::ExecutionIdentityMismatch,
                ));
                return Ok(reject(
                    receipt,
                    AcceptanceFailure::ExecutionIdentityMismatch,
                ));
            }

            // After capture, before creating a Session: honor cancellation and the
            // deadline so no disposable Session is ever created past the budget.
            if budget.is_cancelled() {
                receipt.attempts.push(attempt_receipt(
                    attempt,
                    Some(snapshot_id.clone()),
                    AcceptanceAttemptOutcome::Cancelled,
                ));
                return Ok(reject(receipt, AcceptanceFailure::Cancelled));
            }
            if budget.is_expired() {
                receipt.attempts.push(attempt_receipt(
                    attempt,
                    Some(snapshot_id.clone()),
                    AcceptanceAttemptOutcome::DeadlineExceeded,
                ));
                break;
            }

            let mut attempt_rec = attempt_receipt(
                attempt,
                Some(snapshot_id.clone()),
                AcceptanceAttemptOutcome::RestoreFailed,
            );
            let session = match lifecycle.create_disposable_session(&candidate, &budget) {
                Ok(session) => session,
                Err(_) => {
                    attempt_rec.outcome = AcceptanceAttemptOutcome::CreateSessionFailed;
                    receipt.attempts.push(attempt_rec);
                    continue;
                }
            };

            // The Session is now live. Hold it in an RAII guard so that even if a
            // seam method or a downstream `.clone()` panics and unwinds the loop
            // before the explicit teardown below, the guard's `Drop` still
            // terminates the process tree and destroys the Session (RFC §8.4).
            let mut guard = DisposableSessionGuard::new(&mut *lifecycle, session);

            // Determine acceptance and whether the process tree must be terminated.
            // Per RFC §8.4, the tree is terminated on EVERY non-accepted outcome
            // that reached command execution; a clean accepted exit 0 needs no kill.
            let (accepted, terminate_required, outcome) = if budget.is_cancelled() {
                // Cancelled after Session creation, before restore: no process tree.
                (false, false, AcceptanceAttemptOutcome::Cancelled)
            } else if budget.is_expired() {
                // Deadline exceeded DURING create, before restore: do not start a
                // restore past the budget. Teardown still runs (destroy), but no
                // new phase begins.
                (false, false, AcceptanceAttemptOutcome::DeadlineExceeded)
            } else if guard.restore_candidate(&candidate, &budget).is_err() {
                // Restore failed: the command never ran, so no process tree exists.
                (false, false, AcceptanceAttemptOutcome::RestoreFailed)
            } else if budget.is_cancelled() {
                // Cancelled after restore, before execute: the command MUST NOT run.
                (false, false, AcceptanceAttemptOutcome::Cancelled)
            } else if budget.is_expired() {
                // Deadline exceeded DURING restore, before execute: do not start the
                // command past the budget (it would otherwise get a 0-length
                // timeout). Teardown still runs.
                (false, false, AcceptanceAttemptOutcome::DeadlineExceeded)
            } else {
                // Truncate the per-attempt timeout to the remaining deadline so the
                // total run never overshoots `total_deadline`.
                let exec_timeout = config.verification_timeout.min(budget.remaining());
                classify(
                    guard.execute_exact_argv(&config.seal_at_argv, exec_timeout, &budget),
                    &budget,
                )
            };

            attempt_rec.outcome = outcome;
            let cleanup_failed = guard.teardown(terminate_required, &mut attempt_rec);
            receipt.attempts.push(attempt_rec);

            if cleanup_failed {
                return Ok(reject(receipt, AcceptanceFailure::Cleanup));
            }
            if accepted {
                // Accepted bytes = candidate bytes, unchanged. The overlay the
                // command wrote to has been destroyed above; the record pins the
                // address derived from the candidate before any verification ran.
                receipt.accepted_snapshot_id = Some(snapshot_id.clone());
                receipt.sanitization_attestation =
                    Some(candidate.manifest.sanitization_attestation.clone());
                receipt.secret_scan_attestation =
                    Some(candidate.manifest.secret_scan_attestation.clone());
                receipt.outcome = AcceptanceOutcome::Accepted;
                return Ok(AcceptanceRun {
                    disposition: AcceptanceDisposition::Accepted(SnapshotCatalogRecord::new(
                        snapshot_id,
                        AcceptanceStatus::Accepted,
                    )),
                    receipt,
                });
            }
            if budget.is_cancelled() {
                return Ok(reject(receipt, AcceptanceFailure::Cancelled));
            }
        }

        Ok(reject(receipt, AcceptanceFailure::Exhausted))
    }
}

/// Map an `execute_exact_argv` result to `(accepted, terminate_required, outcome)`.
/// A clean exit 0 within the deadline is the sole accept and needs no tree kill;
/// every other outcome that reached execution rejects AND terminates the tree.
fn classify(
    result: Result<VerificationOutcome, String>,
    budget: &AcceptanceBudget,
) -> (bool, bool, AcceptanceAttemptOutcome) {
    match result {
        // A cancellation observed after a nominal exit still rejects and tears the
        // tree down — we never accept a race winner.
        Ok(VerificationOutcome::Exited(_)) if budget.is_cancelled() => {
            (false, true, AcceptanceAttemptOutcome::Cancelled)
        }
        // Exit 0 within the deadline is the SOLE success signal.
        Ok(VerificationOutcome::Exited(0)) if !budget.is_expired() => {
            (true, false, AcceptanceAttemptOutcome::Accepted)
        }
        // Exit 0 that only landed after the deadline is not an accept.
        Ok(VerificationOutcome::Exited(0)) => {
            (false, true, AcceptanceAttemptOutcome::DeadlineExceeded)
        }
        Ok(VerificationOutcome::Exited(_)) => (false, true, AcceptanceAttemptOutcome::NonzeroExit),
        // A signalled or lost process has no exit code, so it can never be
        // `Exited(0)`: structurally impossible to reach the accept arm.
        Ok(VerificationOutcome::Signalled(_)) => (false, true, AcceptanceAttemptOutcome::Signalled),
        Ok(VerificationOutcome::Lost) => (false, true, AcceptanceAttemptOutcome::Lost),
        Ok(VerificationOutcome::TimedOut) => (false, true, AcceptanceAttemptOutcome::Timeout),
        Ok(VerificationOutcome::Cancelled) => (false, true, AcceptanceAttemptOutcome::Cancelled),
        Err(_) => (false, true, AcceptanceAttemptOutcome::VerificationError),
    }
}

fn new_receipt(config: &AcceptanceConfig) -> AcceptanceReceiptV1 {
    AcceptanceReceiptV1 {
        schema: ACCEPTANCE_RECEIPT_V1_SCHEMA.to_string(),
        verifier_identity: DISPOSABLE_RESTORE_VERIFIER_IDENTITY.to_string(),
        verifier_version: DISPOSABLE_RESTORE_VERIFIER_VERSION.to_string(),
        execution_id: None,
        capture_policy: CapturePolicyV1::Running,
        maximum_attempts: config.maximum_attempts,
        attempts: Vec::new(),
        outcome: AcceptanceOutcome::Rejected,
        accepted_snapshot_id: None,
        sanitization_attestation: None,
        secret_scan_attestation: None,
    }
}

fn attempt_receipt(
    attempt: u32,
    candidate_snapshot_id: Option<SnapshotId>,
    outcome: AcceptanceAttemptOutcome,
) -> AcceptanceAttemptReceiptV1 {
    AcceptanceAttemptReceiptV1 {
        attempt,
        candidate_snapshot_id,
        outcome,
        process_tree_terminated: false,
        disposable_session_destroyed: false,
    }
}

fn reject(mut receipt: AcceptanceReceiptV1, failure: AcceptanceFailure) -> AcceptanceRun {
    receipt.outcome = AcceptanceOutcome::Rejected;
    AcceptanceRun {
        disposition: AcceptanceDisposition::Rejected(failure),
        receipt,
    }
}

fn validate_config(config: &AcceptanceConfig) -> Result<(), AcceptanceFailure> {
    if config.maximum_attempts == 0 {
        return Err(AcceptanceFailure::InvalidConfig(
            "maximum_attempts must be positive",
        ));
    }
    if config.verification_timeout.is_zero() {
        return Err(AcceptanceFailure::InvalidConfig(
            "verification_timeout must be positive",
        ));
    }
    if config.total_deadline < config.verification_timeout {
        return Err(AcceptanceFailure::InvalidConfig(
            "total_deadline must cover one verification timeout",
        ));
    }
    // Exact argv, no implicit shell (RFC §6.1). Reject ONLY an empty argv, an empty
    // `argv[0]` (the program), or a NUL byte in any argument (which no exec
    // boundary can carry). Empty-string arguments in positions >= 1 are VALID argv
    // (e.g. `["prog", "--value", ""]`) and are preserved unchanged — dropping or
    // rejecting them would silently change the authored command's meaning.
    if config.seal_at_argv.is_empty()
        || config.seal_at_argv[0].is_empty()
        || config.seal_at_argv.iter().any(|arg| arg.contains('\0'))
    {
        return Err(AcceptanceFailure::InvalidConfig(
            "seal_at argv must be non-empty with a non-empty program and no NUL bytes",
        ));
    }
    Ok(())
}

// ── Untrusted-child environment hygiene ─────────────────────────────────────

/// Environment namespace reserved for the trusted Snapshot acceptance broker.
///
/// Capsule-controlled install, build and probe processes must never inherit
/// values from this namespace. The broker locator is a capability hint rather
/// than key material, but hiding the whole namespace keeps the trust boundary
/// simple and stops a future credential from being exposed by accident.
pub const SNAPSHOT_ACCEPTANCE_CREDENTIAL_ENV_PREFIX: &str = "ATO_SNAPSHOT_ACCEPTANCE_";

/// Remove every Snapshot acceptance credential from an untrusted child.
///
/// Call immediately before spawning, as well as when constructing a shared
/// command: a caller may add ordinary environment overrides afterwards, but must
/// not be able to reintroduce a credential inherited from the Ato process.
///
/// Lives here, beside the acceptance protocol it protects, so every executor of
/// `seal_at.command` shares one definition — the CLI's build path and the
/// builder's hold path run the SAME verification command and must scrub the same
/// namespace (RFC §8.4: no production secret is connected).
pub fn sanitize_untrusted_environment(command: &mut std::process::Command) {
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .starts_with(SNAPSHOT_ACCEPTANCE_CREDENTIAL_ENV_PREFIX)
        {
            command.env_remove(name);
        }
    }
    // Remove the currently defined names even when they are absent from Ato's
    // own environment, so an explicitly added one is still dropped.
    command
        .env_remove("ATO_SNAPSHOT_ACCEPTANCE_MAC_KEY")
        .env_remove("ATO_SNAPSHOT_ACCEPTANCE_SIGNER_HELPER");
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

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

    /// A second, distinct verified Execution Identity (identity "B"), used to
    /// exercise the per-candidate identity-binding gate.
    fn exec_id_b() -> ExecutionId {
        ExecutionId::new(format!("blake3:{}", "b".repeat(64))).expect("valid execution id")
    }

    /// A controllable monotonic clock: `now()` is `base + elapsed`, where
    /// `elapsed` is advanced explicitly by tests (or by the fake lifecycle sharing
    /// a clone). No wall-clock sleeps.
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

    fn running_manifest() -> SnapshotManifestV1 {
        manifest_with_policy(CapturePolicyV1::Running)
    }

    /// A valid `running` manifest whose Execution Identity is `execution_id`.
    fn manifest_with_execution_id(execution_id: ExecutionId) -> SnapshotManifestV1 {
        SnapshotManifestV1 {
            execution_id,
            ..running_manifest()
        }
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
    /// a Session handle distinct from any candidate, records what the loop did, and
    /// can advance a shared [`FakeClock`] at capture / execute to exercise deadlines
    /// without sleeping.
    struct FakeLifecycle {
        manifest: SnapshotManifestV1,
        /// When non-empty, the candidate captured on attempt N is
        /// `manifests_per_attempt[N-1]` instead of `manifest` — lets a test drift
        /// the candidate's Execution Identity across retries.
        manifests_per_attempt: Vec<SnapshotManifestV1>,
        outcomes: Vec<VerificationOutcome>,
        captures: u32,
        creates: u32,
        restores: u32,
        terminates: u32,
        destroys: u32,
        executed_argv: Vec<Vec<String>>,
        recorded_timeouts: Vec<Duration>,
        destroyed_sessions: Vec<DisposableSessionHandle>,
        restore_fails: bool,
        terminate_fails: bool,
        destroy_fails: bool,
        clock: Option<FakeClock>,
        advance_on_capture: Duration,
        advance_on_create: Duration,
        advance_on_restore: Duration,
        advance_on_execute: Duration,
        cancel_on_execute: Option<AcceptanceCancellation>,
        cancel_on_restore: Option<AcceptanceCancellation>,
        panic_on_execute: bool,
    }

    impl FakeLifecycle {
        fn new(outcomes: Vec<VerificationOutcome>) -> Self {
            Self {
                manifest: running_manifest(),
                manifests_per_attempt: Vec::new(),
                outcomes,
                captures: 0,
                creates: 0,
                restores: 0,
                terminates: 0,
                destroys: 0,
                executed_argv: Vec::new(),
                recorded_timeouts: Vec::new(),
                destroyed_sessions: Vec::new(),
                restore_fails: false,
                terminate_fails: false,
                destroy_fails: false,
                clock: None,
                advance_on_capture: Duration::ZERO,
                advance_on_create: Duration::ZERO,
                advance_on_restore: Duration::ZERO,
                advance_on_execute: Duration::ZERO,
                cancel_on_execute: None,
                cancel_on_restore: None,
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
        fn capture_candidate(
            &mut self,
            attempt: u32,
            _budget: &AcceptanceBudget,
        ) -> Result<CandidateSnapshot, String> {
            self.captures += 1;
            if let Some(clock) = &self.clock {
                clock.advance(self.advance_on_capture);
            }
            let manifest = if self.manifests_per_attempt.is_empty() {
                self.manifest.clone()
            } else {
                self.manifests_per_attempt[(attempt - 1) as usize].clone()
            };
            Ok(CandidateSnapshot { manifest })
        }

        fn create_disposable_session(
            &mut self,
            _candidate: &CandidateSnapshot,
            _budget: &AcceptanceBudget,
        ) -> Result<DisposableSessionHandle, String> {
            self.creates += 1;
            if let Some(clock) = &self.clock {
                clock.advance(self.advance_on_create);
            }
            Ok(DisposableSessionHandle {
                opaque_id: format!("disposable-session-{}", self.captures),
            })
        }

        fn restore_candidate(
            &mut self,
            _session: &DisposableSessionHandle,
            _candidate: &CandidateSnapshot,
            _budget: &AcceptanceBudget,
        ) -> Result<(), String> {
            self.restores += 1;
            if let Some(clock) = &self.clock {
                clock.advance(self.advance_on_restore);
            }
            if let Some(cancellation) = &self.cancel_on_restore {
                cancellation.cancel();
            }
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
            timeout: Duration,
            _budget: &AcceptanceBudget,
        ) -> Result<VerificationOutcome, String> {
            self.executed_argv.push(argv.to_vec());
            self.recorded_timeouts.push(timeout);
            if let Some(clock) = &self.clock {
                clock.advance(self.advance_on_execute);
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

    fn proof() -> VerifiedRunningSnapshotEligibility {
        VerifiedRunningSnapshotEligibility::for_test(exec_id())
    }

    /// Run acceptance with a real system clock and no cancellation (the common case).
    fn run(lifecycle: &mut FakeLifecycle, config: &AcceptanceConfig) -> AcceptanceRun {
        RunningSnapshotAcceptance::accept(
            lifecycle,
            proof(),
            config,
            &AcceptanceCancellation::default(),
            &SystemClock,
        )
        .expect("no internal fault")
    }

    // --- AC: exit 0 is the ONLY success; exact argv preserved ---
    #[test]
    fn only_exit_zero_accepts_and_exact_argv_is_preserved() {
        // A non-zero exit is rejected; the next attempt's exit 0 accepts.
        let mut lifecycle = FakeLifecycle::new(vec![
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(0),
        ]);
        let run = run(&mut lifecycle, &config(2));

        assert_eq!(lifecycle.executed_argv, vec![config(2).seal_at_argv; 2]);
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::NonzeroExit
        );
        assert_eq!(
            run.receipt.attempts[1].outcome,
            AcceptanceAttemptOutcome::Accepted
        );
        assert!(run.is_accepted());
        assert!(run.accepted_record().unwrap().is_accepted());
    }

    // --- Major 1: exact argv preserves empty-string arguments ---
    #[test]
    fn empty_string_argument_is_preserved_through_to_the_lifecycle() {
        let cfg = AcceptanceConfig {
            // A valid argv whose second argument is deliberately the empty string.
            seal_at_argv: vec!["prog".to_string(), "--value".to_string(), String::new()],
            ..config(1)
        };
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let run = run(&mut lifecycle, &cfg);

        assert!(run.is_accepted());
        // The empty argument survived validation AND was passed through unchanged.
        assert_eq!(lifecycle.executed_argv, vec![cfg.seal_at_argv]);
    }

    // --- AC: a signal-terminated process is REJECTED, and its tree is terminated ---
    #[test]
    fn signalled_outcome_is_rejected_terminated_and_destroyed() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Signalled(9)]);
        let run = run(&mut lifecycle, &config(1));

        assert_eq!(run.failure(), Some(&AcceptanceFailure::Exhausted));
        assert_eq!(run.receipt.attempts.len(), 1);
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::Signalled
        );
        assert!(run.receipt.accepted_snapshot_id.is_none());
        // Blocker 1: a signalled outcome terminates the tree AND destroys.
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1);
        assert!(run.receipt.attempts[0].process_tree_terminated);
    }

    // --- AC: a lost / undecodable child is REJECTED, terminated, destroyed ---
    #[test]
    fn lost_outcome_is_rejected_terminated_and_destroyed() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Lost]);
        let run = run(&mut lifecycle, &config(1));

        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::Lost
        );
        assert!(run.receipt.accepted_snapshot_id.is_none());
        // Blocker 1: a lost outcome terminates the tree AND destroys.
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1);
    }

    // --- Blocker 1: a non-zero exit terminates the tree AND destroys ---
    #[test]
    fn nonzero_exit_terminates_and_destroys() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(2)]);
        let run = run(&mut lifecycle, &config(1));

        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::NonzeroExit
        );
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1);
    }

    // --- Blocker 1: a clean accepted exit 0 destroys WITHOUT terminating ---
    #[test]
    fn accepted_exit_zero_destroys_without_terminating() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let run = run(&mut lifecycle, &config(1));

        assert!(run.is_accepted());
        assert_eq!(lifecycle.destroys, 1);
        assert_eq!(lifecycle.terminates, 0, "a clean exit 0 needs no tree kill");
        assert!(!run.receipt.attempts[0].process_tree_terminated);
        assert!(run.receipt.attempts[0].disposable_session_destroyed);
    }

    // --- Blocker 1: a deadline-exceeded exit 0 terminates the tree AND destroys ---
    #[test]
    fn deadline_exceeded_exit_zero_terminates_and_destroys() {
        // Exit 0 that only lands after the deadline: reject, and (Blocker 1)
        // terminate the tree since the command reached execution.
        let clock = FakeClock::new();
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        lifecycle.clock = Some(clock.clone());
        // The command runs, then wall-clock (the fake clock) crosses the deadline.
        lifecycle.advance_on_execute = Duration::from_secs(20);

        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &config(1),
            &AcceptanceCancellation::default(),
            &clock,
        )
        .expect("no internal fault");

        assert!(!run.is_accepted());
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::DeadlineExceeded
        );
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1);
    }

    // --- AC: verification side effects do not alter accepted bytes ---
    #[test]
    fn accepted_bytes_are_the_unchanged_candidate_bytes() {
        let manifest = running_manifest();
        let expected_id = manifest.snapshot_id().expect("valid snapshot id");
        let mut lifecycle = FakeLifecycle::with_manifest(manifest.clone());
        lifecycle.outcomes = vec![VerificationOutcome::Exited(0)];

        let run = run(&mut lifecycle, &config(1));

        assert_eq!(run.accepted_record().unwrap().snapshot_id, expected_id);
        assert_eq!(
            run.receipt.accepted_snapshot_id.as_ref(),
            Some(&expected_id)
        );
        // The fake's source manifest is byte-identical after the run.
        assert_eq!(lifecycle.manifest, manifest);
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
        assert!(run(&mut ok, &config(1)).is_accepted());
        assert_eq!(ok.destroys, 1);

        // non-zero failure: Blocker 1 now terminates the tree too.
        let mut fail = FakeLifecycle::new(vec![VerificationOutcome::Exited(2)]);
        assert!(!run(&mut fail, &config(1)).is_accepted());
        assert_eq!(fail.destroys, 1);
        assert_eq!(fail.terminates, 1);

        // timeout: full process tree terminated AND session destroyed
        let mut timed = FakeLifecycle::new(vec![VerificationOutcome::TimedOut]);
        assert!(!run(&mut timed, &config(1)).is_accepted());
        assert_eq!(timed.terminates, 1);
        assert_eq!(timed.destroys, 1);

        // cancellation racing a nominal exit: tree terminated AND session destroyed
        let cancellation = AcceptanceCancellation::default();
        let mut cancelled = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        cancelled.cancel_on_execute = Some(cancellation.clone());
        let run = RunningSnapshotAcceptance::accept(
            &mut cancelled,
            proof(),
            &config(1),
            &cancellation,
            &SystemClock,
        )
        .expect("no internal fault");
        assert_eq!(run.failure(), Some(&AcceptanceFailure::Cancelled));
        assert_eq!(cancelled.terminates, 1);
        assert_eq!(cancelled.destroys, 1);
    }

    // --- AC: cleanup runs even when process-tree termination itself fails ---
    #[test]
    fn termination_failure_still_destroys_and_surfaces_cleanup() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::TimedOut]);
        lifecycle.terminate_fails = true;
        let run = run(&mut lifecycle, &config(1));

        assert_eq!(run.failure(), Some(&AcceptanceFailure::Cleanup));
        assert_eq!(lifecycle.terminates, 1);
        assert_eq!(lifecycle.destroys, 1); // destroy attempted despite terminate failure
        // Major 2: a cleanup failure is still receipted.
        assert_eq!(run.receipt.attempts.len(), 1);
        assert_eq!(
            run.receipt.verifier_identity,
            DISPOSABLE_RESTORE_VERIFIER_IDENTITY
        );
    }

    // --- AC: a panic mid-verification still tears the disposable Session down ---
    #[test]
    fn panic_during_execute_still_destroys_session() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let mut lifecycle = FakeLifecycle::new(Vec::new());
        lifecycle.panic_on_execute = true;

        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let unwound = catch_unwind(AssertUnwindSafe(|| run(&mut lifecycle, &config(1))));
        std::panic::set_hook(previous_hook);

        assert!(
            unwound.is_err(),
            "the execute panic must unwind out of accept"
        );
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
        let run = run(&mut by_count, &config(3));
        assert_eq!(run.failure(), Some(&AcceptanceFailure::Exhausted));
        assert_eq!(by_count.captures, 3);
        assert_eq!(run.receipt.attempts.len(), 3);
        assert_eq!(run.receipt.maximum_attempts, 3);
        for (i, attempt) in run.receipt.attempts.iter().enumerate() {
            assert_eq!(attempt.attempt, i as u32 + 1);
            assert_eq!(attempt.outcome, AcceptanceAttemptOutcome::NonzeroExit);
            assert!(attempt.candidate_snapshot_id.is_some());
        }

        // Bounded by deadline (no sleep): the fake advances the injected clock past
        // a tiny deadline during the first execute, so no second attempt starts.
        let clock = FakeClock::new();
        let mut by_deadline = FakeLifecycle::new(vec![VerificationOutcome::Exited(1)]);
        by_deadline.clock = Some(clock.clone());
        by_deadline.advance_on_execute = Duration::from_secs(5);
        let deadline_config = AcceptanceConfig {
            verification_timeout: Duration::from_secs(1),
            total_deadline: Duration::from_secs(1),
            maximum_attempts: 100,
            ..config(1)
        };
        RunningSnapshotAcceptance::accept(
            &mut by_deadline,
            proof(),
            &deadline_config,
            &AcceptanceCancellation::default(),
            &clock,
        )
        .expect("no internal fault");
        assert_eq!(
            by_deadline.captures, 1,
            "deadline must stop recapture after the first over-budget attempt"
        );
    }

    // --- Blocker 3: no new attempt starts after the deadline ---
    #[test]
    fn no_new_attempt_starts_after_the_deadline() {
        // The first attempt runs and its command overruns the deadline; the loop
        // must then break instead of starting a second attempt, even though the
        // attempt budget (3) is nowhere near exhausted.
        let clock = FakeClock::new();
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(1)]);
        lifecycle.clock = Some(clock.clone());
        lifecycle.advance_on_execute = Duration::from_secs(20);
        let cfg = AcceptanceConfig {
            verification_timeout: Duration::from_secs(1),
            total_deadline: Duration::from_secs(1),
            maximum_attempts: 3,
            ..config(3)
        };
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &cfg,
            &AcceptanceCancellation::default(),
            &clock,
        )
        .expect("no internal fault");

        assert_eq!(run.failure(), Some(&AcceptanceFailure::Exhausted));
        assert_eq!(
            lifecycle.captures, 1,
            "the second attempt must not start once the deadline has passed"
        );
    }

    // --- Blocker 3: the verification timeout is truncated to the remaining budget ---
    #[test]
    fn verification_timeout_is_truncated_to_remaining_deadline() {
        let clock = FakeClock::new();
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        lifecycle.clock = Some(clock.clone());
        // Consume 8s of a 10s deadline during capture, leaving 2s < the 5s timeout.
        lifecycle.advance_on_capture = Duration::from_secs(8);
        let cfg = AcceptanceConfig {
            verification_timeout: Duration::from_secs(5),
            total_deadline: Duration::from_secs(10),
            maximum_attempts: 1,
            ..config(1)
        };
        RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &cfg,
            &AcceptanceCancellation::default(),
            &clock,
        )
        .expect("no internal fault");

        // execute saw the remaining 2s, not the full 5s verification timeout.
        assert_eq!(lifecycle.recorded_timeouts, vec![Duration::from_secs(2)]);
    }

    // --- Blocker 3: if the deadline is exceeded after capture, no Session is created ---
    #[test]
    fn deadline_exceeded_after_capture_creates_no_session() {
        let clock = FakeClock::new();
        let mut lifecycle = FakeLifecycle::new(Vec::new());
        lifecycle.clock = Some(clock.clone());
        // Capture itself overruns the whole deadline.
        lifecycle.advance_on_capture = Duration::from_secs(20);
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &config(1),
            &AcceptanceCancellation::default(),
            &clock,
        )
        .expect("no internal fault");

        assert_eq!(lifecycle.captures, 1);
        assert_eq!(lifecycle.creates, 0, "no Session past the deadline");
        assert_eq!(lifecycle.destroys, 0);
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::DeadlineExceeded
        );
    }

    // --- Blocker 3: if cancelled after restore, the command is not executed ---
    #[test]
    fn cancelled_after_restore_does_not_execute_the_command() {
        let cancellation = AcceptanceCancellation::default();
        let mut lifecycle = FakeLifecycle::new(Vec::new());
        lifecycle.cancel_on_restore = Some(cancellation.clone());
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &config(1),
            &cancellation,
            &SystemClock,
        )
        .expect("no internal fault");

        assert_eq!(run.failure(), Some(&AcceptanceFailure::Cancelled));
        assert!(
            lifecycle.executed_argv.is_empty(),
            "the command must not run once cancelled after restore"
        );
        // The Session was created and restored, then torn down.
        assert_eq!(lifecycle.creates, 1);
        assert_eq!(lifecycle.restores, 1);
        assert_eq!(lifecycle.destroys, 1);
        assert_eq!(
            lifecycle.terminates, 0,
            "no process ran, so no tree to kill"
        );
    }

    // --- Blocker 2: acceptance requires a proof-carrying eligibility ---
    #[test]
    fn acceptance_requires_proof_carrying_eligibility() {
        // The proof mints (in tests) via the #1090 analysis stand-in; the accept
        // signature takes the proof by value — there is no bool to pass.
        let eligibility = VerifiedRunningSnapshotEligibility::analyze_for_test(false, exec_id())
            .expect("eligible");
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligibility,
            &config(1),
            &AcceptanceCancellation::default(),
            &SystemClock,
        )
        .expect("no internal fault");
        assert!(run.is_accepted());
    }

    // --- Blocker 2: the capsule-requires-external-state path fails CLOSED ---
    #[test]
    fn external_state_live_workload_fails_eligibility_closed() {
        // The #1090 analysis stand-in refuses to mint a proof when the live
        // workload requires External State, so acceptance can never proceed.
        let denied = VerifiedRunningSnapshotEligibility::analyze_for_test(true, exec_id());
        assert_eq!(
            denied.unwrap_err(),
            AcceptanceFailure::ExternalStateRequiresWorkloadIdle
        );
    }

    // --- #1090: the production constructor mints eligibility from a verified
    // contract and BINDS the proof id to that same verified contract ---
    #[test]
    fn production_constructor_binds_eligibility_to_the_verified_contract() {
        // A contract with NO External State AND NO restore-time secret bindings is
        // eligible for a running capture — both must be cleared (secret values are
        // External State; a secret-bearing running capture has no fallback).
        let contract = {
            let mut contract = crate::contract_fixtures::sample_execution_contract();
            contract.external_state.clear();
            contract.launch.secret_bindings.clear();
            contract
        };
        let bound_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let envelope = crate::contract_fixtures::envelope_for(contract);

        let eligibility = VerifiedRunningSnapshotEligibility::analyze_execution_contract(&envelope)
            .expect("external-state-free contract is eligible");

        // The proof is bound to the verified contract's id: a candidate carrying
        // exactly that id is accepted, and the receipt is bound to that id — proof
        // the constructor sourced the id from the SAME verified contract.
        let mut lifecycle =
            FakeLifecycle::with_manifest(manifest_with_execution_id(bound_id.clone()));
        lifecycle.outcomes = vec![VerificationOutcome::Exited(0)];
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            eligibility,
            &config(1),
            &AcceptanceCancellation::default(),
            &SystemClock,
        )
        .expect("no internal fault");
        assert!(run.is_accepted());
        assert_eq!(run.receipt.execution_id.as_ref(), Some(&bound_id));
    }

    // --- Blocker 1 (#1090): the production constructor's 4 eligibility quadrants
    // over the REAL analyze_execution_contract path — External State and/or
    // restore-time secret bindings both make a running capture ineligible ---
    #[test]
    fn production_constructor_eligibility_four_quadrants() {
        // Build an envelope from a mutated sample contract (re-hashing the stored
        // id so verification passes), then assert eligibility.
        let envelope_of = |external: bool, secret: bool| {
            let mut contract = crate::contract_fixtures::sample_execution_contract();
            if !external {
                contract.external_state.clear();
            }
            if !secret {
                contract.launch.secret_bindings.clear();
            }
            crate::contract_fixtures::envelope_for(contract)
        };
        let ineligible = |external: bool, secret: bool| {
            matches!(
                VerifiedRunningSnapshotEligibility::analyze_execution_contract(&envelope_of(
                    external, secret
                )),
                Err(AcceptanceFailure::ExternalStateRequiresWorkloadIdle)
            )
        };

        // external present & no secret -> reject.
        assert!(ineligible(true, false));
        // no external & secret present -> reject.
        assert!(ineligible(false, true));
        // both present -> reject.
        assert!(ineligible(true, true));
        // both empty -> eligible (proof minted, bound to the verified id).
        let eligible_contract = {
            let mut contract = crate::contract_fixtures::sample_execution_contract();
            contract.external_state.clear();
            contract.launch.secret_bindings.clear();
            contract
        };
        let bound_id = eligible_contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let proof = VerifiedRunningSnapshotEligibility::analyze_execution_contract(
            &crate::contract_fixtures::envelope_for(eligible_contract),
        )
        .expect("external-state-free, secret-free contract is eligible");
        assert_eq!(proof.execution_id(), &bound_id);
    }

    // --- Blocker 1 / #1090: the production constructor rejects an UNVERIFIED
    // contract (a tampered stored id) before analyzing or binding anything, and the
    // failure preserves the underlying ExecutionContractError as its source ---
    #[test]
    fn production_constructor_fails_closed_on_unverified_contract() {
        let contract = {
            let mut contract = crate::contract_fixtures::sample_execution_contract();
            contract.external_state.clear();
            contract.launch.secret_bindings.clear();
            contract
        };
        let mut envelope = crate::contract_fixtures::envelope_for(contract);
        // Tamper the stored id so it no longer equals the contract's canonical
        // hash: verification (recompute+match) must fail closed.
        envelope.execution_id =
            ExecutionId::new(format!("blake3:{}", "e".repeat(64))).expect("valid id shape");
        let error =
            VerifiedRunningSnapshotEligibility::analyze_execution_contract(&envelope).unwrap_err();
        // The public message stays general, but the real cause (an id mismatch) is
        // preserved as the error `#[source]`.
        let source = std::error::Error::source(&error).expect("verification failure has a source");
        assert!(
            source
                .downcast_ref::<ExecutionContractError>()
                .is_some_and(|inner| {
                    matches!(inner, ExecutionContractError::ExecutionIdMismatch { .. })
                }),
            "source must be the underlying ExecutionContractError id mismatch"
        );
        assert!(matches!(
            error,
            AcceptanceFailure::ExecutionContractVerificationFailed(_)
        ));
    }

    // --- AC: the secret scan is recorded as an ATTESTATION, not proof of absence ---
    #[test]
    fn receipt_records_secret_scan_as_attestation_not_proof() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let run = run(&mut lifecycle, &config(1));

        assert_eq!(run.receipt.capture_policy, CapturePolicyV1::Running);
        let sanitization = run
            .receipt
            .sanitization_attestation
            .as_ref()
            .expect("sanitization attestation recorded");
        assert_eq!(
            sanitization.steps,
            vec!["session_id_regenerate".to_string()]
        );

        let secret_scan = run
            .receipt
            .secret_scan_attestation
            .as_ref()
            .expect("secret-scan attestation recorded");
        assert_eq!(secret_scan.scanner_identity, "ato-secret-scan/1.0");
        assert_eq!(secret_scan.policy_identity, "default/v1");
        assert_eq!(secret_scan.verdict, "clean");

        let json = serde_json::to_string(&secret_scan).unwrap();
        assert!(!json.contains("proof"));
        assert!(!json.contains("secrets_absent"));
    }

    // --- Major 2: the receipt identifies the verifier and the execution id ---
    #[test]
    fn receipt_identifies_the_verifier_and_execution_id() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let run = run(&mut lifecycle, &config(1));

        assert_eq!(run.receipt.schema, ACCEPTANCE_RECEIPT_V1_SCHEMA);
        assert_eq!(
            run.receipt.verifier_identity,
            DISPOSABLE_RESTORE_VERIFIER_IDENTITY
        );
        assert_eq!(
            run.receipt.verifier_version,
            DISPOSABLE_RESTORE_VERIFIER_VERSION
        );
        assert_eq!(run.receipt.execution_id.as_ref(), Some(&exec_id()));
        assert_eq!(run.receipt.outcome, AcceptanceOutcome::Accepted);
    }

    // --- Major 2: every terminal outcome yields a receipt via the Rejected arm ---
    #[test]
    fn every_terminal_outcome_is_receipted() {
        // cancel (pre-cancelled)
        let cancellation = AcceptanceCancellation::default();
        cancellation.cancel();
        let mut cancelled = FakeLifecycle::new(Vec::new());
        let cancelled_run = RunningSnapshotAcceptance::accept(
            &mut cancelled,
            proof(),
            &config(1),
            &cancellation,
            &SystemClock,
        )
        .expect("no internal fault");
        assert_eq!(cancelled_run.failure(), Some(&AcceptanceFailure::Cancelled));
        assert_eq!(cancelled_run.receipt.schema, ACCEPTANCE_RECEIPT_V1_SCHEMA);
        assert_eq!(cancelled.captures, 0);

        // unsupported capture policy
        let mut wrong_policy =
            FakeLifecycle::with_manifest(manifest_with_policy(CapturePolicyV1::WorkloadIdle));
        let policy_run = run(&mut wrong_policy, &config(1));
        assert_eq!(
            policy_run.failure(),
            Some(&AcceptanceFailure::UnsupportedCapturePolicy)
        );
        assert_eq!(
            policy_run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::UnsupportedCapturePolicy
        );
        assert_eq!(wrong_policy.restores, 0);

        // invalid config
        let mut bad_cfg = FakeLifecycle::new(Vec::new());
        let cfg_run = run(
            &mut bad_cfg,
            &AcceptanceConfig {
                maximum_attempts: 0,
                ..config(1)
            },
        );
        assert!(matches!(
            cfg_run.failure(),
            Some(&AcceptanceFailure::InvalidConfig(_))
        ));
        assert!(cfg_run.receipt.attempts.is_empty());

        // cleanup failure
        let mut cleanup = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        cleanup.destroy_fails = true;
        let cleanup_run = run(&mut cleanup, &config(1));
        assert_eq!(cleanup_run.failure(), Some(&AcceptanceFailure::Cleanup));
        assert_eq!(cleanup_run.receipt.attempts.len(), 1);
    }

    // --- Major 2: typed receipt fields reject unknown / malformed values ---
    #[test]
    fn typed_receipt_fields_reject_unknown_and_malformed_values() {
        // A known-good receipt round-trips.
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let run = run(&mut lifecycle, &config(1));
        let json = serde_json::to_string(&run.receipt).unwrap();
        let parsed: AcceptanceReceiptV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, run.receipt);

        // An unknown attempt outcome is rejected at deserialize.
        let bad_outcome = json.replace("\"accepted\"", "\"totally-bogus\"");
        assert!(serde_json::from_str::<AcceptanceReceiptV1>(&bad_outcome).is_err());

        // A malformed snapshot id is rejected at deserialize (typed SnapshotId).
        let bad_id = json.replace("blake3:", "not-a-digest:");
        assert!(serde_json::from_str::<AcceptanceReceiptV1>(&bad_id).is_err());
    }

    // --- Major 3: the schema discriminator is enforced AT deserialize (wire-version
    // dispatch), so a wrong/unknown schema is never silently read as v1 ---
    #[test]
    fn receipt_wrong_schema_is_rejected_at_deserialize() {
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        let receipt = run(&mut lifecycle, &config(1)).receipt;
        let json = serde_json::to_string(&receipt).unwrap();
        // A valid receipt round-trips through the validated Deserialize boundary.
        assert!(serde_json::from_str::<AcceptanceReceiptV1>(&json).is_ok());

        // A wrong/unknown schema is REJECTED at deserialize — not read as v1.
        let wrong = json.replace(ACCEPTANCE_RECEIPT_V1_SCHEMA, "attacker.receipt/v999");
        assert!(serde_json::from_str::<AcceptanceReceiptV1>(&wrong).is_err());

        // An untrusted verifier is likewise rejected at deserialize, closing the
        // raw-path bypass of the consumer boundary.
        let untrusted = json.replace(DISPOSABLE_RESTORE_VERIFIER_IDENTITY, "attacker.verifier");
        assert!(serde_json::from_str::<AcceptanceReceiptV1>(&untrusted).is_err());
    }

    // --- Capture-policy gate: a non-`running` candidate is rejected explicitly ---
    #[test]
    fn non_running_candidate_manifest_is_rejected_with_unsupported_policy() {
        let mut lifecycle =
            FakeLifecycle::with_manifest(manifest_with_policy(CapturePolicyV1::WorkloadIdle));
        let run = run(&mut lifecycle, &config(1));

        assert_eq!(
            run.failure(),
            Some(&AcceptanceFailure::UnsupportedCapturePolicy)
        );
        assert_eq!(lifecycle.restores, 0);
        assert_eq!(lifecycle.destroys, 0);
    }

    // --- Config validation: empty/NUL argv, empty program, zero timeout, zero attempts ---
    #[test]
    fn invalid_configurations_fail_closed() {
        let mut lifecycle = FakeLifecycle::new(Vec::new());
        let bad = [
            // empty argv
            AcceptanceConfig {
                seal_at_argv: Vec::new(),
                ..config(1)
            },
            // empty argv[0] (the program)
            AcceptanceConfig {
                seal_at_argv: vec![String::new(), "arg".to_string()],
                ..config(1)
            },
            // NUL byte in an argument
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
            let run = run(&mut lifecycle, &cfg);
            assert!(matches!(
                run.failure(),
                Some(&AcceptanceFailure::InvalidConfig(_))
            ));
        }
        assert_eq!(lifecycle.captures, 0);
    }

    // --- Pre-cancellation creates no resources ---
    #[test]
    fn pre_cancelled_run_creates_no_resources() {
        let cancellation = AcceptanceCancellation::default();
        cancellation.cancel();
        let mut lifecycle = FakeLifecycle::new(Vec::new());
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &config(1),
            &cancellation,
            &SystemClock,
        )
        .expect("no internal fault");
        assert_eq!(run.failure(), Some(&AcceptanceFailure::Cancelled));
        assert_eq!(lifecycle.captures, 0);
    }

    // --- Blocker 1: a proof for Identity A rejects a candidate of Identity B ---
    #[test]
    fn candidate_execution_identity_mismatch_is_rejected_before_create() {
        // Proof bound to Identity A; the captured candidate carries Identity B.
        let mut lifecycle = FakeLifecycle::with_manifest(manifest_with_execution_id(exec_id_b()));
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            VerifiedRunningSnapshotEligibility::for_test(exec_id()),
            &config(1),
            &AcceptanceCancellation::default(),
            &SystemClock,
        )
        .expect("no internal fault");

        assert_eq!(
            run.failure(),
            Some(&AcceptanceFailure::ExecutionIdentityMismatch)
        );
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::ExecutionIdentityMismatch
        );
        // Fail CLOSED: nothing was created, restored, or executed.
        assert_eq!(lifecycle.creates, 0);
        assert_eq!(lifecycle.restores, 0);
        assert!(lifecycle.executed_argv.is_empty());
        assert!(run.receipt.accepted_snapshot_id.is_none());
        // The receipt is bound to the proof's identity (A), not the candidate's (B).
        assert_eq!(run.receipt.execution_id.as_ref(), Some(&exec_id()));
    }

    // --- Blocker 1: candidate identity must not drift across retries ---
    #[test]
    fn candidate_identity_drift_across_retries_rejects_and_never_executes_b() {
        // Attempt 1's candidate is Identity A (runs, exits non-zero → would retry);
        // attempt 2's candidate drifts to Identity B → identity-mismatch reject.
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(1)]);
        lifecycle.manifests_per_attempt = vec![
            manifest_with_execution_id(exec_id()),
            manifest_with_execution_id(exec_id_b()),
        ];
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            VerifiedRunningSnapshotEligibility::for_test(exec_id()),
            &config(2),
            &AcceptanceCancellation::default(),
            &SystemClock,
        )
        .expect("no internal fault");

        assert_eq!(
            run.failure(),
            Some(&AcceptanceFailure::ExecutionIdentityMismatch)
        );
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::NonzeroExit
        );
        assert_eq!(
            run.receipt.attempts[1].outcome,
            AcceptanceAttemptOutcome::ExecutionIdentityMismatch
        );
        // Only the Identity-A candidate ever executed; B never ran.
        assert_eq!(lifecycle.executed_argv.len(), 1);
        assert_eq!(run.receipt.execution_id.as_ref(), Some(&exec_id()));
    }

    // --- Blocker 1: a run whose every candidate matches the proof proceeds ---
    #[test]
    fn matching_identity_across_all_attempts_proceeds_normally() {
        // Both attempts carry Identity A (the proof's identity): the identity gate
        // never fires and the second attempt's exit 0 accepts.
        let mut lifecycle = FakeLifecycle::new(vec![
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(0),
        ]);
        lifecycle.manifests_per_attempt = vec![
            manifest_with_execution_id(exec_id()),
            manifest_with_execution_id(exec_id()),
        ];
        let run = run(&mut lifecycle, &config(2));

        assert!(run.is_accepted());
        assert_eq!(lifecycle.executed_argv.len(), 2);
        assert_eq!(run.receipt.execution_id.as_ref(), Some(&exec_id()));
    }

    // --- Blocker 2: deadline exceeded DURING create → no restore, no execute ---
    #[test]
    fn deadline_exceeded_during_create_skips_restore_and_execute() {
        let clock = FakeClock::new();
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        lifecycle.clock = Some(clock.clone());
        // Creating the Session itself overruns the whole deadline.
        lifecycle.advance_on_create = Duration::from_secs(20);
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &config(1),
            &AcceptanceCancellation::default(),
            &clock,
        )
        .expect("no internal fault");

        assert_eq!(lifecycle.creates, 1);
        assert_eq!(
            lifecycle.restores, 0,
            "no restore is started past the deadline"
        );
        assert!(
            lifecycle.executed_argv.is_empty(),
            "no command is started past the deadline"
        );
        // The created Session is still torn down even though the deadline passed.
        assert_eq!(lifecycle.destroys, 1);
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::DeadlineExceeded
        );
    }

    // --- Blocker 2: deadline exceeded DURING restore → no execute ---
    #[test]
    fn deadline_exceeded_during_restore_skips_execute() {
        let clock = FakeClock::new();
        let mut lifecycle = FakeLifecycle::new(vec![VerificationOutcome::Exited(0)]);
        lifecycle.clock = Some(clock.clone());
        // Restore runs, but overruns the whole deadline before execute can start.
        lifecycle.advance_on_restore = Duration::from_secs(20);
        let run = RunningSnapshotAcceptance::accept(
            &mut lifecycle,
            proof(),
            &config(1),
            &AcceptanceCancellation::default(),
            &clock,
        )
        .expect("no internal fault");

        assert_eq!(lifecycle.restores, 1);
        assert!(
            lifecycle.executed_argv.is_empty(),
            "no command is started once the deadline passed during restore"
        );
        assert_eq!(lifecycle.destroys, 1);
        assert_eq!(
            run.receipt.attempts[0].outcome,
            AcceptanceAttemptOutcome::DeadlineExceeded
        );
    }

    // --- Major: AcceptanceReceiptV1::validate() is the consumer boundary ---
    #[test]
    fn receipt_validate_enforces_the_consumer_boundary() {
        // A produced accepted receipt (with a preceding non-zero attempt) validates.
        let mut acc = FakeLifecycle::new(vec![
            VerificationOutcome::Exited(1),
            VerificationOutcome::Exited(0),
        ]);
        let accepted = run(&mut acc, &config(2)).receipt;
        accepted
            .validate()
            .expect("a produced accepted receipt validates");

        // A produced rejected receipt validates too.
        let mut rej = FakeLifecycle::new(vec![VerificationOutcome::Exited(2)]);
        let rejected = run(&mut rej, &config(1)).receipt;
        rejected
            .validate()
            .expect("a produced rejected receipt validates");

        // Negative vectors.
        let other_id = SnapshotId::new(format!("blake3:{}", "d".repeat(64))).unwrap();

        // wrong schema rejected
        let mut bad = accepted.clone();
        bad.schema = "attacker.receipt/v99".to_string();
        assert_eq!(
            bad.validate(),
            Err(AcceptanceReceiptValidationError::UnsupportedSchema)
        );

        // untrusted verifier rejected
        let mut bad = accepted.clone();
        bad.verifier_identity = "attacker.verifier".to_string();
        assert_eq!(
            bad.validate(),
            Err(AcceptanceReceiptValidationError::UntrustedVerifier)
        );

        // accepted-without-snapshot-id rejected
        let mut bad = accepted.clone();
        bad.accepted_snapshot_id = None;
        assert_eq!(
            bad.validate(),
            Err(AcceptanceReceiptValidationError::AcceptedMissingField(
                "accepted_snapshot_id"
            ))
        );

        // accepted_snapshot_id != final attempt candidate rejected
        let mut bad = accepted.clone();
        bad.accepted_snapshot_id = Some(other_id.clone());
        assert_eq!(
            bad.validate(),
            Err(AcceptanceReceiptValidationError::AcceptedSnapshotIdMismatch)
        );

        // rejected-with-snapshot-id rejected
        let mut bad = rejected.clone();
        bad.accepted_snapshot_id = Some(other_id);
        assert_eq!(
            bad.validate(),
            Err(
                AcceptanceReceiptValidationError::RejectedCarriesAcceptedField(
                    "accepted_snapshot_id"
                )
            )
        );

        // non-monotonic attempts rejected
        let mut bad = accepted.clone();
        bad.attempts[0].attempt = 7;
        assert_eq!(
            bad.validate(),
            Err(AcceptanceReceiptValidationError::NonMonotonicAttempts)
        );

        // over-max attempts rejected
        let mut bad = accepted.clone();
        bad.maximum_attempts = 1; // but two attempts were recorded
        assert_eq!(
            bad.validate(),
            Err(AcceptanceReceiptValidationError::TooManyAttempts)
        );
    }
}
