/-!
# ato-api: rows a recovery report wrote before the lease recorded its error

Companion to `RecoveredLeaseStopConfirmation.lean` (same definitions; the
fixed endpoint, `recoverFixed`, is implemented as modelled there). This file
covers what that model leaves out: the pairs the old endpoint already wrote,
and a later confirmed report after an unconfirmed one.

Sources (ato-api): `src/services/run_stop.ts`
(`recordConfirmedRecoveryEvidence`, `runResourcesReleasedPredicate`),
`src/routes/runner_lease_state.ts` (`POST /:id/recovery`),
`selfhost/recovered-runs.ts` (`repair-recovered-runs`).
-/
namespace RecoveredLeaseRepair

inductive Status where
  | claimed
  | failed
deriving DecidableEq, Repr

inductive Code where
  | runnerRecovery
  | runnerRecoveryUnconfirmed
  | stopAckTimeout
  | realizationFailed
deriving DecidableEq, Repr

structure Rows where
  runStatus : Status
  runError : Option Code
  leaseStatus : Status
  leaseError : Option Code
  leaseLastStatusAt : Bool
  /-- Written only from a Runner's confirmed teardown. -/
  leaseStoppedAt : Bool
deriving DecidableEq, Repr

def withoutTeardownEvidence : Code → Bool
  | .stopAckTimeout => true
  | .runnerRecoveryUnconfirmed => true
  | _ => false

/-- `runResourcesReleasedPredicate`, failed arm. -/
def released (r : Rows) : Bool :=
  r.runStatus == .failed && r.leaseStatus == .failed && r.leaseLastStatusAt
    && r.runError == r.leaseError
    && (match r.runError with
        | some c => !withoutTeardownEvidence c
        | none => false)

def claimedLease : Rows := ⟨.claimed, none, .claimed, none, true, false⟩

/-- The old endpoint: the Run gets `runner_recovery` either way, the lease no error. -/
def recoverLegacy (confirmed : Bool) (r : Rows) : Rows :=
  { r with runStatus := .failed, runError := some .runnerRecovery,
           leaseStatus := .failed, leaseStoppedAt := confirmed }

/-- The fixed endpoint on a live lease. -/
def recoverFixed (confirmed : Bool) (r : Rows) : Rows :=
  let code := if confirmed then Code.runnerRecovery else Code.runnerRecoveryUnconfirmed
  { r with runStatus := .failed, runError := some code,
           leaseStatus := .failed, leaseError := some code, leaseStoppedAt := confirmed }

/-- BUG: the affected rows. The stop was confirmed and is not recognised, and
  no later report can change them: the lease is terminal. -/
theorem legacy_confirmed_pair_is_stuck_BUG :
    (recoverLegacy true claimedLease).leaseStoppedAt = true
      ∧ released (recoverLegacy true claimedLease) = false := by decide

def endedByRecovery : Option Code → Bool
  | some .runnerRecovery => true
  | some .runnerRecoveryUnconfirmed => true
  | _ => false

def leaseOwed : Option Code → Bool
  | none => true
  | some .runnerRecoveryUnconfirmed => true
  | _ => false

/-- `recordConfirmedRecoveryEvidence`: only a failed pair whose lease carries
  `stopped_at`, whose Run was ended by recovery and whose lease has no error
  or the unconfirmed one. Everything else is returned unchanged. -/
def repair (r : Rows) : Rows :=
  if r.runStatus == .failed && r.leaseStatus == .failed && r.leaseStoppedAt
      && endedByRecovery r.runError && leaseOwed r.leaseError then
    { r with runError := some .runnerRecovery, leaseError := some .runnerRecovery }
  else r

/-- A report that confirms the stop of an already terminal lease:
  `stopped_at` is recorded, then the evidence is completed. -/
def confirmLater (r : Rows) : Rows := repair { r with leaseStoppedAt := true }

/-- FIXED: the affected rows become a confirmed stop. -/
theorem repair_releases_a_confirmed_legacy_pair :
    released (repair (recoverLegacy true claimedLease)) = true := by decide

/-- A legacy pair whose stop was never confirmed stays a blocker. -/
theorem repair_keeps_an_unconfirmed_legacy_pair :
    repair (recoverLegacy false claimedLease) = recoverLegacy false claimedLease
      ∧ released (repair (recoverLegacy false claimedLease)) = false := by decide

/-- An unconfirmed recovery under the fixed endpoint is not released by the
  repair, and is released once the Runner confirms. -/
theorem unconfirmed_then_confirmed :
    released (repair (recoverFixed false claimedLease)) = false
      ∧ released (confirmLater (recoverFixed false claimedLease)) = true := by decide

/-- The repair never turns evidence it does not own into a stop: without
  `stopped_at` nothing changes, whatever the rows are. -/
theorem repair_requires_confirmed_teardown (r : Rows) (h : r.leaseStoppedAt = false) :
    repair r = r := by
  simp [repair, h]

/-- A pair that failed for its own reason is never rewritten. -/
theorem repair_keeps_other_failures (r : Rows)
    (h : endedByRecovery r.runError = false) : repair r = r := by
  simp [repair, h]

theorem repair_is_idempotent (r : Rows) : repair (repair r) = repair r := by
  unfold repair
  split <;> simp_all [endedByRecovery, leaseOwed]

/-- A timeout pair with a later `stopped_at` is outside this repair. -/
theorem repair_leaves_a_timeout_pair :
    let timeout : Rows := ⟨.failed, some .stopAckTimeout, .failed, some .stopAckTimeout, true, true⟩
    repair timeout = timeout ∧ released timeout = false := by decide

end RecoveredLeaseRepair
