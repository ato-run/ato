/-!
# Connected Runner: a lease claimed for a Runner that never received it

Sources (ato `apps/connected-realization-worker/src/lib.rs`, ato-api
`src/routes/runners.ts`, `selfhost/server.mjs`):

* `attemptClaimNextLease` flips the lease to `claimed` inside the long-poll
  handler and only then writes the response.  The selfhost HTTP bridge never
  observes a closed client socket, so a long-poll issued by a Runner process
  that has since exited still claims, and the response is discarded.
* Any open lease makes `/leases/next` answer `lease: null` (capacity gate) for
  a single-slot Runner.
* The Runner reconciled the control plane's open leases only in startup
  recovery, and treated `lease: null` as "sleep and poll again".

The model is one lease and one single-slot Runner process.
-/
namespace RunnerClaimDelivery

inductive LeaseStatus where
  | pending
  | claimed
  /-- The Runner reported it (preparing / ready / failed). -/
  | reported
  /-- Recovery reported the stop and the control plane released slot, writer and volume. -/
  | released
deriving DecidableEq, Repr

structure World where
  lease : LeaseStatus
  /-- The live Runner process received the claim response. -/
  runnerHolds : Bool
  /-- Idle polls since the Runner last listed the control plane's open leases. -/
  sinceAudit : Nat
deriving DecidableEq, Repr

/-- The control plane's claim: committed whether or not the response is delivered. -/
def claim (delivered : Bool) (w : World) : World :=
  match w.lease with
  | .pending => { w with lease := .claimed, runnerHolds := delivered }
  | _ => w

def iter (f : α → α) : Nat → α → α
  | 0, a => a
  | n + 1, a => iter f n (f a)

theorem iter_fixpoint (f : α → α) (a : α) (h : f a = a) : ∀ n, iter f n a = a := by
  intro n
  induction n with
  | zero => rfl
  | succ n ih => simp [iter, h, ih]

/-- One pass of the Runner main loop before the fix. -/
def runnerStepBug (w : World) : World :=
  if w.runnerHolds then { w with lease := .reported }
  else
    match w.lease with
    | .pending => claim true w
    -- `let Some(lease) = claim.lease else { sleep; heartbeat; continue }`
    | _ => w

/-- The lease claimed by a request whose client is gone. -/
def lostClaim : World := claim false ⟨.pending, false, 0⟩

/-- BUG: the Runner keeps polling and heartbeating; the lease stays `claimed`
  and unreported for every number of loop passes. -/
theorem lost_claim_pins_the_slot_BUG :
    ∀ n, (iter runnerStepBug n lostClaim).lease = .claimed
      ∧ (iter runnerStepBug n lostClaim).runnerHolds = false := by
  intro n
  have h := iter_fixpoint runnerStepBug lostClaim (by decide) n
  rw [h]
  decide

/-- Idle polls between open-lease audits: 30 s over the 5 s idle poll. -/
def auditPeriod : Nat := 6

/-- One pass of the fixed loop: an idle Runner lists the control plane's open
  leases every `auditPeriod` polls and hands what it finds to lease recovery. -/
def runnerStepFixed (w : World) : World :=
  if w.runnerHolds then { w with lease := .reported, sinceAudit := 0 }
  else
    match w.lease with
    | .pending => claim true w
    | .claimed =>
        if w.sinceAudit + 1 < auditPeriod then { w with sinceAudit := w.sinceAudit + 1 }
        else { w with lease := .released, sinceAudit := 0 }
    | _ => w

/-- FIXED: a claimed lease the Runner does not hold is released within one
  audit period, whatever the audit phase, with no restart. -/
theorem fixed_lost_claim_is_released (k : Fin auditPeriod) :
    (iter runnerStepFixed (auditPeriod - k.val) ⟨.claimed, false, k.val⟩).lease = .released := by
  revert k
  decide

theorem fixed_lost_claim_from_the_incident :
    (iter runnerStepFixed auditPeriod lostClaim).lease = .released := by decide

/-- The audit never takes a lease away from the process that is executing it. -/
theorem fixed_keeps_a_delivered_lease (w : World) (h : w.runnerHolds = true) :
    (runnerStepFixed w).lease = .reported := by
  simp [runnerStepFixed, h]

/-- A delivered claim still runs exactly as before. -/
theorem fixed_delivered_claim_is_reported :
    (iter runnerStepFixed 2 ⟨.pending, false, 0⟩).lease = .reported := by decide

/-! ## Every claim response is accounted for -/

/-- What a `/leases/next` body can be. -/
inductive Body where
  | empty
  | lease
  /-- Names a lease id, but the command does not decode. -/
  | undecodable
  /-- No usable lease id at all. -/
  | unreadable
deriving DecidableEq, Repr

inductive Reaction where
  | idle
  | execute
  | reportFailed
  /-- Log, back off and poll again. -/
  | retryOnly
  | retryAndAudit
deriving DecidableEq, Repr

/-- Before: every decode error was a "transient control-plane failure". -/
def reactBug : Body → Reaction
  | .empty => .idle
  | .lease => .execute
  | .undecodable => .retryOnly
  | .unreadable => .retryOnly

def reactFixed : Body → Reaction
  | .empty => .idle
  | .lease => .execute
  | .undecodable => .reportFailed
  | .unreadable => .retryAndAudit

/-- A body that may stand for a committed claim must lead to a report for that
  lease or to an audit of the control plane's open leases. -/
def accountedFor : Body → Reaction → Bool
  | .empty, _ => true
  | .lease, r => r == .execute
  | .undecodable, r => r == .reportFailed
  | .unreadable, r => r == .retryAndAudit

theorem undecodable_claim_is_dropped_BUG :
    accountedFor .undecodable (reactBug .undecodable) = false := by decide

theorem fixed_every_claim_response_is_accounted_for :
    ∀ b : Body, accountedFor b (reactFixed b) = true := by
  intro b
  cases b <;> decide

end RunnerClaimDelivery
