/-!
# ato-api: a lease settled by Runner recovery never counts as a confirmed stop

Sources (ato-api): `src/routes/runner_lease_state.ts` (`POST /:id/recovery`)
and `src/services/run_stop.ts` (`runResourcesReleasedPredicate`, the
runner-lease arm for a failed Run).

The recovery endpoint writes the Run's `error_json` (`runner_recovery`) and the
lease's `status` and `stopped_at`, but never the lease's `error_json`.  The
predicate requires `run.error_json = lease.error_json`, so the Run keeps the
`run_stop_unconfirmed` blocker and its Instance cannot be deleted.

`recoverFixed` is the proposed API change; it is not implemented in this
repository.
-/
namespace RecoveredLeaseStopConfirmation

inductive Status where
  | claimed
  | failed
deriving DecidableEq, Repr

inductive Code where
  | runnerRecovery
  /-- Proposed: a recovery that could not confirm the stop. -/
  | runnerRecoveryUnconfirmed
  | stopAckTimeout
deriving DecidableEq, Repr

structure Rows where
  runStatus : Status
  runError : Option Code
  leaseStatus : Status
  leaseError : Option Code
  leaseLastStatusAt : Bool
  leaseStoppedAt : Bool
deriving DecidableEq, Repr

/-- Codes that are terminal words without Runner teardown evidence. -/
def withoutTeardownEvidence : Code → Bool
  | .stopAckTimeout => true
  | .runnerRecoveryUnconfirmed => true
  | .runnerRecovery => false

/-- `runResourcesReleasedPredicate`, failed arm. -/
def released (r : Rows) : Bool :=
  r.runStatus == .failed && r.leaseStatus == .failed && r.leaseLastStatusAt
    && r.runError == r.leaseError
    && (match r.runError with
        | some c => !withoutTeardownEvidence c
        | none => false)

/-- The API-only lease of the incident: claimed, never reported. -/
def claimedLease : Rows := ⟨.claimed, none, .claimed, none, true, false⟩

/-- `POST /:id/recovery` today. -/
def recoverBug (confirmed : Bool) (r : Rows) : Rows :=
  { r with runStatus := .failed, runError := some .runnerRecovery,
           leaseStatus := .failed, leaseStoppedAt := confirmed }

/-- BUG: the Runner confirmed the stop, `stopped_at` is recorded, and the Run
  is still not released. -/
theorem confirmed_recovery_is_not_a_confirmed_stop_BUG :
    (recoverBug true claimedLease).leaseStoppedAt = true
      ∧ released (recoverBug true claimedLease) = false := by decide

/-- Proposed: a confirmed recovery records the same error on the lease; an
  unconfirmed one uses a code the predicate excludes. -/
def recoverFixed (confirmed : Bool) (r : Rows) : Rows :=
  let code := if confirmed then Code.runnerRecovery else Code.runnerRecoveryUnconfirmed
  { r with runStatus := .failed, runError := some code,
           leaseStatus := .failed, leaseError := some code, leaseStoppedAt := confirmed }

theorem fixed_confirmed_recovery_releases_the_run :
    released (recoverFixed true claimedLease) = true := by decide

/-- An unconfirmed stop must keep the blocker. -/
theorem fixed_unconfirmed_recovery_keeps_the_blocker :
    released (recoverFixed false claimedLease) = false := by decide

end RecoveredLeaseStopConfirmation
