/-!
# selfhost: a first run that is ready and verified, and gone after an hour

Sources (ato-api): `src/routes/capsule_bundles.ts` (`importPortableApplication`
with `start: true` calls `launchPortableRoute` without `maxDurationSeconds`),
`src/services/execution_authorization.ts` (`DEFAULT_POLICY` is
`on_demand`/`stopped`; `reserveExecutionAuthorization` returns `null` unless
the policy is `always_on`/`running` or the Runner is `ato_managed`;
`setRuntimePolicy`; `renewExecutionAuthorization`),
`src/services/portable_application_launch.ts` (the lease command carries
`execution_authorization` only when one was reserved),
`src/services/always_on.ts` (`scheduleInstanceRestart` returns `false` unless
the policy is `always_on`/`running`), `src/routes/compute_instances.ts`
(`PATCH /compute-instances/:id/runtime-policy` stops the Run, sets the policy
and launches a replacement), `src/routes/runner_leases.ts` (`POST /:id/status`
stores the Runner's `error.code`).
Sources (ato): `apps/connected-realization-worker/src/runtime_launch/lease.rs`
(`RUNTIME_LAUNCH_MAX_DURATION_SECS` = 3600; `maximum_lifetime`: no
`max_duration_secs` and no authorization gives the one-hour cap; `wait_for_stop`
bails with "the Run exceeded its maximum lifetime without a stop request"),
`apps/connected-realization-worker/src/lib.rs` (`execute_lease` reports that
error as `realization_failed`, the code of any other failure).
Sources (ato-pwa): `src/selfhost/SelfhostApp.tsx` (`Library` renders a name and
an Open button per Instance and no status).

`HostedLeaseLifetime` in `AtoBugcheck.lean` models `maximum_lifetime` itself.
This file models what follows from it for an Instance and its owner.

What is abstracted:
* Time is counted in minutes; `cap` = 60.
* An authorized Run is renewed every minute; a refused renewal (owner stop,
  revoked policy) is outside this model.
* `LegacyView` is what the self-hosted Library and the Instance view give the
  owner about the Run's future. `runtime_policy.mode` is in the API response,
  and for a first run it says `on_demand`, which is true for the whole hour
  and says nothing about when the Run ends. Nothing stores the deadline.
* On demand, a later HTTP request can start a new Run. Nothing does so for a
  fixed TCP listener or a background service, which is the case modelled.
-/
namespace FirstRunResidency

inductive Policy where
  | onDemand
  | alwaysOn
deriving DecidableEq, Repr

inductive Run where
  | none
  /-- `age` minutes since the lease was claimed; `authorized` = the lease
    command carried a renewable execution authorization. -/
  | live (age : Nat) (authorized : Bool)
deriving DecidableEq, Repr

inductive Ending where
  | crash
  | lifetime
deriving DecidableEq, Repr

structure World where
  policy : Policy
  run : Run
  lastEnding : Option Ending
  verified : Bool
deriving DecidableEq, Repr

def cap : Nat := 60

/-- One minute. -/
def tick (w : World) : World :=
  match w.run with
  | .live age true => { w with run := .live (age + 1) true }
  | .live age false =>
    if age + 1 < cap then { w with run := .live (age + 1) false }
    else { w with run := .none, lastEnding := some .lifetime }
  | .none =>
    match w.policy with
    | .alwaysOn => { w with run := .live 0 true }
    | .onDemand => w

def after (n : Nat) (w : World) : World := Nat.repeat tick n w

def alive (w : World) : Bool :=
  match w.run with
  | .live _ _ => true
  | .none => false

/-- `POST /v1/capsule-bundles/:id/import-app {start: true}`, verified. -/
def firstRun : World := ⟨.onDemand, .live 0 false, none, true⟩

/-- `PATCH runtime-policy {mode: always_on, desired_state: running}`. -/
def keepRunning (w : World) : World :=
  { w with policy := .alwaysOn, run := .live 0 true }

/-- What the owner is shown today. -/
structure LegacyView where
  ready : Bool
  verified : Bool
  policy : Policy
deriving DecidableEq, Repr

def legacyView (w : World) : LegacyView := ⟨alive w, w.verified, w.policy⟩

/-- The code stored for the ended Run today. -/
def legacyCode : Ending → String
  | .crash => "realization_failed"
  | .lifetime => "realization_failed"

/-- Proposed Instance view: `run_lifetime.stops_at` (here: minutes left),
  `run_lifetime.resident`, `last_stop.reason`. -/
structure View where
  ready : Bool
  verified : Bool
  stopsIn : Option Nat
  resident : Bool
  lastStop : Option String
deriving DecidableEq, Repr

def fixedCode : Ending → String
  | .crash => "realization_failed"
  | .lifetime => "run_lifetime_ended"

def resident (w : World) : Bool :=
  match w.policy, w.run with
  | .alwaysOn, .live _ true => true
  | _, _ => false

def view (w : World) : View :=
  { ready := alive w
    verified := w.verified
    stopsIn := match w.run with
      | .live age false => some (cap - age)
      | _ => none
    resident := resident w
    lastStop := w.lastEnding.map fixedCode }

theorem unauthorized_run_ages (p : Policy) (e : Option Ending) (v : Bool) (age : Nat) :
    ∀ m, age + m < cap → after m ⟨p, .live age false, e, v⟩ = ⟨p, .live (age + m) false, e, v⟩ := by
  intro m
  induction m with
  | zero => intro _; rfl
  | succ m ih =>
    intro h
    have g := ih (by omega)
    simp only [after, Nat.repeat] at g ⊢
    rw [g]
    simp [tick, Nat.add_assoc, h]

/-- An unauthorized Run ends exactly `cap - age` minutes later. -/
theorem unauthorized_run_ends (p : Policy) (e : Option Ending) (v : Bool) (age : Nat)
    (h : age < cap) :
    after (cap - age) ⟨p, .live age false, e, v⟩ = ⟨p, .none, some .lifetime, v⟩ := by
  have step : cap - age = (cap - age - 1) + 1 := by omega
  rw [step]
  have g := unauthorized_run_ages p e v age (cap - age - 1) (by omega)
  simp only [after, Nat.repeat] at *
  rw [g]
  have : ¬ (age + (cap - age - 1) + 1 < cap) := by omega
  simp [tick, this]

theorem on_demand_stays_stopped (e : Option Ending) (v : Bool) :
    ∀ n, after n ⟨.onDemand, .none, e, v⟩ = ⟨.onDemand, .none, e, v⟩ := by
  intro n
  induction n with
  | zero => rfl
  | succ n ih =>
    simp only [after, Nat.repeat] at *
    rw [ih]
    rfl

theorem after_add (m n : Nat) (w : World) : after (m + n) w = after n (after m w) := by
  induction n with
  | zero => rfl
  | succ n ih =>
    show tick (after (m + n) w) = tick (after n (after m w))
    rw [ih]

/-- BUG: the first run is ready and verified, ends at the cap, and never
  comes back. -/
theorem first_run_ends_and_stays_stopped_BUG :
    alive firstRun = true ∧ firstRun.verified = true ∧
      ∀ n, alive (after (cap + n) firstRun) = false := by
  refine ⟨rfl, rfl, ?_⟩
  intro n
  rw [after_add]
  have h := unauthorized_run_ends .onDemand none true 0 (by decide)
  simp only [Nat.sub_zero] at h
  simp only [firstRun]
  rw [h, on_demand_stays_stopped]
  rfl

/-- BUG: nothing the owner is shown changes while the hour runs out. The
  view one minute before the end equals the view at the start. -/
theorem no_warning_before_the_end_BUG (age : Nat) (h : age < cap) :
    legacyView (after age firstRun) = legacyView firstRun := by
  have g := unauthorized_run_ages .onDemand none true 0 age (by omega)
  simp only [firstRun]
  rw [g]
  rfl

/-- BUG: the ended Run carries the code of any crash. -/
theorem lifetime_end_looks_like_a_crash_BUG : legacyCode .lifetime = legacyCode .crash := rfl

/-- The three together, as one reachable Instance. -/
theorem silent_non_resident_instance_BUG :
    ∃ w : World, alive w = true ∧ w.verified = true
      ∧ (∀ n, alive (after (cap + n) w) = false)
      ∧ (∀ age, age < cap → legacyView (after age w) = legacyView w)
      ∧ (after cap w).lastEnding.map legacyCode = some (legacyCode .crash) := by
  refine ⟨firstRun, rfl, rfl, first_run_ends_and_stays_stopped_BUG.2.2,
    no_warning_before_the_end_BUG, ?_⟩
  have h := unauthorized_run_ends .onDemand none true 0 (by decide)
  simp only [Nat.sub_zero] at h
  simp only [firstRun]
  rw [h]
  rfl

/-- FIXED: the view of a Run without an authorization names its end, and the
  Run is alive until then and ended at that minute. -/
theorem stop_time_is_shown_and_exact (p : Policy) (e : Option Ending) (v : Bool)
    (age : Nat) (h : age < cap) :
    (view ⟨p, .live age false, e, v⟩).stopsIn = some (cap - age)
      ∧ (∀ m, m < cap - age → alive (after m ⟨p, .live age false, e, v⟩) = true)
      ∧ (after (cap - age) ⟨p, .live age false, e, v⟩).run = .none := by
  refine ⟨rfl, ?_, ?_⟩
  · intro m hm
    rw [unauthorized_run_ages p e v age m (by omega)]
    rfl
  · rw [unauthorized_run_ends p e v age h]

/-- FIXED: the shown time changes every minute, so the first run is told
  apart from a resident one from the first minute on. -/
theorem first_run_is_told_apart :
    (view firstRun).resident = false ∧ (view firstRun).stopsIn = some cap
      ∧ view firstRun ≠ view (keepRunning firstRun) := by
  decide

theorem resident_stays_resident (w : World) (h : resident w = true) :
    resident (tick w) = true := by
  obtain ⟨p, r, e, v⟩ := w
  cases p <;> cases r <;> simp_all [resident, tick]
  next age authorized => cases authorized <;> simp_all

/-- FIXED: `resident` in the view means what it says: the Run is alive at
  every later minute and no stop time is shown. -/
theorem resident_never_ends (w : World) (h : resident w = true) :
    ∀ n, alive (after n w) = true ∧ (view (after n w)).stopsIn = none := by
  intro n
  have inv : resident (after n w) = true := by
    induction n with
    | zero => exact h
    | succ n ih => exact resident_stays_resident _ ih
  generalize after n w = u at inv
  obtain ⟨p, r, e, v⟩ := u
  cases p <;> cases r <;> simp_all [resident, alive, view]
  next age authorized => cases authorized <;> simp_all

/-- FIXED: "Keep running" makes any Instance resident, at any minute. -/
theorem keep_running_is_resident (w : World) : resident (keepRunning w) = true := by
  simp [resident, keepRunning]

/-- FIXED: a Run ended by its lifetime is recorded as such. -/
theorem lifetime_end_is_distinct : fixedCode .lifetime ≠ fixedCode .crash := by decide

/-- FIXED: after the end the owner sees why the first run stopped. -/
theorem ended_first_run_says_why :
    (view (after cap firstRun)).ready = false
      ∧ (view (after cap firstRun)).lastStop = some "run_lifetime_ended" := by
  have h := unauthorized_run_ends .onDemand none true 0 (by decide)
  simp only [Nat.sub_zero] at h
  simp only [firstRun]
  rw [h]
  exact ⟨rfl, rfl⟩

end FirstRunResidency
