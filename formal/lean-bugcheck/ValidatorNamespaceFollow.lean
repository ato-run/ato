/-!
# selfhost: an API replacement that strands the validator and restarts a resident Run

Sources (ato, `deploy/selfhost/`): `compose.yaml` (`validator` uses
`network_mode: service:api`, `depends_on.api.restart`, the `validator-run.sh`
entrypoint and healthcheck), `stack` (`validator_follows_api`), `start.sh`
(`start_api`). Validator: `apps/portable-application/src/validator_agent.rs`
(`run_forever` retries a refused connection forever and never exits).
Sources (ato-api): `selfhost/server.mjs` (the verification listener binds
`127.0.0.1:8788`), `src/services/portable_attempt_lifecycle.ts`
(`PORTABLE_ATTEMPT_DEADLINE_MS` = 10 min; `reconcilePortableImportAttempt`
sets `stop_requested_at` on the lease and interrupts the attempt once
`attempt_deadline_at` has passed, whether or not any validator claimed the
job), `src/services/always_on.ts` (`scheduleAlwaysOnRestartForRun`, then
`reconcileAlwaysOnInstances` calls `launchInstanceRunForWake`),
`src/services/portable_application_launch.ts` (`launchInstanceRunForWake`
inserts a new `queued` job with a fresh deadline for every relaunch).

What is abstracted:
* A restart and a recreation of the API container are one event: the network
  namespace the listener lives in is replaced (`api` generation + 1).
* Time is counted in sweeps; `deadline` sweeps is the attempt deadline.
* `follows = true` stands for the three mechanisms of the fixed distribution
  together. The watchdog's 30 s delay is far below the deadline and is taken
  as immediate. A validator that Docker could not restart because the API was
  still down is repaired by the next `./stack up`; that window is not modelled.
* A claimed job is reported in the next sweep. A job that was claimed and
  never reported keeps today's behaviour and is outside this model.
-/
namespace ValidatorNamespaceFollow

inductive Job where
  /-- Waiting for a validator; `age` sweeps since it was created. -/
  | queued (age : Nat)
  | claimed
  | verified
deriving DecidableEq, Repr

structure World where
  /-- Generation of the API container's network namespace. -/
  api : Nat
  /-- The generation the validator is attached to. -/
  validator : Nat
  job : Job
  /-- How often the resident Run was stopped and relaunched. -/
  restarts : Nat
  /-- Owner-visible: verification is waiting because no validator is polling. -/
  validatorUnavailable : Bool
deriving DecidableEq, Repr

def deadline : Nat := 10

/-- The validator reaches `localhost:8788` only inside the current namespace. -/
def reaches (w : World) : Bool := w.validator == w.api

/-- `./stack up -d api` after an image change, `docker restart`, or a crash. -/
def replaceApi (w : World) : World := { w with api := w.api + 1 }

/-- What the distribution does after a replacement. -/
def settle (follows : Bool) (w : World) : World :=
  if follows then { w with validator := w.api } else w

/-- One sweep. `keepUnclaimed` is the proposed API behaviour: a job that no
  validator ever claimed re-arms and is reported, and the Run is left alone. -/
def tick (keepUnclaimed : Bool) (w : World) : World :=
  match w.job with
  | .queued age =>
    if reaches w then { w with job := .claimed, validatorUnavailable := false }
    else if age + 1 < deadline then { w with job := .queued (age + 1) }
    else if keepUnclaimed then { w with job := .queued 0, validatorUnavailable := true }
    -- deadline branch: stop the lease; the always-on policy relaunches the
    -- Instance, and the relaunch queues a new job.
    else { w with job := .queued 0, restarts := w.restarts + 1 }
  | .claimed => { w with job := .verified }
  | .verified => w

def run (keepUnclaimed : Bool) (n : Nat) (w : World) : World :=
  Nat.repeat (tick keepUnclaimed) n w

/-- One full deadline period. -/
def period (keepUnclaimed : Bool) (w : World) : World := run keepUnclaimed deadline w

theorem tick_keeps_generations (k : Bool) (w : World) :
    (tick k w).api = w.api ∧ (tick k w).validator = w.validator := by
  unfold tick
  split
  · split
    · exact ⟨rfl, rfl⟩
    · split
      · exact ⟨rfl, rfl⟩
      · split <;> exact ⟨rfl, rfl⟩
  · exact ⟨rfl, rfl⟩
  · exact ⟨rfl, rfl⟩

/-- Before the fix nothing moves the validator: one API replacement leaves it
  in the old namespace. -/
theorem replacement_strands_validator_BUG (w : World) (h : reaches w = true) :
    reaches (settle false (replaceApi w)) = false := by
  simp [reaches, settle, replaceApi] at *
  omega

/-- A stranded validator stays stranded: no sweep reattaches it. -/
theorem stranded_stays_stranded (k : Bool) (n : Nat) (w : World)
    (h : reaches w = false) : reaches (run k n w) = false := by
  induction n with
  | zero => exact h
  | succ n ih =>
    have g := tick_keeps_generations k (run k n w)
    simp only [reaches, run, Nat.repeat] at *
    rw [g.1, g.2]
    exact ih

/-- One deadline period with a stranded validator and the current API: the
  job is never claimed and the Run is stopped and relaunched once. -/
theorem stranded_period_restarts (a v r : Nat) (u : Bool) (h : (v == a) = false) :
    period false ⟨a, v, .queued 0, r, u⟩ = ⟨a, v, .queued 0, r + 1, u⟩ := by
  simp [period, run, deadline, Nat.repeat, tick, reaches, h]

/-- BUG: a routine API replacement restarts the resident Run, and keeps
  restarting it once per deadline period for as long as nobody notices. -/
theorem replacement_restart_loop_BUG (a r : Nat) (n : Nat) :
    (Nat.repeat (period false) n
        (settle false (replaceApi ⟨a, a, .queued 0, r, false⟩))).restarts = r + n
      ∧ (Nat.repeat (period false) n
        (settle false (replaceApi ⟨a, a, .queued 0, r, false⟩))).validatorUnavailable = false := by
  have key : Nat.repeat (period false) n ⟨a + 1, a, .queued 0, r, false⟩
      = ⟨a + 1, a, .queued 0, r + n, false⟩ := by
    induction n with
    | zero => rfl
    | succ n ih =>
      have h : (a == a + 1) = false := by simp
      simp only [Nat.repeat, ih]
      rw [stranded_period_restarts (a + 1) a (r + n) false h]
      simp [Nat.add_assoc]
  simp only [settle, replaceApi]
  simp [key]

/-- FIXED (distribution): after any replacement the validator is inside the
  current namespace. -/
theorem follow_reattaches (w : World) : reaches (settle true (replaceApi w)) = true := by
  simp [reaches, settle, replaceApi]

/-- A validator that reaches the API never lets a job run into its deadline:
  the Run is never restarted, for either API behaviour. -/
theorem attached_never_restarts (k : Bool) (n : Nat) (w : World)
    (h : reaches w = true) : (run k n w).restarts = w.restarts ∧ reaches (run k n w) = true := by
  induction n with
  | zero => exact ⟨rfl, h⟩
  | succ n ih =>
    obtain ⟨r, a⟩ := ih
    have g := tick_keeps_generations k (run k n w)
    simp only [run, Nat.repeat] at *
    refine ⟨?_, ?_⟩
    · rw [← r]
      generalize Nat.repeat (tick k) n w = v at a
      unfold tick
      split
      · simp [a]
      · rfl
      · rfl
    · simp only [reaches] at *
      rw [g.1, g.2]
      exact a

/-- FIXED, end to end: any number of API replacements, each followed by the
  distribution's settle step, then any number of sweeps: no restart. -/
theorem replacements_never_restart (k : Bool) (m n : Nat) (w : World) :
    (run k n (Nat.repeat (fun w => settle true (replaceApi w)) (m + 1) w)).restarts
      = w.restarts := by
  have attached : reaches (Nat.repeat (fun w => settle true (replaceApi w)) (m + 1) w) = true :=
    follow_reattaches _
  have same : ∀ m, (Nat.repeat (fun w => settle true (replaceApi w)) m w).restarts = w.restarts := by
    intro m
    induction m with
    | zero => rfl
    | succ m ih => simpa [Nat.repeat, settle, replaceApi] using ih
  rw [(attached_never_restarts k n _ attached).1, same]

/-- With the validator attached the job is claimed and verified in two sweeps. -/
theorem attached_job_is_verified (k : Bool) (a r : Nat) (u : Bool) :
    (run k 2 ⟨a, a, .queued 0, r, u⟩).job = .verified := by
  simp [run, Nat.repeat, tick, reaches]

/-- FIXED (API, independent of the distribution): a job no validator claimed
  never stops the Run, however long the validator is away. -/
theorem unclaimed_job_never_stops_run (n : Nat) (w : World) :
    (run true n w).restarts = w.restarts := by
  induction n with
  | zero => rfl
  | succ n ih =>
    simp only [run, Nat.repeat] at *
    rw [← ih]
    generalize Nat.repeat (tick true) n w = v
    unfold tick
    split
    · split
      · rfl
      · split <;> rfl
    · rfl
    · rfl

/-- ... and the outage is reported instead: after one deadline period the
  owner sees that verification is waiting for a validator. -/
theorem unclaimed_job_is_reported (a v r : Nat) (h : (v == a) = false) :
    (period true ⟨a, v, .queued 0, r, false⟩).validatorUnavailable = true := by
  simp [period, run, deadline, Nat.repeat, tick, reaches, h]

/-- The report clears as soon as a validator claims the job. -/
theorem report_clears_on_claim (k : Bool) (a r age : Nat) :
    (tick k ⟨a, a, .queued age, r, true⟩).validatorUnavailable = false := by
  simp [tick, reaches]

end ValidatorNamespaceFollow
