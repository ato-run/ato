/-!
# ato-api: a claim whose response was lost, and a poll whose client left

Sources (ato-api): `src/routes/runners.ts` (`GET /:id/leases/next`,
`attemptClaimNextLease`), `src/routes/runner_leases.ts` (`POST /:id/status`),
`src/services/runner_claim_reclaim.ts` (`reclaimUnreportedClaims`),
`selfhost/server.mjs` and `selfhost/app.ts` (`reconcileInstances`).
Runner (ato): `apps/connected-realization-worker/src/lib.rs`, `execute_lease`
reports `preparing` before any side effect and stops if it is refused.

`RunnerClaimDelivery.lean` models the Runner-side audit of the same incident.
This file models the control plane's own guarantee, which does not depend on
the Runner process still running.

One lease, one single-slot Runner, discrete sweeps of the Coordinator.
-/
namespace ControlPlaneClaimReclaim

inductive Lease where
  | pending
  /-- Claimed; `age` sweeps have passed without a report. -/
  | claimed (age : Nat)
  /-- The Runner's `preparing` report was accepted. -/
  | preparing
  /-- Released by the control plane: terminal. -/
  | failed
deriving DecidableEq, Repr

structure World where
  lease : Lease
  /-- A workload, lease directory or state writer exists on the Runner. -/
  workload : Bool
deriving DecidableEq, Repr

/-- The capacity gate of `/leases/next`: an open lease means `lease: null`. -/
def slotFree : Lease → Bool
  | .pending => true
  | .failed => true
  | _ => false

def iter (f : α → α) : Nat → α → α
  | 0, a => a
  | n + 1, a => iter f n (f a)

/-! ## The claim is committed before the response is written -/

/-- What the handler sees of its client when it is about to claim. -/
structure Poll where
  /-- The client's connection is still open. -/
  connected : Bool
  /-- The host tells the handler when the connection closes. -/
  observed : Bool

/-- Before: `selfhost/server.mjs` built the Request without an abort signal,
  so the handler claimed whatever happened to the client. -/
def claimBug (_ : Poll) (w : World) : World :=
  match w.lease with
  | .pending => { w with lease := .claimed 0 }
  | _ => w

/-- Fixed: the long-poll loop checks the signal before each claim attempt. -/
def claimFixed (p : Poll) (w : World) : World :=
  if p.observed && !p.connected then w else claimBug p w

def start : World := ⟨.pending, false⟩

/-- BUG (item 2): a poll from a Runner process that has exited still claims. -/
theorem dead_poll_claims_BUG :
    (claimBug ⟨false, false⟩ start).lease = .claimed 0 := by decide

/-- FIXED (item 2): a poll whose disconnect the host reported claims nothing. -/
theorem fixed_aborted_poll_claims_nothing (w : World) :
    claimFixed ⟨false, true⟩ w = w := by
  simp [claimFixed]

/-- A live poll claims exactly as before. -/
theorem fixed_live_poll_claims :
    (claimFixed ⟨true, true⟩ start).lease = .claimed 0 := by decide

/-- What the signal cannot cover: the client leaves after the check, or the
  host never reports it (Workers). The claim is committed and its response is
  lost, so the guarantee has to come from the sweep below. -/
theorem fixed_poll_window_remains :
    (claimFixed ⟨true, true⟩ start).lease = .claimed 0
      ∧ (claimFixed ⟨false, false⟩ start).lease = .claimed 0 := by decide

/-! ## The sweep -/

/-- Before: no sweep looks at a claimed lease. -/
def sweepBug (w : World) : World :=
  match w.lease with
  | .claimed age => { w with lease := .claimed (age + 1) }
  | _ => w

/-- The lost claim of the incident: claimed, and no Runner will report it. -/
def lostClaim : World := claimBug ⟨false, false⟩ start

/-- BUG (item 1): the slot stays occupied for every number of sweeps. -/
theorem lost_claim_pins_the_slot_BUG :
    ∀ n, slotFree (iter sweepBug n lostClaim).lease = false := by
  intro n
  have h : ∀ k (w : World), (∃ age, w.lease = .claimed age) →
      ∃ age, (iter sweepBug k w).lease = .claimed age := by
    intro k
    induction k with
    | zero => intro w h; exact h
    | succ k ih =>
      intro w ⟨age, hw⟩
      exact ih (sweepBug w) ⟨age + 1, by simp [sweepBug, hw]⟩
  obtain ⟨age, h⟩ := h n lostClaim ⟨0, by decide⟩
  simp [h, slotFree]

/-- Sweeps a claim may stay unreported before it is released. With a 60 s
  sweep and a 30 s grace period, the first sweep that sees the claim older
  than the grace period releases it: at most two sweeps, 90 s. -/
def graceSweeps : Nat := 1

/-- `reclaimUnreportedClaims`: a lease still `claimed` with no report after the
  grace period is failed. A lease that reported is never touched. -/
def sweepFixed (w : World) : World :=
  match w.lease with
  | .claimed age =>
      if age < graceSweeps then { w with lease := .claimed (age + 1) }
      else { w with lease := .failed }
  | _ => w

/-- FIXED (item 1): the lost claim is released within the bound, and the slot
  is free again with no restart. -/
theorem fixed_lost_claim_is_released :
    (iter sweepFixed (graceSweeps + 1) lostClaim).lease = .failed
      ∧ slotFree (iter sweepFixed (graceSweeps + 1) lostClaim).lease = true := by decide

/-- A lease that reported `preparing` is never reclaimed, however long it runs. -/
theorem fixed_reported_lease_is_never_reclaimed (workload : Bool) :
    ∀ n, iter sweepFixed n ⟨.preparing, workload⟩ = ⟨.preparing, workload⟩ := by
  intro n
  induction n with
  | zero => rfl
  | succ n ih => simpa [iter, sweepFixed] using ih

/-! ## A late report -/

/-- `POST /:id/status` with `preparing`, then the Runner's side effects. The
  Runner creates nothing unless the report was accepted; a terminal or
  unclaimed lease refuses it. -/
def report (w : World) : World :=
  match w.lease with
  | .claimed _ => ⟨.preparing, true⟩
  | .preparing => ⟨.preparing, true⟩
  | _ => w

inductive Event where
  | sweep
  | report
deriving DecidableEq, Repr

def step (w : World) : Event → World
  | .sweep => sweepFixed w
  | .report => report w

def run (w : World) (events : List Event) : World := events.foldl step w

/-- A released lease has no workload behind it. -/
def safe (w : World) : Prop := w.workload = true → w.lease = .preparing

theorem step_keeps_safe (w : World) (e : Event) (h : safe w) : safe (step w e) := by
  cases e with
  | report =>
    cases hl : w.lease <;> simp_all [safe, step, report]
  | sweep =>
    intro hw
    cases hl : w.lease with
    | claimed age =>
      have : w.workload = true := by
        simp [step, sweepFixed, hl] at hw; split at hw <;> simpa using hw
      simp [safe, hl] at h; exact absurd this (by simp [h])
    | pending => simp_all [safe, step, sweepFixed]
    | preparing => simp_all [safe, step, sweepFixed]
    | failed => simp_all [safe, step, sweepFixed]

/-- FIXED: over every interleaving of sweeps and reports after a claim, a
  workload exists only under a lease that reported and was never reclaimed. -/
theorem fixed_no_workload_under_a_released_lease (events : List Event) :
    safe (run (claimBug ⟨true, false⟩ start) events) := by
  have h : ∀ (es : List Event) (w : World), safe w → safe (run w es) := by
    intro es
    induction es with
    | nil => intro w h; exact h
    | cons e es ih => intro w h; exact ih (step w e) (step_keeps_safe w e h)
  exact h events _ (by simp [safe, claimBug, start])

/-- A late report after the release is refused: nothing changes. -/
theorem fixed_late_report_is_rejected (workload : Bool) :
    report ⟨.failed, workload⟩ = ⟨.failed, workload⟩ := rfl

/-! ## Why the lease is failed and not returned to `pending` -/

/-- The alternative: put the lease back for redelivery to the same Runner. -/
def sweepRequeue (w : World) : World :=
  match w.lease with
  | .claimed age =>
      if age < graceSweeps then { w with lease := .claimed (age + 1) }
      else { w with lease := .pending }
  | _ => w

/-- A report carries only the lease id. After a requeue and a second claim, a
  late report of the FIRST delivery is accepted as current: the first process
  starts a workload under a lease that was taken away from it and handed out
  again. Failing the lease instead makes the next Run a new lease id. -/
theorem requeue_accepts_a_stale_report_BUG :
    let reclaimed := iter sweepRequeue (graceSweeps + 1) (claimBug ⟨true, false⟩ start)
    reclaimed.lease = .pending
      ∧ (report (claimBug ⟨true, false⟩ reclaimed)) = ⟨.preparing, true⟩ := by decide

end ControlPlaneClaimReclaim
