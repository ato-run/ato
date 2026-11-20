/-!
# Hosted validator: a certificate endpoint the validator cannot reach

Source: ato `apps/portable-application/src/validator_agent.rs`
(`verify_hosted`, `with_retry`).

Before the fix every TLS probe error was wrapped as
`RetryableFailure::retryable`, and `with_retry` only gives up when
`now + pause >= deadline`, where `deadline` is the job's remaining budget.
On giving up the whole job was abandoned (`report_runtime_failure`), so no
receipt was produced and every observation of K, including the HTTP and
snapshot ones already made, was left undecided.

Time is in seconds.  `attempt` is what one probe costs (0 for an immediate
"network is unreachable", up to the 10 s probe timeout).
-/
namespace ValidatorTlsObservation

/-- `ato_tls_observe::TlsProbeFailureClass`. -/
inductive Class where
  | unreachable
  | refused
  | timeout
  | connectFailed
  | noCertificate
deriving DecidableEq, Repr

/-- The retry pause once the backoff has reached its cap. -/
def pause : Nat := 5

/-- `with_retry` against an endpoint that keeps failing: the time at which it gives up. -/
def giveUpBug (deadline attempt : Nat) : Nat → Nat → Nat
  | 0, now => now
  | fuel + 1, now =>
      let t := now + attempt
      if deadline ≤ t + pause then t else giveUpBug deadline attempt fuel (t + pause)

/-- BUG: whatever the failure, the observation is not decided until one pause
  before the job deadline. -/
theorem failing_probe_runs_to_the_job_deadline_BUG (deadline attempt : Nat) :
    ∀ fuel now, deadline ≤ now + fuel →
      deadline ≤ giveUpBug deadline attempt fuel now + pause := by
  intro fuel
  induction fuel with
  | zero => intro now h; simp [giveUpBug, pause]; omega
  | succ fuel ih =>
      intro now h
      simp only [giveUpBug]
      split
      · assumption
      · apply ih
        simp [pause]
        omega

/-- The incident: a 600 s job budget and an immediate "network is unreachable". -/
theorem unreachable_endpoint_holds_the_job_BUG :
    595 ≤ giveUpBug 600 0 600 0 := by
  have h := failing_probe_runs_to_the_job_deadline_BUG 600 0 600 0 (by omega)
  simp [pause] at h
  omega

/-- After the fix: only a failure that waiting can change is asked again. -/
def mayChange : Class → Bool
  | .unreachable => false
  | .timeout => false
  | _ => true

/-- `observe_tls_within`: `deadline` is the observation's own budget. -/
def giveUpFixed (c : Class) (deadline attempt : Nat) : Nat → Nat → Nat
  | 0, now => now
  | fuel + 1, now =>
      let t := now + attempt
      if !mayChange c || decide (deadline ≤ t + pause) then t
      else giveUpFixed c deadline attempt fuel (t + pause)

/-- FIXED: no route to the address is decided after exactly one attempt. -/
theorem fixed_unreachable_is_decided_after_one_attempt (deadline attempt fuel now : Nat) :
    giveUpFixed .unreachable deadline attempt (fuel + 1) now = now + attempt := by
  simp [giveUpFixed, mayChange]

theorem fixed_timeout_is_decided_after_one_attempt (deadline attempt fuel now : Nat) :
    giveUpFixed .timeout deadline attempt (fuel + 1) now = now + attempt := by
  simp [giveUpFixed, mayChange]

/-- FIXED: every failure class is decided within the observation's own budget
  plus one attempt, independent of the job deadline. -/
theorem fixed_failure_is_decided_within_the_observation_budget
    (c : Class) (deadline attempt : Nat) :
    ∀ fuel now, giveUpFixed c deadline attempt fuel now ≤ max now deadline + attempt := by
  intro fuel
  induction fuel with
  | zero => intro now; simp [giveUpFixed]; omega
  | succ fuel ih =>
      intro now
      simp only [giveUpFixed]
      split
      · omega
      · rename_i h
        have hlt : now + attempt + pause < deadline := by
          simp at h
          omega
        have := ih (now + attempt + pause)
        omega

/-- The observation budget is 30 s and one attempt is at most 10 s. -/
theorem fixed_observation_is_decided_within_40s (c : Class) (fuel now attempt : Nat)
    (h : attempt ≤ 10) :
    giveUpFixed c (now + 30) attempt fuel now ≤ now + 40 := by
  have := fixed_failure_is_decided_within_the_observation_budget c (now + 30) attempt fuel now
  omega

/-! ## What the job reports -/

inductive Probe where
  | presented
  | failed (c : Class)
deriving DecidableEq, Repr

inductive Verdict where
  | satisfied
  | failed
  /-- `tls_probe_failed`, naming the failure class. -/
  | probeFailed (c : Class)
deriving DecidableEq, Repr

structure Job where
  /-- HTTP and snapshot observations, already made when TLS is probed. -/
  http : List Bool
  tls : List Probe

def httpVerdict (ok : Bool) : Verdict := if ok then .satisfied else .failed

def tlsVerdict : Probe → Verdict
  | .presented => .satisfied
  | .failed c => .probeFailed c

def isPresented : Probe → Bool
  | .presented => true
  | .failed _ => false

/-- Before: one failed probe abandoned the job; there is no receipt. -/
def receiptBug (j : Job) : Option (List Verdict) :=
  if j.tls.all isPresented then some (j.http.map httpVerdict ++ j.tls.map tlsVerdict) else none

theorem one_unreachable_endpoint_loses_every_verdict_BUG :
    receiptBug ⟨[true, true], [.failed .unreachable]⟩ = none := by decide

def receiptFixed (j : Job) : List Verdict :=
  j.http.map httpVerdict ++ j.tls.map tlsVerdict

/-- FIXED: the receipt decides every observation, exactly once each. -/
theorem fixed_receipt_decides_every_observation (j : Job) :
    (receiptFixed j).length = j.http.length + j.tls.length := by
  simp [receiptFixed]

/-- FIXED: the verdicts of the other observations do not depend on any probe. -/
theorem fixed_other_verdicts_do_not_depend_on_tls (http : List Bool) (a b : List Probe) :
    (receiptFixed ⟨http, a⟩).take http.length = (receiptFixed ⟨http, b⟩).take http.length := by
  simp [receiptFixed]

theorem fixed_failed_probe_names_its_class (c : Class) :
    tlsVerdict (.failed c) = .probeFailed c := rfl

theorem fixed_incident_receipt :
    receiptFixed ⟨[true, true], [.failed .unreachable]⟩
      = [.satisfied, .satisfied, .probeFailed .unreachable] := by decide

end ValidatorTlsObservation
