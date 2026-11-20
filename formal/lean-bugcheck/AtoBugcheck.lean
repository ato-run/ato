/-
  ato-run/ato @ 0cb26f6f6 — Lean 4 models of small security-critical decision logic.
  Core Lean only (no Mathlib).  Check with:  lean AtoBugcheck.lean
  Theorems describe the modeled decision logic and its intended properties.
-/

/-! ## 1. extensions/providers/nacelle/src/system/path.rs :: validate_path -/
namespace PathAllow
/-- Absolute path = list of components; `[]` is `/`. Symlink-free world, so
    `canonicalize` is the identity on existing paths. -/
abbrev Path := List Nat

/-- Number of leading components of the nearest existing ancestor
    (the `while let Some(c) = cur { if c.exists() ... cur = c.parent() }` loop). -/
def keep (ex : Path → Bool) (a : Path) : Nat → Nat
  | 0 => 0
  | k+1 => if ex (a.take (k+1)) then k+1 else keep ex a k

/-- Root the code actually whitelists for the allowed entry `a`. -/
def codeRoot (ex : Path → Bool) (a : Path) : Path := a.take (keep ex a a.length)

def validate (ex : Path → Bool) (allowed : List Path) (p : Path) : Bool :=
  allowed.any (fun a => (codeRoot ex a).isPrefixOf p)

/-- Intended property: accepted paths lie under an allowed entry. -/
def spec (allowed : List Path) (p : Path) : Bool := allowed.any (fun a => a.isPrefixOf p)

/-- Fix: keep the non-existing remainder (as the code already does for the
    *candidate* path).  Then the whitelisted root is the entry itself. -/
def fixedRoot (ex : Path → Bool) (a : Path) : Path :=
  codeRoot ex a ++ a.drop (keep ex a a.length)

theorem fixedRoot_eq (ex : Path → Bool) (a : Path) : fixedRoot ex a = a := by
  simp [fixedRoot, codeRoot]

theorem fixed_sound (ex : Path → Bool) (allowed : List Path) (p : Path) :
    allowed.any (fun a => (fixedRoot ex a).isPrefixOf p) = spec allowed p := by
  simp [spec, fixedRoot_eq]
end PathAllow

/-! ## 2. services/netd/src/surface_websocket_auth.rs :: grant replay cache -/
namespace Grants
/-- The cache uses a nondecreasing high-water time and retains unexpired grants. -/
def cutoff (highWater now : Nat) : Nat := max highWater now

def accepted (highWater now expiry : Nat) (seen : Bool) : Bool :=
  decide (cutoff highWater now < expiry) && !seen

theorem expired_rejected (highWater now expiry : Nat)
    (h : expiry ≤ cutoff highWater now) : accepted highWater now expiry seen = false := by
  simp [accepted, Nat.not_lt_of_ge h]

theorem replay_rejected (highWater now expiry : Nat) :
    accepted highWater now expiry true = false := by
  simp [accepted]

theorem rollback_does_not_revive (highWater now expiry : Nat) (h : expiry ≤ highWater) :
    accepted highWater now expiry false = false := by
  apply expired_rejected
  simp [cutoff] at *; omega

theorem fresh_after_expiry : accepted 20 20 30 false = true := by decide
end Grants

/-! ## 3. services/netd/src/egress/policy.rs :: TransferBudget::consume -/
namespace Budget
structure B where
  used : Nat
  exh : Bool

/-- Ideal (unbounded) arithmetic. -/
def consume (limit : Nat) (b : B) (n : Nat) : B × Bool :=
  let total := b.used + n
  let exh := b.exh || decide (limit < total)
  (⟨total, exh⟩, !exh)

def Inv (limit : Nat) (b : B) : Prop := b.exh = false → b.used ≤ limit

theorem consume_inv (limit : Nat) (b : B) (n : Nat) (_ : Inv limit b) :
    Inv limit (consume limit b n).1 := by
  simp [Inv, consume]

/-- With ideal arithmetic: `consume` returning true implies the total is within budget. -/
theorem consume_sound (limit : Nat) (b : B) (n : Nat) (h : (consume limit b n).2 = true) :
    b.used + n ≤ limit := by
  simp [consume] at h; omega

/-- Saturating counter plus explicit overflow rejection. -/
def consumeU64 (limit : Nat) (b : B) (n : Nat) : B × Bool :=
  let sum := b.used + n
  let overflow := decide (18446744073709551615 < sum)
  let total := min sum 18446744073709551615
  let exh := b.exh || overflow || decide (limit < total)
  (⟨total, exh⟩, !exh)

theorem consumeU64_sound (limit : Nat) (b : B) (n : Nat)
    (h : (consumeU64 limit b n).2 = true) : b.used + n ≤ limit := by
  simp [consumeU64] at h
  omega
end Budget

/-! ## 4. services/netd/src/egress/{gate.rs,policy.rs} :: DENIED_RANGES / check_addr -/
namespace Egress
inductive Addr
  | v4 (n : Nat)
  | v6 (n : Nat)

structure Net where
  isV6 : Bool
  base : Nat
  len : Nat

/-- `ipnet::IpNet::contains`: families never match across v4/v6. -/
def Net.contains (net : Net) : Addr → Bool
  | .v4 n => !net.isV6 && n / 2 ^ (32 - net.len) == net.base / 2 ^ (32 - net.len)
  | .v6 n => net.isV6 && n / 2 ^ (128 - net.len) == net.base / 2 ^ (128 - net.len)

def ip4 (a b c d : Nat) : Nat := ((a * 256 + b) * 256 + c) * 256 + d

/-- gate.rs DENIED_RANGES (security-relevant subset; v6 values as 128-bit naturals). -/
def denied : List Net :=
  [ ⟨false, ip4 0 0 0 0, 8⟩, ⟨false, ip4 10 0 0 0, 8⟩, ⟨false, ip4 100 64 0 0, 10⟩,
    ⟨false, ip4 127 0 0 0, 8⟩, ⟨false, ip4 169 254 0 0, 16⟩, ⟨false, ip4 172 16 0 0, 12⟩,
    ⟨false, ip4 192 168 0 0, 16⟩,
    ⟨true, 0, 128⟩, ⟨true, 1, 128⟩, ⟨true, 0xffff * 2 ^ 32, 96⟩ ]

def allowed (a : Addr) : Bool := !(denied.any (·.contains a))

/-- IPv4 address the kernel really dials for a v4 or v4-mapped v6 destination. -/
def effectiveV4 : Addr → Option Nat
  | .v4 n => some n
  | .v6 n => if n / 2 ^ 32 = 0xffff then some (n % 2 ^ 32) else none

/-- HOLDS: no IPv4-mapped IPv6 literal slips past the IPv4 deny list. -/
theorem mapped_always_denied (n : Nat) (h : n / 2 ^ 32 = 0xffff) : allowed (.v6 n) = false := by
  have : (⟨true, 0xffff * 2 ^ 32, 96⟩ : Net).contains (.v6 n) = true := by
    simp [Net.contains, h]
  simp [allowed, denied, this]

/-- HOLDS: an allowed destination never dials loopback / metadata (169.254/16) over IPv4. -/
theorem no_loopback_or_linklocal (a : Addr) (h : allowed a = true) :
    ∀ m, effectiveV4 a = some m → m / 2 ^ 24 ≠ 127 ∧ m / 2 ^ 16 ≠ 169 * 256 + 254 := by
  intro m hm
  cases a with
  | v4 n =>
    simp [effectiveV4] at hm; subst hm
    simp [allowed, denied, Net.contains, ip4] at h
    omega
  | v6 n =>
    by_cases hn : n / 2 ^ 32 = 0xffff
    · rw [mapped_always_denied n hn] at h; cases h
    · simp [effectiveV4, hn] at hm
end Egress

/-! ## 5. extensions/materializers/static-web/src/manifest.rs :: validate (dead branches) -/
namespace StaticWeb
def MAX_SAFE_JSON_INTEGER := 9007199254740991
def MAX_FILE_SIZE := 67108864
def MAX_TOTAL_SIZE := 1073741824

/-- DEAD: `file.size > MAX_SAFE_JSON_INTEGER` can never be the deciding disjunct. -/
theorem safe_integer_check_dead (size : Nat) (h : ¬ size > MAX_FILE_SIZE) :
    ¬ size > MAX_SAFE_JSON_INTEGER := by
  simp [MAX_FILE_SIZE, MAX_SAFE_JSON_INTEGER] at *; omega

/-- DEAD: `checked_add(...).ok_or(ClosureLimit)` can never fail (loop invariant total ≤ MAX_TOTAL). -/
theorem checked_add_never_overflows (total size : Nat)
    (h1 : total ≤ MAX_TOTAL_SIZE) (h2 : size ≤ MAX_FILE_SIZE) : total + size < 2 ^ 64 := by
  simp [MAX_TOTAL_SIZE, MAX_FILE_SIZE] at *; omega
end StaticWeb

/-! ## 6. services/capsulehub/src/rate_limit.rs :: RateLimiter::allow (fixed window) -/
namespace RateLimit
structure Bkt where
  start : Nat
  count : Nat

def allow (limit window : Nat) (b : Bkt) (now : Nat) : Bkt × Bool :=
  let b := if now - b.start ≥ window then ⟨now, 0⟩ else b
  if b.count ≥ limit then (b, false) else (⟨b.start, b.count + 1⟩, true)

/-- HOLDS: within one bucket window the counter never exceeds the limit. -/
theorem count_le_limit (limit window : Nat) (b : Bkt) (now : Nat) (h : b.count ≤ limit) :
    (allow limit window b now).1.count ≤ limit := by
  unfold allow; simp only []; split <;> split <;> simp_all <;> omega

/-- NOTE (design): "N per minute" is not a sliding-window bound — 2·limit requests
    fit into an arbitrarily short interval straddling a window edge. -/
theorem boundary_burst :
    let r1 := allow 1 60 ⟨0, 0⟩ 59
    let r2 := allow 1 60 r1.1 60
    r1.2 = true ∧ r2.2 = true := by decide
end RateLimit

/-! ## 7. lib/runtime-attempt/src/launch/storage_capacity.rs :: check_capacity -/
namespace StorageCapacity
/-- This models only the comparison, not filesystem observation or reservation. -/
def admitted (available required : Nat) : Bool := !(available < required)

theorem admission_has_capacity (available required : Nat)
    (h : admitted available required = true) : required ≤ available := by
  simp [admitted] at h
  omega

theorem equal_capacity_admitted (n : Nat) : admitted n n = true := by
  simp [admitted]
end StorageCapacity

/-! ## 8. lib/objects/src/repository.rs :: commit_observation -/
namespace RecordContinuity
/-- The commit gate checks token, expected cursor and stream but not the record's source head. -/
def commitAllowed (tokenOk headOk streamOk : Bool) (_recordBeforeMatches : Bool) : Bool :=
  tokenOk && headOk && streamOk

def continuous (recordBeforeMatches : Bool) : Bool := recordBeforeMatches

theorem mismatched_record_BUG :
    ∃ tokenOk headOk streamOk recordBeforeMatches,
      commitAllowed tokenOk headOk streamOk recordBeforeMatches = true ∧
      continuous recordBeforeMatches = false :=
  ⟨true, true, true, false, by decide⟩
end RecordContinuity

/-! ## 9. lib/computation/src/lib.rs :: Connection::new, Boundary, identifiers -/

/-! Small total helpers used by the models below (definitional, so `decide` reduces them). -/
namespace Model

def allOf (p : α → Bool) : List α → Bool
  | [] => true
  | x :: xs => p x && allOf p xs

def findOf (p : α → Bool) : List α → Option α
  | [] => none
  | x :: xs => if p x then some x else findOf p xs

def containsNat (x : Nat) : List Nat → Bool
  | [] => false
  | y :: ys => (x == y) || containsNat x ys

def nodupNat : List Nat → Bool
  | [] => true
  | x :: xs => !containsNat x xs && nodupNat xs

end Model

/-! ### Connection::new canonicalises the endpoint pair, including the self-connection error. -/
namespace ConnectionNormalization

set_option linter.unusedSimpArgs false

def newConn (a b : Nat) : Option (Nat × Nat) :=
  if a = b then none else if a < b then some (a, b) else some (b, a)

/-- HOLDS: construction is order independent, including the self-connection branch. -/
theorem newConn_comm (a b : Nat) : newConn a b = newConn b a := by
  by_cases h : a = b
  · subst h; simp [newConn]
  · have h' : ¬ b = a := fun hc => h hc.symm
    by_cases hlt : a < b
    · have hba : ¬ b < a := by omega
      simp [newConn, h, h', hlt, hba]
    · have hba : b < a := by omega
      simp [newConn, h, h', hlt, hba]

/-- HOLDS: an endpoint is never connected to itself. -/
theorem newConn_self (a : Nat) : newConn a a = none := by
  simp [newConn]

theorem newConn_none_iff (a b : Nat) : newConn a b = none ↔ a = b := by
  by_cases h : a = b
  · subst h; simp [newConn]
  · have h' : ¬ b = a := fun hc => h hc.symm
    by_cases hlt : a < b
    · have hba : ¬ b < a := by omega
      simp [newConn, h, h', hlt, hba]
    · have hba : b < a := by omega
      simp [newConn, h, h', hlt, hba]

/-- Endpoint code for the (node, port) pair, injective for a fixed node. -/
def code (node port : Nat) : Nat := node * 1000000 + port

theorem code_inj {n p q : Nat} (h : code n p = code n q) : p = q := by
  unfold code at h
  omega

/-- Two different Ports on one Node are accepted by the constructor;
    capsule.compose@1 rejects them later (SameNodeConnectionUnsupported). -/
theorem same_node_distinct_port_accepted (n p q : Nat) (h : p ≠ q) :
    newConn (code n p) (code n q) ≠ none := by
  intro hc
  exact h (code_inj ((newConn_none_iff _ _).mp hc))

/-- Duplicate entries survive construction: the constructor has no set semantics. -/
theorem duplicate_entries_survive (a b : Nat) (h : a ≠ b) :
    ∃ c : Nat × Nat, newConn a b = some c ∧ ([c, c] : List (Nat × Nat)) ≠ [c] := by
  by_cases hlt : a < b
  · exact ⟨(a, b), by simp [newConn, h, hlt], by simp⟩
  · exact ⟨(b, a), by simp [newConn, h, hlt], by simp⟩

/-- Both encodings hold exactly the same endpoints but are different objects: the
    identity of a wiring is over the list, not over the set of its edges. -/
theorem duplicate_encoding_is_a_second_identity (c : Nat × Nat) :
    (∀ x : Nat × Nat, x ∈ [c, c] ↔ x ∈ [c]) ∧ ([c, c] : List (Nat × Nat)) ≠ [c] := by
  refine ⟨?_, by simp⟩
  intro x
  simp

end ConnectionNormalization

/-! ### Boundary is a BTreeMap, so one PortId cannot be bound twice. -/
namespace BoundaryIdentity

structure PortDef where
  proto : Nat
  role : Nat
deriving DecidableEq

def binsert (m : List (Nat × PortDef)) (k : Nat) (v : PortDef) : List (Nat × PortDef) :=
  (k, v) :: m

def blookup : List (Nat × PortDef) → Nat → Option PortDef
  | [], _ => none
  | (k', v) :: m, k => if k' = k then some v else blookup m k

theorem lookup_insert_same (m : List (Nat × PortDef)) (k : Nat) (v : PortDef) :
    blookup (binsert m k v) k = some v := by
  simp [blookup, binsert]

theorem lookup_insert_other (m : List (Nat × PortDef)) (j : Nat) (v : PortDef)
    (k : Nat) (h : k ≠ j) : blookup (binsert m j v) k = blookup m k := by
  simp [blookup, binsert, Ne.symm h]

/-- HOLDS: one PortId carries at most one PortDef and the last insertion wins, so a
    duplicate Port inside the boundary map cannot become two Ports. -/
theorem boundary_has_one_value (m : List (Nat × PortDef)) (k : Nat) (v w : PortDef) :
    blookup (binsert (binsert m k v) k w) k = some w := by
  exact lookup_insert_same (binsert m k v) k w

end BoundaryIdentity
/-! ## 10. lib/kernel/src/lib.rs :: register and register_protocol -/
namespace KernelRegistration

/-- A semantics registry, modeled as id -> registered behavior marker.
    Kernel::enabled for an already-sealed computation resolves this marker. -/
abbrev Registry := Nat → Option Nat

def insert (r : Registry) (id v : Nat) : Registry :=
  fun k => if k = id then some v else r k

/-- The code: insert first (BTreeMap::insert replaces), then read whether the
    old value existed. false = the duplicate error. -/
def codeRegister (r : Registry) (id v : Nat) : Registry × Bool :=
  (insert r id v, (r id).isNone)

/-- Fix: look first (contains_key), insert only when absent. -/
def fixedRegister (r : Registry) (id v : Nat) : Registry × Bool :=
  match r id with
  | some _ => (r, false)
  | none => (insert r id v, true)

/-- Contract expected of a registry: a call that reports an error must not
    change the registry. -/
def Atomic (f : Registry → Nat → Nat → Registry × Bool) : Prop :=
  ∀ r id v, (f r id v).2 = false → (f r id v).1 = r

theorem code_register_not_atomic : ¬ Atomic codeRegister := by
  intro h
  let r : Registry := fun _ => some 0
  have hcall : (codeRegister r 0 1).2 = false := by decide
  have hsame : (codeRegister r 0 1).1 = r := h r 0 1 hcall
  have h1 : (codeRegister r 0 1).1 0 = some 1 := by
    simp [codeRegister, insert]
  have h2 : r 0 = some 0 := rfl
  rw [hsame] at h1
  rw [h2] at h1
  exact absurd h1 (by decide)

theorem fixed_register_atomic : Atomic fixedRegister := by
  intro r id v herr
  unfold fixedRegister at herr ⊢
  split at herr
  · rfl
  · exact absurd herr (by simp)

/-- What Kernel::enabled can observe: the marker of the registered impl. -/
def offers (r : Registry) (id : Nat) : Option Nat := r id

/-- BUG witness: the error is reported, yet the already-registered
    implementation has been replaced, so enabled for an existing computation
    now returns the replacement's offers. -/
theorem duplicate_replaces_BUG :
    ∃ (r : Registry) (id v : Nat),
      (codeRegister r id v).2 = false ∧ offers (codeRegister r id v).1 id ≠ offers r id := by
  refine ⟨fun _ => some 0, 0, 1, ?_, ?_⟩
  · decide
  · simp [codeRegister, insert, offers]

/-- The fix keeps the earlier registration when it reports the duplicate. -/
theorem fixed_error_keeps_offers (r : Registry) (id v : Nat)
    (h : (fixedRegister r id v).2 = false) :
    offers (fixedRegister r id v).1 id = offers r id := by
  cases hr : r id with
  | some a => simp [fixedRegister, hr, offers]
  | none => simp [fixedRegister, hr] at h

/-- The fix still registers a fresh id. -/
theorem fixed_success_registers (r : Registry) (id v : Nat)
    (h : (fixedRegister r id v).2 = true) :
    offers (fixedRegister r id v).1 id = some v := by
  cases hr : r id with
  | some a => simp [fixedRegister, hr] at h
  | none => simp [fixedRegister, hr, offers, insert]

/-- Concrete observation: after the failed duplicate registration the marker is
    1 (the replacement), not 0 (the original). -/
theorem replacement_visible :
    (codeRegister (fun _ => some 0) 0 1).1 0 = some 1 := by
  simp [codeRegister, insert]

/-- register_protocol is textually the same pattern (the protocols map). -/
def codeRegisterProtocol (r : Registry) (id v : Nat) : Registry × Bool :=
  (insert r id v, (r id).isNone)
theorem protocol_duplicate_replaces_BUG :
    (codeRegisterProtocol (fun _ => some 0) 0 1).2 = false ∧
    (codeRegisterProtocol (fun _ => some 0) 0 1).1 0 = some 1 := by
  constructor <;> simp [codeRegisterProtocol, insert]

end KernelRegistration

/-! ## 11. lib/compose/src/validate.rs :: validate_iteratively -/
namespace ComposeDepth

/-- Graph: node id (a BTreeMap key, i.e. NodeId) -> child references, kept in
    NodeId order because the code pushes children reversed so that DFS visits
    them in ascending NodeId order. -/
abbrev Graph := Nat → List Nat

/-- One entry of the explicit visits stack: Enter / Exit. -/
structure Visit where
  isEnter : Bool
  node : Nat
  depth : Nat
deriving DecidableEq, Repr

/-- Model of ValidationState::validate_iteratively. fuel stands for the code's
    ReferenceCycle rejection (a cyclic graph never terminates here). -/
def run (g : Graph) (maxDepth fuel : Nat) (stack : List Visit) (active validated : List Nat) : Bool :=
  match fuel with
  | 0 => false
  | f + 1 =>
    match stack with
    | [] => true
    | v :: rest =>
      if v.isEnter then
        if v.depth > maxDepth then false
        else if validated.elem v.node then run g maxDepth f rest active validated
        else if active.elem v.node then false
        else
          run g maxDepth f
            ((g v.node).map (fun c => ⟨true, c, v.depth + 1⟩) ++ ⟨false, v.node, v.depth⟩ :: rest)
            (v.node :: active) validated
      else
        run g maxDepth f rest (active.erase v.node) (v.node :: validated)

/-- Whole entry point: root is pushed at depth 0. -/
def validate (g : Graph) (maxDepth root fuel : Nat) : Bool :=
  run g maxDepth fuel [⟨true, root, 0⟩] [] []

/-! ### Witness graph: root -> [shared, longer], shared -> [leaf], longer -> [shared] -/

/-- Node order root, shared, longer, leaf = 0, 1, 2, 3; children ascending. -/
def gShallow : Graph
  | 0 => [1, 2]
  | 1 => [3]
  | 2 => [1]
  | _ => []

/-- The same DAG with the author-chosen node names swapped (a <-> b). -/
def gDeep : Graph
  | 0 => [2, 1]
  | 1 => [3]
  | 2 => [1]
  | _ => []

/-- Cycle witness. -/
def gCycle : Graph
  | 0 => [1]
  | 1 => [0]
  | _ => []

/-- Shared node is a leaf, so only the revisited node itself can exceed budget. -/
def gRevisit : Graph
  | 0 => [1, 2]
  | 1 => []
  | 2 => [1]
  | _ => []

/-- SPEC: every path from the root has at most maxDepth + 1 nodes. -/
def isPath (g : Graph) : List Nat → Bool
  | [] => false
  | [_] => true
  | n :: m :: rest => (g n).elem m && isPath g (m :: rest)

def Spec (g : Graph) (b root : Nat) : Prop :=
  ∀ p, isPath g (root :: p) = true → p.length + 1 ≤ b + 1

/-- The code accepts the graph whose only over-budget path is hidden behind a
    shared node already validated on the shallow path. -/
theorem accepts_shallow_first : validate gShallow 2 0 64 = true := by decide

/-- The very same DAG with the other node naming is rejected. -/
theorem rejects_deep_first : validate gDeep 2 0 64 = false := by decide

/-- The over-budget path exists: root -> longer -> shared -> leaf. -/
theorem over_budget_path : isPath gShallow [0, 2, 1, 3] = true := by decide

/-- BUG: accepted although the spec is violated. -/
theorem order_dependent_depth_BUG :
    validate gShallow 2 0 64 = true ∧ ¬ Spec gShallow 2 0 := by
  constructor
  · decide
  · intro h
    have hb := h [2, 1, 3] over_budget_path
    exact absurd hb (by decide)

/-- Fidelity: a node *itself* re-entered beyond the budget is still rejected,
    because check_depth runs before the validated-skip. -/
theorem revisited_node_itself_checked : validate gRevisit 1 0 64 = false := by decide

/-- ... and the same graph is accepted once the budget covers it. -/
theorem revisited_node_within_budget : validate gRevisit 2 0 64 = true := by decide

/-- Fidelity: cycles are rejected (fuel exhaustion stands in for ReferenceCycle;
    with enough fuel the cycle is detected). -/
theorem cycle_rejected : validate gCycle 64 0 64 = false := by decide

/-! ### Fix: order-independent longest-path height -/

/-- Longest path length from n, by fuel-bounded recursion. -/
def height (g : Graph) : Nat → Nat → Nat
  | 0, _ => 0
  | f + 1, n => 1 + (g n).foldl (fun acc c => max acc (height g f c)) 0

theorem le_foldl_max_acc (l : List Nat) (h : Nat → Nat) (acc : Nat) :
    acc ≤ l.foldl (fun a x => max a (h x)) acc := by
  induction l generalizing acc with
  | nil => simp
  | cons x xs ih =>
    rw [List.foldl_cons]
    exact Nat.le_trans (Nat.le_max_left _ _) (ih (max acc (h x)))

theorem le_foldl_max (l : List Nat) (h : Nat → Nat) (c : Nat) (hm : c ∈ l) :
    ∀ acc, h c ≤ l.foldl (fun a x => max a (h x)) acc := by
  induction l with
  | nil => simp at hm
  | cons x xs ih =>
    intro acc
    rw [List.foldl_cons]
    by_cases hx : c = x
    · subst hx
      exact Nat.le_trans (Nat.le_max_right _ _) (le_foldl_max_acc xs h (max acc (h c)))
    · have hc : c ∈ xs := by
        rcases List.mem_cons.mp hm with h' | h'
        · exact absurd h' hx
        · exact h'
      exact ih hc (max acc (h x))

/-- The height dominates every path leaving n. -/
theorem height_bounds_path (g : Graph) :
    ∀ (p : List Nat) (fuel n : Nat),
      p.length + 1 ≤ fuel → isPath g (n :: p) = true → p.length + 1 ≤ height g fuel n := by
  intro p
  induction p with
  | nil =>
    intro fuel n hf _
    cases fuel with
    | zero => simp at hf
    | succ f => simp [height]
  | cons c rest ih =>
    intro fuel n hf hp
    cases fuel with
    | zero => simp at hf
    | succ f =>
      have hmem : c ∈ g n := by
        simp [isPath] at hp
        exact hp.1
      have hrest : isPath g (c :: rest) = true := by
        simp [isPath] at hp
        exact hp.2
      have hlen : rest.length + 1 ≤ f := by simp at hf; omega
      have ih' : rest.length + 1 ≤ height g f c := ih f c hlen hrest
      have hfold : height g f c ≤ (g n).foldl (fun acc x => max acc (height g f x)) 0 :=
        le_foldl_max (g n) (fun x => height g f x) c hmem 0
      simp only [height, List.length_cons]
      omega

/-- The fix: accept iff the longest path fits the budget. -/
def fixed (g : Graph) (b root fuel : Nat) : Bool := decide (height g fuel root ≤ b + 1)

/-- Soundness of the fix: if the fuel covers every path and the height fits,
    the spec holds. -/
theorem fixed_sound (g : Graph) (b root fuel : Nat)
    (hfuel : ∀ p, isPath g (root :: p) = true → p.length + 1 ≤ fuel)
    (h : fixed g b root fuel = true) : Spec g b root := by
  intro p hp
  have hh : height g fuel root ≤ b + 1 := of_decide_eq_true h
  have hp_le : p.length + 1 ≤ height g fuel root := height_bounds_path g p fuel root (hfuel p hp) hp
  omega

/-- The fix rejects both node orderings of the same DAG (order independence). -/
theorem fixed_rejects_shallow_first : fixed gShallow 2 0 64 = false := by decide
theorem fixed_rejects_deep_first : fixed gDeep 2 0 64 = false := by decide
theorem fixed_pair_order_independent : fixed gShallow 2 0 64 = fixed gDeep 2 0 64 := by decide

/-- The fix still accepts an in-budget chain (no false rejection on the witness). -/
theorem fixed_accepts_in_budget : fixed gRevisit 2 0 64 = true := by decide


/-! ### Witness: the root lists every chain node, so each is entered at depth 1 -/

/-- Chain 4 -> 3 -> 2 -> 1 -> 5 (5 is a leaf); the root also lists 1, 2, 3, 4
    directly, so none of them is ever entered at its real path depth. -/
def gListed : Graph
  | 0 => [1, 2, 3, 4]
  | 1 => [5]
  | 2 => [1]
  | 3 => [2]
  | 4 => [3]
  | _ => []

theorem listed_nodes_BUG : validate gListed 2 0 64 = true ∧ ¬ Spec gListed 2 0 := by
  constructor
  · decide
  · intro h
    have hb := h [4, 3, 2, 1, 5] (by decide)
    exact absurd hb (by decide)

end ComposeDepth

/-! ## 12. apps/cli/src/derivation_selection.rs :: resolve_choice -/
namespace CliChoice
inductive Choice where
  | explicit (value : Nat)
  | only (value : Nat)
  | needsSelection
  deriving DecidableEq

def choose (declared : List Nat) (requested : Option Nat) : Choice :=
  match requested with
  | some r => .explicit r
  | none => match declared with | [one] => .only one | _ => .needsSelection

theorem explicit_never_replaced (declared : List Nat) (r : Nat) :
    choose declared (some r) = .explicit r := by rfl

theorem multi_requires_choice (a b : Nat) (rest : List Nat) :
    choose (a :: b :: rest) none = .needsSelection := by rfl

/-- The route the Run executes. -/
def executed (declared : List Nat) (requested : Option Nat) : Option Nat :=
  match choose declared requested with
  | .explicit value => some value
  | .only value => some value
  | .needsSelection => none

/-- VerificationReceipt.derivation_ref names the route the Run was started with. -/
def receiptRef (route : Nat) : Nat := route

theorem explicit_executes_requested (declared : List Nat) (r : Nat) :
    executed declared (some r) = some r := rfl

theorem multi_executes_nothing (a b : Nat) (rest : List Nat) :
    executed (a :: b :: rest) none = none := rfl

theorem single_executes_only (d : Nat) : executed [d] none = some d := rfl

theorem receipt_names_selected (declared : List Nat) (r : Nat) :
    (executed declared (some r)).map receiptRef = some r := rfl

theorem no_receipt_without_selection (a b : Nat) (rest : List Nat) :
    (executed (a :: b :: rest) none).map receiptRef = none := rfl
end CliChoice

/-! ## 13. apps/cli/src/lib.rs :: resume and
    extensions/adapters/workspace/src/lib.rs :: restore_workspace -/
namespace CliResumeOrder
/-- A file's content is abstracted to a number; a workspace is a total map, so
    absence of a file is not modelled. The snapshot names the files it contains,
    which is what restore_workspace iterates over. -/
abbrev Workspace := String → Nat

/-- restore_workspace: every named file is overwritten with the snapshot bytes,
    every unnamed file is left alone (nothing is deleted). -/
def restore (named : String → Bool) (snapshot ws : Workspace) : Workspace :=
  fun f => if named f then snapshot f else ws f

/-- The shipped resume: restore_workspace first (apps/cli/src/lib.rs:1553), then
    the duplicate-branch, historical-point and lease checks. The checks read the
    repository, never the working tree, so they are abstracted to a List Bool. -/
def resumeCode (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) : Workspace × Bool :=
  (restore named snapshot ws, checks.all id)

/-- Validation first, restore last: the order the property needs. -/
def resumeFixed (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) : Workspace × Bool :=
  if checks.all id then (restore named snapshot ws, true) else (ws, false)

/-- HOLDS: the shipped order overwrites a named file however the Run is rejected. -/
theorem code_rejection_writes_named (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) (f : String) (hm : named f = true) :
    (resumeCode checks named snapshot ws).1 f = snapshot f := by
  simp [resumeCode, restore, hm]

/-- HOLDS: a rejected command still changes a named file whose content differs
    from the snapshot. This is the general form of the bug. -/
theorem code_rejection_changes_workspace (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) (f : String) (hm : named f = true)
    (hne : snapshot f ≠ ws f) :
    (resumeCode checks named snapshot ws).1 ≠ ws := by
  intro h
  have hwritten : (resumeCode checks named snapshot ws).1 f = snapshot f :=
    code_rejection_writes_named checks named snapshot ws f hm
  rw [h] at hwritten
  exact hne hwritten.symm

/-- BUG witness: one failing check (duplicate --branch, historical point, lease)
    with a named file whose unsaved content differs from the snapshot. -/
theorem rejection_overwrites_BUG :
    ∃ (checks : List Bool) (named : String → Bool) (snapshot ws : Workspace) (f : String),
      (resumeCode checks named snapshot ws).2 = false ∧
      named f = true ∧ snapshot f ≠ ws f ∧
      (resumeCode checks named snapshot ws).1 ≠ ws :=
  ⟨[false], fun _ => true, fun _ => 1, fun _ => 2, "state.txt", by decide, by decide,
   by decide, by
     intro h
     have := congrFun h "state.txt"
     simp [resumeCode, restore] at this⟩

/-- HOLDS: files the snapshot does not name are never written (union semantics),
    so a rejected resume keeps the user's new files. -/
theorem code_keeps_unnamed (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) (f : String) (hm : named f = false) :
    (resumeCode checks named snapshot ws).1 f = ws f := by
  simp [resumeCode, restore, hm]

/-- The fix: a rejected Run leaves the workspace exactly as it was. -/
theorem fixed_rejection_preserves (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) (h : checks.all id = false) :
    (resumeFixed checks named snapshot ws).1 = ws := by
  simp [resumeFixed, h]

/-- The fix is safe for every rejection reason at once, not just one of them. -/
theorem fixed_never_writes_on_any_rejection (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) (f : String) (h : checks.all id = false) :
    (resumeFixed checks named snapshot ws).1 f = ws f := by
  simp [resumeFixed, h]

/-- The fix keeps the intended behaviour when every check passes. -/
theorem fixed_success_restores (checks : List Bool) (named : String → Bool)
    (snapshot ws : Workspace) (h : checks.all id = true) :
    (resumeFixed checks named snapshot ws).1 = restore named snapshot ws := by
  simp [resumeFixed, h]

/-- A resumed Run sees files the recorded snapshot does not contain: after
    restore_workspace the workspace is a union, not the snapshot. -/
theorem resumed_workspace_exceeds_snapshot (named : String → Bool)
    (snapshot ws : Workspace) (f : String) (hm : named f = false) :
    restore named snapshot ws f = ws f := by
  simp [restore, hm]
end CliResumeOrder

/-! ## 14. stale active Run lease: no command reclaims it -/
namespace LeaseRecovery
/-- claim_active_run refuses whenever runs/active.json exists, without checking
    whether the recorded Run is still alive (lib/objects/src/repository.rs:803). -/
def claim (leaseExists : Bool) : Option Bool := if leaseExists then none else some true

/-- stop_active only proceeds for an active Run whose recorded process identity
    still matches; otherwise it fails closed (lib/local-execution/src/supervisor.rs:370). -/
def stop (leaseExists statusActive identityMatches : Bool) : Option Bool :=
  if !leaseExists then some false
  else if !statusActive then none
  else if !identityMatches then none
  else some false

/-- HOLDS: an existing lease always blocks a new claim. -/
theorem claim_blocked_by_any_lease : claim true = none := rfl

/-- HOLDS: a stale lease blocks stop for every status. -/
theorem stale_lease_blocks_stop (statusActive : Bool) :
    stop true statusActive false = none := by
  cases statusActive <;> rfl

/-- HOLDS: a matching live process can be stopped normally. -/
theorem matching_process_can_stop : stop true true true = some false := rfl

theorem fresh_lease_is_claimable : claim false = some true := rfl
end LeaseRecovery


/-! ## 15. lib/runtime-attempt/src/data_plane.rs :: ModelCache::ensure_bounded -/
namespace DataPlaneQuota

/-- ensure_bounded's projected usage: what the cache will hold once this entry is
    present. "existing" is max(object_len, partial_len). -/
def projected (usage existing entry : Nat) : Nat := usage + (entry - existing)

/-- The admission decision. -/
def admitted (usage existing entry ceiling : Nat) : Bool :=
  decide (projected usage existing entry ≤ ceiling)

/-- What the import actually commits: the previous usage, minus the object and
    partial bytes this entry already occupied, plus the verified object. -/
def committed (usage objectSize partialSize entry : Nat) : Nat :=
  usage - (objectSize + partialSize) + entry

/-- Bytes on disk in the middle of a resumed import: the cache before it, the
    bytes the partial already holds, and the batch files being written now.
    resumed + pending <= entry because a batch only asks for bytes the object
    still lacks, so the transient files never hold more than the object will. -/
def peak (usage resumed pending : Nat) : Nat := usage + resumed + pending

/-- The check's "existing" is no larger than the bytes the previous usage already
    contains, so "projected" over-estimates the committed usage. -/
theorem projected_bounds_committed
    (usage objectSize partialSize entry : Nat)
    (hUsage : objectSize + partialSize ≤ usage) :
    committed usage objectSize partialSize entry ≤
      projected usage (Nat.max objectSize partialSize) entry := by
  have hMax : Nat.max objectSize partialSize ≤ objectSize + partialSize :=
    Nat.max_le.mpr ⟨Nat.le_add_right _ _, Nat.le_add_left _ _⟩
  unfold committed projected
  omega

/-- A passing admission really does bound the cache after the import. -/
theorem admission_bounds_committed
    (usage objectSize partialSize entry ceiling : Nat)
    (hUsage : objectSize + partialSize ≤ usage)
    (h : admitted usage (Nat.max objectSize partialSize) entry ceiling = true) :
    committed usage objectSize partialSize entry ≤ ceiling := by
  have hProjected : projected usage (Nat.max objectSize partialSize) entry ≤ ceiling :=
    of_decide_eq_true h
  exact Nat.le_trans
    (projected_bounds_committed usage objectSize partialSize entry hUsage)
    hProjected

/-- The partial and the range files never hold more than the object, so the peak
    of a resumed import is at most the committed usage. This refutes the guess
    that the untracked .partial.range-N files could push disk usage past the
    ceiling: usage_bytes() may not count them, but the bytes they hold are the
    tail of the same object, which the check already accounted for. -/
theorem peak_never_exceeds_committed
    (usage resumed pending entry : Nat)
    (hInObject : resumed + pending ≤ entry) :
    peak usage resumed pending ≤ committed usage 0 0 entry := by
  unfold peak committed
  omega

/-- So the ceiling also bounds the transient usage of a bounded import. -/
theorem admission_bounds_peak
    (usage resumed pending entry ceiling : Nat)
    (hInObject : resumed + pending ≤ entry)
    (h : admitted usage 0 entry ceiling = true) :
    peak usage resumed pending ≤ ceiling := by
  refine Nat.le_trans (peak_never_exceeds_committed usage resumed pending entry hInObject) ?_
  exact admission_bounds_committed usage 0 0 entry ceiling (by omega) h

/-- An import that writes nothing is admitted while the cache is within its
    ceiling. -/
theorem no_growth_import_admitted
    (usage existing entry ceiling : Nat)
    (hGrows : entry ≤ existing) (hFits : usage ≤ ceiling) :
    admitted usage existing entry ceiling = true := by
  unfold admitted projected
  rw [Nat.sub_eq_zero_of_le hGrows]
  simp [hFits]

/-- BUG (candidate): the predicate bounds the cache's TOTAL usage, not the
    import's growth. Once the cache is already above the ceiling -- the ceiling
    is a per-invocation CLI flag (--max-cache-bytes, default 64 GiB) and is not
    stored with the cache -- an import that adds no byte at all is refused with
    "import needs N bytes". apps/cli/src/model_cache.rs::import then aborts
    before touching the cache. -/
theorem no_growth_import_refused_when_over_ceiling :
    ∃ usage existing entry ceiling : Nat,
      entry ≤ existing ∧ ceiling < usage ∧
      admitted usage existing entry ceiling = false :=
  ⟨100, 10, 10, 50, by decide⟩

/-- The precise gap: for a no-growth import the predicate decides the cache
    ceiling, not the growth. -/
theorem the_gap_is_the_growth_condition :
    ∀ usage existing entry ceiling : Nat,
      entry ≤ existing → (admitted usage existing entry ceiling = true ↔ usage ≤ ceiling) := by
  intro usage existing entry ceiling hGrows
  unfold admitted projected
  rw [Nat.sub_eq_zero_of_le hGrows]
  simp

end DataPlaneQuota

/-! ## 16. lib/runtime-attempt/src/data_plane/ranges.rs :: hydrate ranges -/
namespace RangeResume

/-- have + index * chunk_bytes: the first byte of the index-th range of a batch
    taken from the current prefix length "have". -/
def rangeStart (haveBytes chunk index : Nat) : Nat := haveBytes + index * chunk

/-- min (start + chunk_bytes) total - 1: the last byte of that range. -/
def rangeEnd (haveBytes chunk total index : Nat) : Nat :=
  min (rangeStart haveBytes chunk index + chunk) total - 1

/-- A requested range is non-empty and inside the object. -/
theorem range_is_well_formed
    (haveBytes chunk total index : Nat)
    (hInObject : rangeStart haveBytes chunk index < total) (hChunk : 0 < chunk) :
    rangeStart haveBytes chunk index ≤ rangeEnd haveBytes chunk total index ∧
      rangeEnd haveBytes chunk total index < total := by
  unfold rangeEnd
  omega

/-- The range contributes exactly the bytes the object still lacks, capped at one
    chunk: neither a hole nor an overlap. -/
theorem range_length_exact
    (haveBytes chunk total index : Nat)
    (hInObject : rangeStart haveBytes chunk index < total)
    (_hChunk : 0 < chunk) :
    rangeEnd haveBytes chunk total index - rangeStart haveBytes chunk index + 1 =
      min chunk (total - rangeStart haveBytes chunk index) := by
  unfold rangeEnd
  by_cases h : rangeStart haveBytes chunk index + chunk ≤ total
  · have h1 : min (rangeStart haveBytes chunk index + chunk) total =
        rangeStart haveBytes chunk index + chunk := Nat.min_eq_left h
    have h2 : min chunk (total - rangeStart haveBytes chunk index) = chunk :=
      Nat.min_eq_left (by omega)
    omega
  · have h1 : min (rangeStart haveBytes chunk index + chunk) total = total :=
      Nat.min_eq_right (by omega)
    have h2 : min chunk (total - rangeStart haveBytes chunk index) =
        total - rangeStart haveBytes chunk index := Nat.min_eq_right (by omega)
    omega

/-- Consecutive starts differ by exactly one chunk, so the second range begins at
    the first byte the first did not cover. -/
theorem next_start_is_adjacent (haveBytes chunk index : Nat) :
    rangeStart haveBytes chunk (index + 1) = rangeStart haveBytes chunk index + chunk := by
  unfold rangeStart
  rw [Nat.add_mul, Nat.one_mul]
  omega

/-- The resumed prefix is not required to be chunk-aligned: the first range always
    starts at "have" itself, whatever the previous run appended. -/
theorem first_range_starts_at_the_prefix (haveBytes chunk : Nat) :
    rangeStart haveBytes chunk 0 = haveBytes := by
  simp [rangeStart]

/-- Adjacent ranges of one batch touch: the end of one is the byte before the
    start of the next, whenever the batch still has a whole chunk to ask for. -/
theorem ranges_do_not_overlap
    (haveBytes chunk total index : Nat)
    (hWhole : rangeStart haveBytes chunk index + chunk ≤ total)
    (hChunk : 0 < chunk) :
    rangeEnd haveBytes chunk total index + 1 = rangeStart haveBytes chunk (index + 1) := by
  unfold rangeEnd
  rw [Nat.min_eq_left hWhole, next_start_is_adjacent]
  omega

/-- A batch covers exactly min (4 * chunk_bytes) (total - have) bytes, from "have"
    to the end of what was requested: the sum of the ranges is neither shorter
    (a gap) nor longer (a duplicate) than the uncovered prefix. -/
theorem batch_covers_the_uncovered_prefix
    (haveBytes chunk total : Nat) (_hChunk : 0 < chunk) (hIn : haveBytes ≤ total) :
    haveBytes + min (4 * chunk) (total - haveBytes) = min (haveBytes + 4 * chunk) total := by
  by_cases h : 4 * chunk ≤ total - haveBytes
  · have h1 : min (4 * chunk) (total - haveBytes) = 4 * chunk := Nat.min_eq_left h
    have h2 : min (haveBytes + 4 * chunk) total = haveBytes + 4 * chunk :=
      Nat.min_eq_left (by omega)
    omega
  · have h1 : min (4 * chunk) (total - haveBytes) = total - haveBytes :=
      Nat.min_eq_right (by omega)
    have h2 : min (haveBytes + 4 * chunk) total = total :=
      Nat.min_eq_right (by omega)
    omega

end RangeResume

/-! ## 17. lib/local-execution/src/authoring.rs vs lib/compose/src/validate.rs :: wiring gates -/
namespace WiringGate
open Model

structure Port where
  id : Nat
  node : Nat
  proto : Nat
  client : Bool
  internal : Bool
deriving DecidableEq

structure Conn where
  a : Nat
  b : Nat
deriving DecidableEq

structure Cfg where
  ports : List Port
  conns : List Conn

def portOf (c : Cfg) (portId : Nat) : Option Port :=
  findOf (fun x => x.id == portId) c.ports

def connKey (x : Conn) : Nat := x.a * 1000 + x.b

def connEndpoints (x : Conn) : List Nat := [x.a, x.b]

def authoringAccepts (c : Cfg) : Bool :=
  nodupNat (c.ports.map (fun x => x.id)) &&
  allOf (fun x =>
    (x.a != x.b) &&
    (match portOf c x.a, portOf c x.b with
     | some q, some r => q.internal && r.internal && (q.proto == r.proto)
     | _, _ => false)) c.conns

def distinctNodes (c : Cfg) : Bool :=
  allOf (fun x => match portOf c x.a, portOf c x.b with
    | some q, some r => q.node != r.node
    | _, _ => false) c.conns

def composeChecks (c : Cfg) : Bool :=
  if !nodupNat (c.conns.map connKey) then false
  else if !distinctNodes c then false
  else nodupNat (c.conns.flatMap connEndpoints)

def composeAccepts (c : Cfg) : Bool := composeChecks c

def fixedAccepts (c : Cfg) : Bool := authoringAccepts c && composeChecks c

def pk (id node proto : Nat) (client internal : Bool) : Port :=
  ⟨id, node, proto, client, internal⟩

def cfgDup : Cfg :=
  ⟨[pk 1 10 7 true true, pk 2 20 7 false true], [⟨1, 2⟩, ⟨1, 2⟩]⟩

def cfgSameNode : Cfg :=
  ⟨[pk 1 10 7 true true, pk 2 10 7 false true], [⟨1, 2⟩]⟩

def cfgMultiBound : Cfg :=
  ⟨[pk 1 10 7 true true, pk 2 20 7 false true, pk 3 30 7 false true], [⟨1, 2⟩, ⟨1, 3⟩]⟩

theorem cfgDup_authoring : authoringAccepts cfgDup = true := by decide
theorem cfgDup_compose : composeAccepts cfgDup = false := by decide
theorem cfgSameNode_authoring : authoringAccepts cfgSameNode = true := by decide
theorem cfgSameNode_compose : composeAccepts cfgSameNode = false := by decide
theorem cfgMultiBound_authoring : authoringAccepts cfgMultiBound = true := by decide
theorem cfgMultiBound_compose : composeAccepts cfgMultiBound = false := by decide

/-- BUG shape: accepted by the seal gate, rejected by the semantics that owns it. -/
theorem seal_without_validate_BUG :
    ∃ c : Cfg, authoringAccepts c = true ∧ composeAccepts c = false :=
  ⟨cfgDup, cfgDup_authoring, cfgDup_compose⟩

/-- BUG shape: linear endpoint use and same-node rejection are capsule.compose@1
    rules only; the authoring gate never applies them. -/
theorem linear_endpoint_not_enforced_at_seal :
    authoringAccepts cfgMultiBound = true ∧ composeAccepts cfgMultiBound = false :=
  ⟨cfgMultiBound_authoring, cfgMultiBound_compose⟩

theorem and_split (a b : Bool) (h : (a && b) = true) : b = true := by
  cases a <;> cases b <;> simp_all

/-- Fix: adding the compose checks to the seal gate makes sealing imply validity. -/
theorem fixedAccepts_sound (c : Cfg) (h : fixedAccepts c = true) :
    composeAccepts c = true := by
  have h' : (authoringAccepts c && composeChecks c) = true := by
    simpa [fixedAccepts] using h
  exact and_split _ _ h'

end WiringGate

/-! ## 18. lib/local-execution/src/authoring.rs :: process_environment / project_peer_address -/
namespace PeerProjection
open Model

structure Port where
  id : Nat
  node : Nat
  client : Bool
  envv : Nat
  address : Nat

def projectOne (acc envv target : Nat) (loc peer : Port) : Nat :=
  if loc.node == target then
    if loc.client == true then
      if loc.envv == envv then
        if peer.address != 0 then peer.address else acc
      else acc
    else acc
  else acc

def connAddress (ports : List Port) (conns : List (Nat × Nat)) (target envv : Nat) : Nat :=
  conns.foldl (fun acc c =>
    match findOf (fun x => x.id == c.1) ports, findOf (fun x => x.id == c.2) ports with
    | some x, some y => projectOne (projectOne acc envv target x y) envv target y x
    | _, _ => acc) 0

def portA : Port := ⟨1, 10, true, 99, 0⟩
def portB : Port := ⟨2, 20, false, 0, 1001⟩
def portC : Port := ⟨3, 30, false, 0, 1002⟩
def portsABC : List Port := [portA, portB, portC]

theorem peer_address_depends_on_declaration_order :
    connAddress portsABC [(1, 2), (1, 3)] 10 99 = 1002 ∧
    connAddress portsABC [(1, 3), (1, 2)] 10 99 = 1001 := by
  decide

/-- Both connection lists hold the same two edges; only their order differs. -/
theorem same_edges_different_projection :
    connAddress portsABC [(1, 2), (1, 3)] 10 99 ≠ connAddress portsABC [(1, 3), (1, 2)] 10 99 := by
  decide

end PeerProjection

/-! ## 19. lib/compose/src/validate.rs, lib/objects/src/lib.rs :: resolution trust boundary -/
namespace ResolveTrust

def readLen (actual expected : Nat) : Nat := min actual (expected + 1)

/-- HOLDS: the one-past read accepts only the exact expected size. -/
theorem take_one_past_accepts_only_exact (actual expected : Nat)
    (h : readLen actual expected = expected) : actual = expected := by
  unfold readLen at h
  omega

theorem oversized_never_matches (actual expected : Nat) (h : expected < actual) :
    readLen actual expected ≠ expected := by
  unfold readLen
  intro hc
  omega

def accepts (metadataSize maxBytes actualLen expectedRef actualRef : Nat) : Bool :=
  if maxBytes < metadataSize then false
  else if actualLen == metadataSize then expectedRef == actualRef
  else false

theorem accepts_implies_digest (ms mb al er ar : Nat) (h : accepts ms mb al er ar = true) :
    er = ar := by
  unfold accepts at h
  by_cases h1 : mb < ms
  · simp [h1] at h
  · by_cases h2 : al == ms
    · simpa [h1, h2] using h
    · simp [h1, h2] at h

theorem accepts_implies_size (ms mb al er ar : Nat) (h : accepts ms mb al er ar = true) :
    al = ms := by
  unfold accepts at h
  by_cases h1 : mb < ms
  · simp [h1] at h
  · by_cases h2 : al == ms
    · simpa using h2
    · simp [h1, h2] at h

end ResolveTrust

/-! ## 20. lib/computation/src/lib.rs :: identifier alphabet -/
namespace IdentifierSyntax

inductive Ch where
  | lower (n : Nat)
  | digit (n : Nat)
  | dash
  | underscore
  | at
  | dot
  | colon
  | upper (n : Nat)
  | other
deriving DecidableEq, Repr

def alnum : Ch → Bool
  | .lower _ => true
  | .digit _ => true
  | _ => false

def okByte : Ch → Bool
  | .lower _ => true
  | .digit _ => true
  | .dash => true
  | .underscore => true
  | _ => false

def lastOf : Ch → List Ch → Ch
  | c, [] => c
  | _, d :: ds => lastOf d ds

def validSegment : List Ch → Bool
  | [] => false
  | [b] => alnum b
  | b :: c :: rest => alnum b && alnum (lastOf c rest) && (b :: c :: rest).all okByte

theorem valid_segment_ne_nil (s : List Ch) (h : validSegment s = true) : s ≠ [] := by
  cases s with
  | nil => simp [validSegment] at h
  | cons x xs => simp

theorem all_okByte_mem {l : List Ch} (h : l.all okByte = true) {x : Ch} (hm : x ∈ l) :
    okByte x = true := by
  simpa using (List.all_eq_true.mp h) x hm

theorem valid_segment_all (s : List Ch) (h : validSegment s = true) (hlen : 2 ≤ s.length) :
    s.all okByte = true := by
  cases s with
  | nil => simp at hlen
  | cons b rest =>
    cases rest with
    | nil => simp at hlen
    | cons c rest' =>
      have hh := h
      simp only [validSegment, Bool.and_eq_true] at hh
      exact hh.2

/-- HOLDS: at is impossible inside a valid segment, so splitting the version suffix
    on the last at is unambiguous. -/
theorem valid_segment_no_at (s : List Ch) (h : validSegment s = true) : Ch.at ∉ s := by
  cases s with
  | nil => simp [validSegment] at h
  | cons b rest =>
    cases rest with
    | nil =>
      intro hm
      simp only [validSegment] at h
      have hb : Ch.at = b := by simpa using hm
      rw [← hb] at h
      simp [alnum] at h
    | cons c rest' =>
      intro hm
      have hok := all_okByte_mem (valid_segment_all (b :: c :: rest') h (by simp)) hm
      simp [okByte] at hok

theorem valid_segment_no_upper (s : List Ch) (h : validSegment s = true) :
    ∀ n, Ch.upper n ∉ s := by
  intro n
  cases s with
  | nil => simp [validSegment] at h
  | cons b rest =>
    cases rest with
    | nil =>
      intro hm
      simp only [validSegment] at h
      have hb : Ch.upper n = b := by simpa using hm
      rw [← hb] at h
      simp [alnum] at h
    | cons c rest' =>
      intro hm
      have hok := all_okByte_mem (valid_segment_all (b :: c :: rest') h (by simp)) hm
      simp [okByte] at hok

end IdentifierSyntax


/-! ## 21. lib/local-execution/src/authoring.rs :: [encap] materializers in sealed identity -/
namespace MaterializerIdentity

/-- authoring config のうち computation に関わる部分（process / port / connection）。 -/
structure Computation where
  process : List String
deriving DecidableEq, Repr

/-- [encap] materializers。どの Materializer を encap が呼ぶかの要求。 -/
abbrev MaterializationRequest := List String

/-- 実コードの identity: seal_authoring_state は AuthoringState（= config 全体）を
    JCS 化して residual にするので、materialization 要求も残差に入る。 -/
def identityWithRequest (c : Computation) (r : MaterializationRequest) :
    Computation × MaterializationRequest :=
  (c, r)

/-- 要求を identity から除いた場合。 -/
def identityWithoutRequest (c : Computation) (_r : MaterializationRequest) :
    Computation × MaterializationRequest :=
  (c, [])

/-- MATERIALIZATION.md / AGENTS.md 不変条件 5 が要求する性質。 -/
def SameCapsule (a b : Computation × MaterializationRequest) : Prop := a.1 = b.1

/-- 反例: 同じ computation でも、materialization 要求だけが違うと別の identity になる。 -/
theorem request_changes_identity_BUG :
    ∃ c r1 r2, r1 ≠ r2 ∧
      identityWithRequest c r1 ≠ identityWithRequest c r2 := by
  refine ⟨⟨[]⟩, ["ato.replay@1"], ["ato.replay@2"], by decide, by decide⟩

/-- 実測した 3 例（[encap] materializers だけが違う同じ capsule.toml）は
    3 つの異なる ComputationRef になった。モデル上の同型。 -/
theorem three_requests_three_identities :
    (identityWithRequest ⟨["true"]⟩ ["ato.replay@1"]) ≠
      (identityWithRequest ⟨["true"]⟩ ["ato.replay@2"]) ∧
    (identityWithRequest ⟨["true"]⟩ ["ato.replay@2"]) ≠
      (identityWithRequest ⟨["true"]⟩ []) := by
  refine ⟨by decide, by decide⟩

/-- 修正版: residual から要求を落とせば identity は要求に依存しない。 -/
theorem fixed_identity_ignores_request (c : Computation) (r1 r2 : MaterializationRequest) :
    identityWithoutRequest c r1 = identityWithoutRequest c r2 := rfl

/-- 同じ computation の下で、要求の個数だけ Capsule identity が増える。 -/
theorem request_multiplies_identities :
    ((List.map (identityWithRequest ⟨[]⟩) [["a"], ["b"]]).map Prod.snd).length = 2 := by
  decide

/-- 対照: encap --materialize の選択は root を変えない
    （descriptor は論理 root の隣に運ばれる）。 -/
def encapRoot (target : Nat) (_selection : MaterializationRequest) : Nat := target

theorem encap_keeps_root (t : Nat) (r : MaterializationRequest) :
    encapRoot t r = t := rfl

/-- したがって「宣言された既定 materializer 集合」だけが identity を動かす。
    動作（encap --materialize）は動かさない。 -/
theorem declared_request_is_the_only_identity_effect :
    ∃ c r, encapRoot 7 r = 7 ∧ identityWithRequest c r ≠ identityWithRequest c [] := by
  refine ⟨⟨[]⟩, ["ato.snapshot@1"], rfl, by decide⟩

end MaterializerIdentity

/-! ## 22. apps/portable-application/src/local_instance.rs :: independent imports -/
namespace InstanceIndependence

structure Import where
  instanceId : Nat
  applicationId : Nat
  bundleDigest : Nat
deriving DecidableEq, Repr

/-- import は bundle bytes から application を、毎回新しい instance id を作る。 -/
def importOnce (bundleDigest : Nat) (freshId : Nat) : Import :=
  ⟨freshId, bundleDigest, bundleDigest⟩

/-- 同じ bytes の 2 回の import。 -/
def importTwice (bundleDigest : Nat) (id1 id2 : Nat) : Import × Import :=
  (importOnce bundleDigest id1, importOnce bundleDigest id2)

theorem imports_are_independent (d i1 i2 : Nat) (h : i1 ≠ i2) :
    let p := importTwice d i1 i2
    p.1.applicationId = p.2.applicationId ∧ p.1.instanceId ≠ p.2.instanceId := by
  simp [importTwice, importOnce, h]

/-- 同じ bytes は同じ application に載るので immutable bundle は再利用される。 -/
theorem same_bytes_share_application (d : Nat) :
    (importOnce d 1).applicationId = (importOnce d 2).applicationId := rfl

end InstanceIndependence

/-! ## 23. apps/portable-application/src/lib.rs :: claim_run / activate_run lease -/
namespace InstanceLease

inductive Status where
  | starting
  | active
  | quarantined
deriving DecidableEq, Repr

structure Lease where
  token : Nat
  status : Status
deriving DecidableEq, Repr

structure Store where
  active : Option Lease
deriving DecidableEq, Repr

/-- stop-pending.json / stop-unconfirmed.json の存在。 -/
structure World where
  store : Store
  stopPending : Bool
deriving DecidableEq, Repr

inductive Op where
  | claim
  | activate
  | release
  | save
  | quarantine
deriving DecidableEq, Repr

def startingFor (t : Nat) : World :=
  { store := { active := some { token := t, status := .starting } }, stopPending := false }

def activeFor (t : Nat) : World :=
  { store := { active := some { token := t, status := .active } }, stopPending := false }

/-- claim_run: どんな lease があっても新しい claim は拒否される。 -/
def claim (w : World) (fresh : Nat) : Option World :=
  match w.store.active with
  | none =>
      some { store := { active := some { token := fresh, status := .starting } },
             stopPending := w.stopPending }
  | some _ => none

/-- activate_run: token と run id と status=Starting の 3 つを要求する。 -/
def activate (w : World) (t : Nat) : Option World :=
  match w.store.active with
  | none => none
  | some l =>
      if l.token = t ∧ l.status = .starting then
        some { store := { active := some { token := l.token, status := .active } },
               stopPending := w.stopPending }
      else none

/-- release_run: token 一致、quarantined でない、stop-pending でない。 -/
def release (w : World) (t : Nat) : Option World :=
  match w.store.active with
  | none => some w
  | some l =>
      if l.token = t ∧ l.status ≠ .quarantined ∧ w.stopPending = false then
        some { store := { active := (none : Option Lease) }, stopPending := w.stopPending }
      else none

/-- ensure_snapshot_write_fence / save_xxx_for_run。 -/
def save (w : World) (t : Nat) : Option World :=
  match w.store.active with
  | none => none
  | some l =>
      if l.token = t ∧ l.status ≠ .quarantined ∧ w.stopPending = false then some w
      else none

def quarantine (w : World) (t : Nat) : Option World :=
  match w.store.active with
  | none => none
  | some l =>
      if l.token = t then
        some { store := { active := some { token := l.token, status := .quarantined } },
               stopPending := w.stopPending }
      else none

def step (w : World) (op : Op) (t : Nat) : Option World :=
  match op with
  | .claim => claim w t
  | .activate => activate w t
  | .release => release w t
  | .save => save w t
  | .quarantine => quarantine w t

/-- fence は健全: 他人の token では release できない。 -/
theorem release_fences_by_token :
    release (activeFor 1) 2 = none ∧
    release (activeFor 1) 1 ≠ none := by
  decide

/-- quarantine 済みの Run は token を持っていても release できない。 -/
theorem quarantined_cannot_be_released :
    release { store := { active := some { token := 1, status := .quarantined } },
              stopPending := false } 1 = none := by
  decide

/-- stop 未確認の marker がある間は release できない（finish_instance_stop の fence）。 -/
theorem stop_pending_fences_release :
    release { store := { active := some { token := 1, status := .active } },
              stopPending := true } 1 = none := by
  decide

/-- 保存も同じ fence を通る。 -/
theorem save_fences_by_token :
    save (activeFor 1) 2 = none ∧ save (activeFor 1) 1 ≠ none := by
  decide

/-- activate は Starting のときだけ通る。二重 activate はできない。 -/
theorem activate_only_from_starting :
    activate (startingFor 1) 1 ≠ none ∧ activate (activeFor 1) 1 = none := by
  decide

/-- どんな lease があっても claim は拒否される。 -/
theorem claim_blocked_by_any_lease (l : Lease) :
    claim { store := { active := some l }, stopPending := false } 9 = none := by
  cases l with
  | mk token status => cases status <;> rfl

/-- 主結果: token t を持つ Starting lease から出る道は、t を知っている操作だけ。
    token を失う（CLI 親が claim 直後に死ぬ）と、instance は claim も release も
    activate も save も quarantine もできない吸収状態になる。 -/
theorem starting_lease_is_absorbing :
    step (startingFor 1) .claim 0 = none ∧
    step (startingFor 1) .release 2 = none ∧
    step (startingFor 1) .activate 2 = none ∧
    step (startingFor 1) .save 2 = none ∧
    step (startingFor 1) .quarantine 2 = none := by
  decide

/-- 同じことは Active lease にも言える（B6 の Instance 版）。 -/
theorem active_lease_is_absorbing :
    step (activeFor 1) .claim 0 = none ∧
    step (activeFor 1) .release 2 = none ∧
    step (activeFor 1) .save 2 = none := by
  decide

/-- 逆向き: token さえ知っていれば Starting でも release できる
    （release_run は status を Active に限定しない）。 -/
theorem starting_release_allowed_with_token :
    step (startingFor 1) .release 1 ≠ none := by
  decide

/-- したがって吸収状態からの唯一の脱出は token の保持。 -/
theorem only_token_escapes :
    (∃ w', step (startingFor 1) .release 1 = some w') ∧
    step (startingFor 1) .release 2 = none := by
  refine ⟨⟨{ store := { active := (none : Option Lease) }, stopPending := false }, by decide⟩,
    by decide⟩

/-- 利用者向けの唯一の停止経路 ato app stop の前提。status が Active で
    ない Run は「still preparing and cannot be stopped」で拒否される。 -/
def userStopAllowed (w : World) : Bool :=
  match w.store.active with
  | some l => l.status == .active
  | none => false

/-- したがって Starting lease は、利用者向けの 2 経路（claim と stop）の
    両方から閉じている。token は active-run.json に残るが、それを release する
    コマンドが無いことが吸収状態の理由である（秘密性ではない）。 -/
theorem starting_lease_closed_to_user_commands :
    userStopAllowed (startingFor 1) = false ∧ claim (startingFor 1) 0 = none := by
  decide

/-- 対照: Active なら stop の前提は満たす（さらに process identity の照合が要る）。 -/
theorem active_lease_allows_user_stop :
    userStopAllowed (activeFor 1) = true := by
  decide

end InstanceLease

/-! ## 24. apps/portable-application/src/lib.rs :: stop identity check -/
namespace StopIdentity

structure Observed where
  pid : Nat
  processStart : Nat
  bootSession : Nat
deriving DecidableEq, Repr

/-- OwnedProcessIdentity::matches_live_process。 -/
def liveMatches (recorded live : Observed) : Bool :=
  recorded.pid == live.pid &&
  recorded.processStart == live.processStart &&
  recorded.bootSession == live.bootSession

/-- PID が再利用されても boot session が違えば stop しない。 -/
theorem boot_session_fences_pid_reuse (recorded : Observed) (pid : Nat) :
    recorded.bootSession ≠ 1 →
    ∀ start, liveMatches recorded ⟨pid, start, 1⟩ = false := by
  intro h start
  simp [liveMatches, h]

/-- 同じ boot でも start time が違えば stop しない。 -/
theorem start_time_fences_restart (recorded : Observed) (start : Nat) :
    recorded.processStart ≠ start →
    liveMatches recorded ⟨recorded.pid, start, recorded.bootSession⟩ = false := by
  intro h
  simp [liveMatches, h]

/-- 3 つ全て一致したときだけ stop が許される。 -/
theorem stop_requires_all_three (recorded : Observed) :
    liveMatches recorded recorded = true := by
  simp [liveMatches]

end StopIdentity

/-! ## 25. apps/portable-application/src/local_instance.rs :: v4 snapshot restore evidence -/
namespace SnapshotRestore

structure Item where
  slot : String
  savedDigest : Nat
  restoredDigest : Nat
  sourceDigest : Option Nat
  /-- Asset alias 再結合を受ける resource か（= filesystem 以外）。 -/
  rebindable : Bool
deriving DecidableEq, Repr

/-- trace_restored_to_saved の 1 slot 分。 -/
def traced (it : Item) : Bool :=
  it.restoredDigest == it.savedDigest ||
  (it.rebindable && it.sourceDigest == some it.savedDigest)

/-- observe_restored_content の 1 slot 分。live は実 bytes の digest。 -/
def accepted (it : Item) (live : Nat) : Bool :=
  traced it && (live == it.restoredDigest)

/-- 受理された slot は、実際に手元にある bytes の digest と一致している。 -/
theorem accepted_implies_observed (it : Item) (live : Nat)
    (h : accepted it live = true) : live = it.restoredDigest := by
  simp [accepted, traced] at h
  exact h.2

/-- 反例: index（metadata）だけを見る判定は、Run が受け取っていない状態でも受理する。 -/
theorem metadata_only_accepts_stale_BUG :
    ∃ it live, traced it = true ∧ it.restoredDigest ≠ live := by
  refine ⟨⟨"data", 1, 1, none, false⟩, 2, by decide, by decide⟩

/-- さらに、live を観測しない実装は saved digest と違う restored でも通る
    （restored_snapshot() 単体がこれに当たる。doc が metadata-only と明記）。 -/
theorem traced_accepts_rebound_bytes :
    ∃ it, traced it = true ∧ it.restoredDigest ≠ it.savedDigest := by
  refine ⟨⟨"data", 1, 2, some 1, true⟩, by decide, by decide⟩

/-- restore 全体の gate: 全 slot について traced と observed の両方。 -/
def gate (items : List Item) (live : String → Nat) : Bool :=
  items.all (fun it => accepted it (live it.slot))

/-- gate が通れば、index に載る全 slot の実 bytes が観測済みである。 -/
theorem gate_accepts_every_slot (items : List Item) (live : String → Nat)
    (h : gate items live = true) :
    ∀ it ∈ items, live it.slot = it.restoredDigest := by
  intro it hmem
  have hall : accepted it (live it.slot) = true := (List.all_eq_true.mp h) it hmem
  exact accepted_implies_observed it (live it.slot) hall

/-- index のみで判定する実装（= 反例側）は gate より弱い。 -/
def indexOnlyGate (items : List Item) : Bool :=
  items.all traced

theorem index_only_gate_is_weaker :
    indexOnlyGate [⟨"data", 1, 1, none, false⟩] = true ∧
    gate [⟨"data", 1, 1, none, false⟩] (fun _ => 2) = false := by
  decide

/-- 空の要求集合は gate としては自明に true。呼び出し側（validate_snapshot /
    attach_instance_snapshot）が「resource も asset も空なら拒否」を別途持つ。 -/
theorem empty_gate_is_true : gate [] (fun _ => 0) = true := rfl

end SnapshotRestore


/-! ## 26. lib/objects/src/repository.rs :: active_run recovery, next_sequence -/
namespace RecordRecovery

/-- 1件の Record。`before` = head_before, `after` = head_after。 -/
structure Rec where
  seq : Nat
  before : Nat
  after : Nat
  deriving DecidableEq, Repr

/-- `h k` を計算 k の head とし、seq が k+1..k+n の連結した Record 列。 -/
def heads (h : Nat → Nat) : Nat → Nat → List Rec
  | _, 0 => []
  | k, n + 1 => ⟨k + 1, h k, h (k + 1)⟩ :: heads h (k + 1) n

/-- `active_run` の復旧ループ本体 (repository.rs:783-799)。seq が連続で
    head_before が cursor の head と一致する限り head と seq を進める。 -/
def recoverFrom (head seq : Nat) : List Rec → Option (Nat × Nat)
  | [] => some (head, seq)
  | r :: rest =>
      if r.seq = seq + 1 then
        if r.before = head then recoverFrom r.after r.seq rest else none
      else none

/-- `seq <= record_seq` の Record は読み飛ばされる (repository.rs:779-782)。 -/
def afterCursor (seq : Nat) (records : List Rec) : List Rec :=
  records.filter (fun r => decide (seq < r.seq))

/-- 復旧後の (head, record_seq)。 -/
def recover (cursorHead cursorSeq : Nat) (records : List Rec) : Option (Nat × Nat) :=
  recoverFrom cursorHead cursorSeq (afterCursor cursorSeq records)

/-- `next_sequence`: 既存 file 名の最大 seq + 1。欠番は埋めない (888-906)。 -/
def nextSequence (records : List Rec) : Nat :=
  records.foldl (fun acc r => max acc r.seq) 0 + 1

/-- Record 列が anchor から head_before/head_after で連結しているか
    (records_for_causal_branch の検査, repository.rs:744-756)。 -/
def contiguousFrom (anchor : Nat) : List Rec → Bool
  | [] => true
  | r :: rest => (r.before = anchor) && contiguousFrom r.after rest

/-- 復旧ループは、cursor の後ろに続く未反映 Record を最後まで吸収する。 -/
theorem recoverFrom_heads (h : Nat → Nat) :
    ∀ (k n : Nat), recoverFrom (h k) k (heads h k n) = some (h (k + n), k + n) := by
  intro k n
  induction n generalizing k with
  | zero => simp [heads, recoverFrom]
  | succ n ih =>
      simp only [heads, recoverFrom]
      simp only [ite_true]
      rw [ih (k + 1)]
      have hk : k + 1 + n = k + (n + 1) := by omega
      rw [hk]

/-- 復旧の吸収性: 1件の Record を書いた後に cursor を書き損じても
    (crash しても)、復旧は head と seq の両方を新しい値へ進める。 -/
theorem recovery_absorbs_unflushed_record (h : Nat → Nat) (n : Nat) :
    recoverFrom (h n) n (heads h n 1) = some (h (n + 1), n + 1) :=
  recoverFrom_heads h n 1

/-- 欠番があると復旧はそこで止まる (nil を返し、呼び出し側は Sequence エラー)。 -/
theorem recovery_stops_at_gap : recoverFrom 103 3 [⟨5, 104, 105⟩] = none := by decide

/-- seq 2 の Record が durable にならなかった状況 (Record 1 だけが残る) で、
    cursor は (head=102, seq=2) を指したまま。復旧は前進しないが失敗もしない。 -/
def lostRecord : List Rec := [⟨1, 100, 101⟩]

theorem lost_record_is_invisible_to_the_cursor :
    recover 102 2 lostRecord = some (102, 2) := by decide

/-- `next_sequence` は最大 seq + 1 なので、失われた seq 2 をそのまま再利用する。 -/
theorem lost_record_sequence_is_reused : nextSequence lostRecord = 2 := by decide

/-- 再利用後の列: Record 1 の after=101 に対し、Record 2 の before は cursor の 102。 -/
def afterReuse : List Rec := [⟨1, 100, 101⟩, ⟨2, 102, 103⟩]

theorem reuse_breaks_the_chain : contiguousFrom 100 afterReuse = false := by decide

/-- 壊れた列でも cursor 経路 (`active_run`) は成功し続ける。 -/
theorem reuse_is_invisible_to_the_cursor : recover 103 2 afterReuse = some (103, 2) := by decide

/-- `record_seq` は狭義単調でない: 2件 commit した後も cursor は seq 2 のまま。 -/
theorem record_seq_not_strictly_monotone :
    recover 102 2 lostRecord = some (102, 2) ∧
    recover 103 2 afterReuse = some (103, 2) := by decide

/-! ### 2段階 commit の部分失敗 (cursor だけ書き損じる) -/

/-- `commit_observation` の受理条件 (repository.rs:632)。token と stream の一致は前提。 -/
def commitAccepted (cachedHead recoveredHead : Nat) : Bool := cachedHead = recoveredHead

/-- sink の cached head は成功時しか更新されない (supervisor.rs:970-972)。 -/
def sinkCachedAfter (cached committed : Nat) (ok : Bool) : Nat := if ok then committed else cached

/-- Record 2 は durable だが cursor の書込みが失敗した状況。 -/
def cursorWriteFailed : List Rec := [⟨1, 100, 101⟩, ⟨2, 101, 102⟩]

/-- 復旧は新しい head 102 を返すのに、sink の cached head は 101 のままで、
    以後の emit はすべて受理されない (再同期の経路が sink に無い)。 -/
theorem cursor_write_failure_stalls_the_sink :
    recover 101 1 cursorWriteFailed = some (102, 2) ∧
    commitAccepted 101 102 = false ∧
    commitAccepted (sinkCachedAfter 101 102 false) 102 = false := by decide

/-- 明示的に cursor を読み直せば前進できる (sink にだけ無い経路)。 -/
theorem resync_restores_progress : commitAccepted 102 102 = true := by decide

end RecordRecovery

/-! ## 27. lib/objects/src/repository.rs :: create_branch origin/head order -/
namespace BranchOriginOrder

structure State where
  head : Option Nat
  origin : Option Nat
  deriving DecidableEq, Repr

/-- `atomic_create`: 既存 file があれば AlreadyExists で失敗し、中身は変えない。 -/
def atomicCreate (slot : Option Nat) (value : Nat) : Option Nat × Bool :=
  match slot with
  | none => (some value, true)
  | some _ => (slot, false)

/-- 実装の順序: ① origin ② head。head の書込みだけを失敗させる。 -/
def createBranchCode (s : State) (origin : Option Nat) (head : Nat)
    (headWriteFails : Bool) : State × Bool :=
  if s.head.isSome then (s, false)
  else
    let written :=
      match origin with
      | none => (s.origin, true)
      | some value => atomicCreate s.origin value
    if !written.2 then (s, false)
    else if headWriteFails then ({ s with origin := written.1 }, false)
    else
      let published := atomicCreate s.head head
      ({ s with origin := written.1, head := published.1 }, published.2)

/-- 修正案: ① head ② origin。origin の失敗では branch を残し、origin を残さない。 -/
def createBranchFixed (s : State) (origin : Option Nat) (head : Nat)
    (originWriteFails : Bool) : State × Bool :=
  if s.head.isSome then (s, false)
  else
    let published := atomicCreate s.head head
    if !published.2 then (s, false)
    else if originWriteFails then ({ s with head := published.1 }, true)
    else
      let written :=
        match origin with
        | none => (s.origin, true)
        | some value => atomicCreate s.origin value
      ({ s with head := published.1, origin := written.1 }, true)

/-! ### 実装の観測 -/

/-- head の書込みが失敗すると、head の無い branch の origin だけが残る。 -/
theorem partial_failure_leaves_an_orphan_origin :
    (createBranchCode ⟨none, none⟩ (some 101) 101 true).1 = ⟨none, some 101⟩ := by decide

/-- その状態は「branch が存在しないのに origin が返る」として観測できる。 -/
theorem orphan_origin_is_observable :
    (createBranchCode ⟨none, none⟩ (some 101) 101 true).1.head = none ∧
    (createBranchCode ⟨none, none⟩ (some 101) 101 true).1.origin = some 101 := by decide

/-- 同じ名前での再作成は、origin の atomic_create が AlreadyExists で失敗して通らない。 -/
theorem orphan_origin_blocks_the_retry :
    (createBranchCode ⟨none, some 101⟩ (some 101) 101 false).2 = false := by decide

/-- origin を渡さない再作成は通るが、残った origin が古い anchor として残る。 -/
theorem stale_origin_anchors_a_different_head :
    (createBranchCode ⟨none, some 101⟩ none 102 false).1 = ⟨some 102, some 101⟩ := by decide

/-- `records_for_causal_branch` は origin.computation を current の初期値にする
    (repository.rs:744-747) ので、head と anchor が食い違うと先頭 Record で落ちる。 -/
def causalAccepts (originAnchor : Option Nat) (firstBefore : Nat) : Bool :=
  match originAnchor with
  | none => true
  | some anchor => anchor = firstBefore

theorem stale_origin_breaks_the_first_record :
    causalAccepts (some 101) 102 = false := by decide

theorem branch_without_origin_replays_from_its_first_record (x : Nat) :
    causalAccepts none x = true := rfl

/-! ### 修正案の性質 -/

/-- origin の書込みが失敗しても、orphan な origin は残らない。 -/
theorem fixed_failure_leaves_no_orphan_origin :
    (createBranchFixed ⟨none, none⟩ (some 101) 101 true).1 = ⟨some 101, none⟩ := by decide

/-- 実装側の途中失敗では、branch は存在しない (名前だけが使えなくなる)。 -/
theorem code_failure_publishes_no_branch :
    (createBranchCode ⟨none, none⟩ (some 101) 101 true).1.head = none := by decide

/-- 修正案の途中失敗では branch が存在するので、利用者はそのまま使える。 -/
theorem fixed_failure_still_publishes_the_branch :
    (createBranchFixed ⟨none, none⟩ (some 101) 101 true).1.head = some 101 := by decide

/-- branch が存在するなら origin は再作成の対象にならない (origin 失敗は branch を
    残さないので、作成済み branch の origin を後から壊す経路が無い)。 -/
theorem fixed_success_publishes_both :
    (createBranchFixed ⟨none, none⟩ (some 101) 101 false).1
      = ⟨some 101, some 101⟩ := by decide

end BranchOriginOrder

/-! ## 28. services/netd/src/egress/gate.rs :: IPv4-embedded IPv6 deny ranges -/
namespace EgressCidr

inductive Fam where
  | v4
  | v6
  deriving DecidableEq, Repr

structure Addr where
  fam : Fam
  val : Nat
  deriving DecidableEq, Repr

structure VNet where
  fam : Fam
  base : Nat
  size : Nat
  deriving DecidableEq, Repr

def VNet.contains (n : VNet) (a : Addr) : Bool :=
  if n.fam = a.fam then
    (if n.base ≤ a.val then (if a.val < n.base + n.size then true else false) else false)
  else false

theorem VNet.contains_of (n : VNet) (a : Addr) (hf : n.fam = a.fam)
    (h1 : n.base ≤ a.val) (h2 : a.val < n.base + n.size) : n.contains a = true := by
  simp [VNet.contains, hf, h1, h2]

/-- gate.rs の DENIED_RANGES をそのまま区間へ展開したもの。 -/
def denied : List VNet :=
  [ ⟨.v4, 0, 2 ^ 24⟩                       -- 0.0.0.0/8
  , ⟨.v4, 0x0A000000, 2 ^ 24⟩              -- 10.0.0.0/8
  , ⟨.v4, 0x64400000, 2 ^ 22⟩              -- 100.64.0.0/10
  , ⟨.v4, 0x7F000000, 2 ^ 24⟩              -- 127.0.0.0/8
  , ⟨.v4, 0xA9FE0000, 2 ^ 16⟩              -- 169.254.0.0/16
  , ⟨.v4, 0xAC100000, 2 ^ 20⟩              -- 172.16.0.0/12
  , ⟨.v4, 0xC0000000, 2 ^ 8⟩               -- 192.0.0.0/24
  , ⟨.v4, 0xC0000200, 2 ^ 8⟩               -- 192.0.2.0/24
  , ⟨.v4, 0xC0A80000, 2 ^ 16⟩              -- 192.168.0.0/16
  , ⟨.v4, 0xC6120000, 2 ^ 17⟩              -- 198.18.0.0/15
  , ⟨.v4, 0xC6336400, 2 ^ 8⟩               -- 198.51.100.0/24
  , ⟨.v4, 0xCB007100, 2 ^ 8⟩               -- 203.0.113.0/24
  , ⟨.v4, 0xE0000000, 2 ^ 28⟩              -- 224.0.0.0/4
  , ⟨.v4, 0xF0000000, 2 ^ 28⟩              -- 240.0.0.0/4
  , ⟨.v6, 0, 1⟩                            -- ::/128
  , ⟨.v6, 1, 1⟩                            -- ::1/128
  , ⟨.v6, 0xFFFF * 2 ^ 32, 2 ^ 32⟩         -- ::ffff:0:0/96
  , ⟨.v6, 0x64FF9B * 2 ^ 96, 2 ^ 32⟩       -- 64:ff9b::/96
  , ⟨.v6, 0xFC00 * 2 ^ 112, 2 ^ 121⟩       -- fc00::/7
  , ⟨.v6, 0xFE80 * 2 ^ 112, 2 ^ 118⟩       -- fe80::/10
  , ⟨.v6, 0xFF00 * 2 ^ 112, 2 ^ 120⟩       -- ff00::/8
  ]

/-- deny が1つも当たらなければ許可 (check_addr の否定)。 -/
def allowed (l : List VNet) (a : Addr) : Bool := !(l.any (fun n => n.contains a))

/-! ### IPv4 を埋め込む IPv6 形式 -/

/-- ::ffff:a.b.c.d (IPv4-mapped) -/
def mapped (a : Nat) : Addr := ⟨.v6, 0xFFFF * 2 ^ 32 + a⟩

/-- 2002:a.b.c.d:: (6to4, RFC 3056) -/
def sixToFour (a : Nat) : Addr := ⟨.v6, 0x2002 * 2 ^ 112 + a * 2 ^ 80⟩

/-- ::a.b.c.d (IPv4-compatible, RFC 4291 で廃止) -/
def compat (a : Nat) : Addr := ⟨.v6, a⟩

/-- 64:ff9b::a.b.c.d (well-known NAT64, RFC 6052) -/
def nat64 (a : Nat) : Addr := ⟨.v6, 0x64FF9B * 2 ^ 96 + a⟩

/-- 64:ff9b:1:::a.b.c.d 相当 (local-use NAT64, RFC 8215) -/
def nat64Local (a : Nat) : Addr := ⟨.v6, 0x64FF9B0001 * 2 ^ 80 + a⟩

/-! ### 観測 -/

theorem v4_loopback_denied : allowed denied ⟨.v4, 0x7F000001⟩ = false := by decide

/-- 既知の健全性: IPv4-mapped のループバックは拒否される。 -/
theorem mapped_loopback_denied : allowed denied (mapped 0x7F000001) = false := by decide

/-- 既知の健全性: well-known NAT64 は /96 全体が拒否される。 -/
theorem wellknown_nat64_denied : allowed denied (nat64 0x7F000001) = false := by decide

/-- IPv4-compatible 形式は ::/128 と ::1/128 の外側なので通る。 -/
theorem compat_loopback_allowed_BUG : allowed denied (compat 0x7F000001) = true := by decide

/-- 6to4 は 2002::/16 が deny list に無いので通る (127.0.0.1)。 -/
theorem sixToFour_loopback_allowed_BUG : allowed denied (sixToFour 0x7F000001) = true := by decide

/-- 6to4 経由の link-local (169.254.169.254)。 -/
theorem sixToFour_linklocal_allowed_BUG : allowed denied (sixToFour 0xA9FEA9FE) = true := by decide

/-- local-use NAT64 経由のループバック。 -/
theorem nat64_local_loopback_allowed_BUG : allowed denied (nat64Local 0x7F000001) = true := by decide

/-- 「IPv4 を埋め込む標準形式はすべて拒否する」という仕様。 -/
def Spec (l : List VNet) : Prop :=
  (∀ a, a < 2 ^ 32 → allowed l (compat a) = false) ∧
  (∀ a, a < 2 ^ 32 → allowed l (sixToFour a) = false) ∧
  (∀ a, a < 2 ^ 32 → allowed l (mapped a) = false) ∧
  (∀ a, a < 2 ^ 32 → allowed l (nat64Local a) = false)

/-- 出荷中の deny list はこの仕様を満たさない。 -/
theorem spec_fails_for_shipped_list : ¬ Spec denied := by
  intro h
  have hc := h.1 0x7F000001 (by decide)
  rw [compat_loopback_allowed_BUG] at hc
  exact absurd hc (by decide)

/-! ### 修正案: ::/96, 2002::/16, 64:ff9b:1::/48 を追加する -/

def deniedFixed : List VNet :=
  ⟨.v6, 0, 2 ^ 32⟩                    -- ::/96 (IPv4-compatible)
  :: ⟨.v6, 0x2002 * 2 ^ 112, 2 ^ 112⟩ -- 2002::/16 (6to4)
  :: ⟨.v6, 0x64FF9B0001 * 2 ^ 80, 2 ^ 80⟩ -- 64:ff9b:1::/48 (local-use NAT64)
  :: denied

theorem any_of_mem {p : VNet → Bool} {n : VNet} {l : List VNet}
    (hm : n ∈ l) (hp : p n = true) : l.any p = true := by
  induction l with
  | nil => simp at hm
  | cons x xs ih =>
      rw [List.any_cons]
      rcases List.mem_cons.mp hm with rfl | hmem
      · simp [hp]
      · have h := ih hmem
        simp [h]

theorem allowed_false_of_contains {n : VNet} {l : List VNet} (hm : n ∈ l) {a : Addr}
    (hc : n.contains a = true) : allowed l a = false := by
  have h : l.any (fun m => m.contains a) = true := any_of_mem hm hc
  simp [allowed, h]

theorem compat_contained (a : Nat) (h : a < 2 ^ 32) :
    (⟨.v6, 0, 2 ^ 32⟩ : VNet).contains (compat a) = true := by
  apply VNet.contains_of
  · rfl
  · simp
  · simpa [compat] using h

theorem sixToFour_contained (a : Nat) (h : a < 2 ^ 32) :
    (⟨.v6, 0x2002 * 2 ^ 112, 2 ^ 112⟩ : VNet).contains (sixToFour a) = true := by
  apply VNet.contains_of
  · rfl
  · simp [sixToFour]
  · simp [sixToFour]; omega

theorem mapped_contained (a : Nat) (h : a < 2 ^ 32) :
    (⟨.v6, 0xFFFF * 2 ^ 32, 2 ^ 32⟩ : VNet).contains (mapped a) = true := by
  apply VNet.contains_of
  · rfl
  · simp [mapped]
  · simp [mapped]; omega

theorem nat64Local_contained (a : Nat) (h : a < 2 ^ 32) :
    (⟨.v6, 0x64FF9B0001 * 2 ^ 80, 2 ^ 80⟩ : VNet).contains (nat64Local a) = true := by
  apply VNet.contains_of
  · rfl
  · simp [nat64Local]
  · simp [nat64Local]; omega

theorem fixed_compat_denied (a : Nat) (h : a < 2 ^ 32) :
    allowed deniedFixed (compat a) = false :=
  allowed_false_of_contains (by simp [deniedFixed, denied]) (compat_contained a h)

theorem fixed_sixToFour_denied (a : Nat) (h : a < 2 ^ 32) :
    allowed deniedFixed (sixToFour a) = false :=
  allowed_false_of_contains (by simp [deniedFixed, denied]) (sixToFour_contained a h)

theorem fixed_mapped_denied (a : Nat) (h : a < 2 ^ 32) :
    allowed deniedFixed (mapped a) = false :=
  allowed_false_of_contains (by simp [deniedFixed, denied]) (mapped_contained a h)

theorem fixed_nat64_local_denied (a : Nat) (h : a < 2 ^ 32) :
    allowed deniedFixed (nat64Local a) = false :=
  allowed_false_of_contains (by simp [deniedFixed, denied]) (nat64Local_contained a h)

/-- 修正案は仕様を満たす。 -/
theorem fixed_spec : Spec deniedFixed :=
  ⟨fixed_compat_denied, fixed_sixToFour_denied, fixed_mapped_denied, fixed_nat64_local_denied⟩

end EgressCidr

/-! ## 29. services/netd/src/egress/policy.rs :: interleaved budget consume -/
namespace BudgetInterleave

structure Budget where
  used : Nat
  exhausted : Bool
  deriving DecidableEq, Repr

/-- ① `fetch_update`: used を加算し、加算前の previous を返す。 -/
def fetch (b : Budget) (bytes : Nat) : Budget × Nat :=
  (⟨b.used + bytes, b.exhausted⟩, b.used)

/-- ② 自分の加算が上限を超えたら flag を立てる。 -/
def flag (limit : Nat) (b : Budget) (previous bytes : Nat) : Budget :=
  if limit < previous + bytes then ⟨b.used, true⟩ else b

/-- ③ `exhausted()` を読んで可否を返す。 -/
def admit (b : Budget) : Bool := !b.exhausted

/-- 他と競合しない consume。 -/
def consume (limit : Nat) (b : Budget) (bytes : Nat) : Budget × Bool :=
  let fetched := fetch b bytes
  let flagged := flag limit fetched.1 fetched.2 bytes
  (flagged, admit flagged)

/-! ### 過剰許可は起きない (否定) -/

/-- 自分が上限内で、他の接続がまだ flag を立てていなければ必ず受理される。 -/
theorem consume_fits (limit : Nat) (b : Budget) (bytes : Nat)
    (hflag : b.exhausted = false) (hfit : b.used + bytes ≤ limit) :
    (consume limit b bytes).2 = true := by
  have hnot : ¬ limit < b.used + bytes := Nat.not_lt.mpr hfit
  simp [consume, fetch, flag, admit, hflag, hnot]

/-- 自分の加算が上限を超えるなら flag が立ち、その consume は拒否される。 -/
theorem over_limit_refused (limit : Nat) (b : Budget) (bytes : Nat)
    (hover : limit < b.used + bytes) : (consume limit b bytes).2 = false := by
  simp [consume, fetch, flag, admit, hover]

/-- 受理された consume は加算後の used も上限内に収まる。 -/
theorem admitted_stays_within_limit (limit : Nat) (b : Budget) (bytes : Nat)
    (h : (consume limit b bytes).2 = true) : b.used + bytes ≤ limit := by
  by_cases hlt : limit < b.used + bytes
  · exact absurd (over_limit_refused limit b bytes hlt) (by simp [h])
  · omega

/-- used は単調。受理された consume の合計が上限を超えないのはこの性質による
    (加算前の値が「それまでの全試行の累積」なので、受理された時点で累積 ≤ 上限)。 -/
theorem used_monotone (limit : Nat) (b : Budget) (bytes : Nat) :
    b.used ≤ (consume limit b bytes).1.used := by
  have hused : (consume limit b bytes).1.used = b.used + bytes := by
    by_cases h : limit < b.used + bytes
    · simp [consume, fetch, flag, h]
    · simp [consume, fetch, flag, h]
  omega

/-! ### 上限内の consume が他接続の flag で拒否される (候補) -/

/-- used=9, limit=10 から始まる。 -/
def b0 : Budget := ⟨9, false⟩

/-- 単独なら 9+1=10 ≤ 10 で受理される。 -/
theorem one_byte_alone_is_admitted : (consume 10 b0 1).2 = true := by decide

/-- 5 バイトを先に consume した接続があると flag が立ち、同じ 1 バイトが拒否される。 -/
theorem one_byte_after_a_larger_consume_is_refused :
    (consume 10 b0 5).1.exhausted = true ∧
    admit (flag 10 (consume 10 b0 5).1 9 1) = false := by decide

/-- 同じ 1 バイトの読み取りが、実行順だけで受理/拒否に分かれる。 -/
theorem same_consume_two_outcomes :
    (consume 10 b0 1).2 = true ∧
    admit (flag 10 (consume 10 b0 5).1 9 1) = false := by decide

/-- 拒否された側の `used` も加算済みなので、累積は上限を超えたまま残る。 -/
theorem refused_bytes_are_still_counted :
    (consume 10 b0 5).1.used = 14 := by decide

end BudgetInterleave


/-! ## 30. lib/runtime-attempt/src/journal.rs :: attempt reserve / UNKNOWN barrier -/
namespace HostedAttemptJournal

inductive AttemptState where
  | notStarted
  | started
  | finished
  deriving DecidableEq, Repr

structure Record where
  attemptId : Nat
  state : AttemptState
  deriving DecidableEq, Repr

abbrev Journal := List Record

inductive Refusal where
  | alreadyStarted (state : AttemptState)
  | unknown (attemptId : Nat)
  deriving DecidableEq, Repr

def findAttempt : Journal → Nat → Option Record
  | [], _ => none
  | r :: rs, aid => if r.attemptId == aid then some r else findAttempt rs aid

def findStarted : Journal → Option Record
  | [] => none
  | r :: rs => if r.state == AttemptState.started then some r else findStarted rs

/-- The Rust reserve, error branches first. -/
def reserve (j : Journal) (aid : Nat) : Except Refusal Journal :=
  match findAttempt j aid, findStarted j with
  | some r, _ => .error (.alreadyStarted r.state)
  | none, some r => .error (.unknown r.attemptId)
  | none, none => .ok (j ++ [⟨aid, AttemptState.notStarted⟩])

theorem findAttempt_append_self (j : Journal) (aid : Nat) (st : AttemptState)
    (h : findAttempt j aid = none) : findAttempt (j ++ [⟨aid, st⟩]) aid = some ⟨aid, st⟩ := by
  induction j with
  | nil => simp [findAttempt]
  | cons r rs ih =>
      by_cases hr : r.attemptId == aid
      · simp [findAttempt, hr] at h
      · have hrs : findAttempt rs aid = none := by simpa [findAttempt, hr] using h
        simp [findAttempt, hr]
        exact ih hrs

theorem reserve_ok_appends (j : Journal) (aid : Nat) (j' : Journal)
    (h : reserve j aid = .ok j') : j' = j ++ [⟨aid, AttemptState.notStarted⟩] := by
  unfold reserve at h
  repeat (split at h)
  all_goals simp_all

theorem started_blocks_every_attempt (j : Journal) (s : Record)
    (hmem : findStarted j = some s) (aid : Nat) : ¬ ∃ j', reserve j aid = .ok j' := by
  rintro ⟨j', hj⟩
  unfold reserve at hj
  repeat (split at hj)
  all_goals simp_all

theorem reserve_twice_is_refused (j : Journal) (aid : Nat) (j₁ : Journal)
    (h : reserve j aid = .ok j₁) : ∃ e, reserve j₁ aid = .error e := by
  have hnone : findAttempt j aid = none := by
    unfold reserve at h
    repeat (split at h) <;> simp_all
  have h₁ := reserve_ok_appends j aid j₁ h
  subst h₁
  refine ⟨Refusal.alreadyStarted AttemptState.notStarted, ?_⟩
  unfold reserve
  rw [findAttempt_append_self j aid AttemptState.notStarted hnone]

/-- The barrier's scope is one journal.  A delivery that lands on a fresh work
  root (another Runner, or a wiped work root) consults no history, so the very
  same attempt id starts again.  journal.rs:14-16 says "not a retry, not another
  Derivation, not a redelivery", which is true per work root only. -/
theorem fresh_journal_admits (aid : Nat) :
    reserve [] aid = .ok [⟨aid, AttemptState.notStarted⟩] := by
  unfold reserve findAttempt findStarted
  simp

theorem started_journal_refuses_same (aid : Nat) :
    reserve [⟨aid, AttemptState.started⟩] aid
      = .error (.alreadyStarted AttemptState.started) := by
  unfold reserve findAttempt findStarted
  simp

theorem started_journal_refuses_other (aid other : Nat) (h : other ≠ aid) :
    reserve [⟨aid, AttemptState.started⟩] other = .error (.unknown aid) := by
  have h' : ¬(aid = other) := fun e => h e.symm
  unfold reserve findAttempt findStarted
  simp [findAttempt, h']

end HostedAttemptJournal

/-! ## 31. apps/connected-realization-worker/src/runtime_launch/lease.rs :: execution authorization -/
namespace HostedExecutionAuthorization

structure Auth where
  /-- policy_generation: must not change across a renewal. -/
  policyGen : Nat
  /-- authorization_generation: must strictly advance. -/
  authGen : Nat
  /-- expires_at - server_time, in seconds. -/
  ttl : Nat
  /-- renew_after_secs. -/
  renewAfter : Nat
  deriving DecidableEq, Repr

structure Monitor where
  policyGen : Nat
  authGen : Nat
  deadline : Nat
  renewAt : Nat
  confirmed : Bool
  deriving DecidableEq, Repr

def maxTtl : Nat := 3600

/-- authorization_window's accepted range. -/
def windowOk (a : Auth) : Prop :=
  a.ttl > 0 ∧ a.ttl ≤ maxTtl ∧ a.renewAfter > 0 ∧ a.renewAfter < a.ttl

instance windowOkDecidable (a : Auth) : Decidable (windowOk a) := by
  unfold windowOk
  infer_instance

def freshOk (a : Auth) : Prop := windowOk a ∧ a.policyGen > 0 ∧ a.authGen > 0

instance freshOkDecidable (a : Auth) : Decidable (freshOk a) := by
  unfold freshOk
  infer_instance

/-- ExecutionAuthorizationMonitor::new.  renew_at is "now", so the first
  confirmation always runs before anything is materialized. -/
def fresh (a : Auth) (now : Nat) : Option Monitor :=
  if freshOk a then some ⟨a.policyGen, a.authGen, now + a.ttl, now, false⟩ else none

/-- ExecutionAuthorizationMonitor::apply's acceptance condition. -/
def applyOk (m : Monitor) (r : Auth) (s t : Nat) : Prop :=
  s ≤ t ∧ t < m.deadline ∧ r.policyGen = m.policyGen ∧ r.authGen > m.authGen ∧ windowOk r

instance applyOkDecidable (m : Monitor) (r : Auth) (s t : Nat) : Decidable (applyOk m r s t) := by
  unfold applyOk
  infer_instance

/-- ExecutionAuthorizationMonitor::apply. -/
def apply (m : Monitor) (r : Auth) (s t : Nat) : Option Monitor :=
  if applyOk m r s t then
    some ⟨r.policyGen, r.authGen, s + r.ttl, s + r.renewAfter, true⟩
  else none

/-- ExecutionAuthorizationMonitor::defer_retry, verbatim (min with the deadline). -/
def deferRetry (m : Monitor) (now delay : Nat) : Monitor :=
  { m with renewAt := if now + delay ≤ m.deadline then now + delay else m.deadline }

/-- The gate refresh_execution_authorization checks first. -/
def alive (m : Monitor) (now : Nat) : Prop := now < m.deadline

theorem fresh_ttl_bounded (a : Auth) (now : Nat) (m : Monitor)
    (h : fresh a now = some m) : m.deadline = now + a.ttl ∧ a.ttl ≤ maxTtl := by
  unfold fresh at h
  split at h
  · rename_i hc
    obtain ⟨hok, -, -⟩ := hc
    obtain ⟨-, hle, -, -⟩ := hok
    simp only [Option.some.injEq] at h
    subst h
    exact ⟨rfl, hle⟩
  · simp at h

theorem apply_advances_generation (m m' : Monitor) (r : Auth) (s t : Nat)
    (h : apply m r s t = some m') : r.authGen > m.authGen ∧ m'.authGen = r.authGen := by
  unfold apply at h
  split at h
  · rename_i hc
    obtain ⟨-, -, -, hgen, -⟩ := hc
    exact ⟨hgen, by simp only [Option.some.injEq] at h; subst h; rfl⟩
  · simp at h

theorem apply_keeps_policy (m m' : Monitor) (r : Auth) (s t : Nat)
    (h : apply m r s t = some m') : m'.policyGen = m.policyGen := by
  unfold apply at h
  split at h
  · rename_i hc
    obtain ⟨-, -, hpol, -, -⟩ := hc
    simp only [Option.some.injEq] at h
    subst h
    exact hpol
  · simp at h

/-- A renewal must arrive before the previous deadline: an expired window is
  never extended by a late answer. -/
theorem apply_requires_unexpired (m m' : Monitor) (r : Auth) (s t : Nat)
    (h : apply m r s t = some m') : t < m.deadline := by
  unfold apply at h
  split at h
  · rename_i hc
    obtain ⟨-, hdl, -, -, -⟩ := hc
    exact hdl
  · simp at h

/-- No local clock enters the new deadline: it is request start + the
  Coordinator's ttl. -/
theorem apply_no_local_clock (m m' : Monitor) (r : Auth) (s t : Nat)
    (h : apply m r s t = some m') : m'.deadline = s + r.ttl := by
  unfold apply at h
  split at h
  · simp only [Option.some.injEq] at h
    subst h
    rfl
  · simp at h

/-- A failed renewal grants no time. -/
theorem defer_retry_never_extends (m : Monitor) (now delay : Nat) :
    (deferRetry m now delay).deadline = m.deadline
    ∧ (deferRetry m now delay).renewAt ≤ m.deadline := by
  unfold deferRetry
  split <;> simp_all <;> omega

/-- Expiry is absorbing: no later renewal makes an expired monitor alive again,
  because refresh checks alive before calling apply. -/
theorem expiry_is_absorbing (m : Monitor) (t₁ t₂ : Nat)
    (hle : t₁ ≤ t₂) (h : ¬ alive m t₁) : ¬ alive m t₂ := by
  unfold alive at *
  omega

/-- A *successful* renewal can move the deadline backwards: the new window is
  anchored at the request start, not at the previous deadline.  Fail-closed, so
  this is a liveness note, not a safety bug. -/
theorem renewal_can_shorten :
    ∃ (m m' : Monitor) (r : Auth) (s t : Nat),
      apply m r s t = some m' ∧ m'.deadline < m.deadline := by
  refine ⟨⟨1, 1, 100, 0, true⟩, ⟨1, 2, 98, 96, true⟩, ⟨1, 2, 3, 1⟩, 95, 99, ?_, ?_⟩
  · unfold apply applyOk windowOk
    decide
  · decide

end HostedExecutionAuthorization

/-! ## 32. apps/connected-realization-worker/src/runtime_launch/lease.rs :: lease lifetime -/
namespace HostedLeaseLifetime

structure Lease where
  /-- expires_at - receipt time, in seconds. -/
  expirySecs : Nat
  maxDuration : Option Nat
  /-- Whether an execution_authorization was attached. -/
  authorization : Bool
  deriving DecidableEq, Repr

def defaultCap : Nat := 3600

/-- lease.rs:800-809, verbatim control flow. -/
def maximumLifetime (l : Lease) : Option Nat :=
  match l.maxDuration, l.authorization with
  | some s, _ => some s
  | none, true => none
  | none, false => some defaultCap

/-- validate_lease: "lease expired before execution". -/
def admitted (l : Lease) : Bool := l.expirySecs > 0

/-- What the Run may consume, from the lease alone. -/
def runsUpTo (l : Lease) (t : Nat) : Bool :=
  match maximumLifetime l with
  | none => true
  | some s => t ≤ s

/-- RUNTIME_LAUNCH_MAX_DURATION_SECS' doc (lease.rs:60-63): "omitting the field
  preserves the ordinary one-hour safety cap".  True without an execution
  authorization. -/
theorem omitted_duration_keeps_cap (n : Nat) :
    maximumLifetime ⟨n, none, false⟩ = some defaultCap := rfl

/-- ... and false with one.  The const's doc sentence and the field's own doc
  ("independent of an optional maximum-duration policy") disagree. -/
theorem omitted_duration_drops_cap (n : Nat) :
    maximumLifetime ⟨n, none, true⟩ = none := rfl

theorem cap_omitted_BUG :
    ∃ l : Lease, l.maxDuration = none ∧ maximumLifetime l ≠ some defaultCap :=
  ⟨⟨1, none, true⟩, rfl, by decide⟩

/-- expires_at bounds admission only.  A lease whose expires_at is one second
  away still authorizes an hour of execution. -/
theorem expiry_does_not_bound_the_run :
    ∃ (l : Lease) (t : Nat),
      admitted l = true ∧ runsUpTo l t = true ∧ l.expirySecs < t :=
  ⟨⟨1, none, false⟩, 3600, by decide, by decide, by decide⟩

theorem expired_lease_not_admitted : admitted ⟨0, none, false⟩ = false := by decide

/-- The fix the two docs imply: the runnable window is the smaller of the
  lease's own expiry and the lease-owned cap. -/
def fixedLifetime (l : Lease) : Nat :=
  match maximumLifetime l with
  | none => if l.expirySecs < defaultCap then l.expirySecs else defaultCap
  | some s => if l.expirySecs < s then l.expirySecs else s

theorem fixed_bounded_by_expiry (l : Lease) : fixedLifetime l ≤ l.expirySecs := by
  unfold fixedLifetime
  split <;> split <;> omega

end HostedLeaseLifetime

/-! ## 33. apps/connected-realization-worker/src/lib.rs :: failure wire codes -/
namespace HostedFailureCode

inductive Cause where
  | malformedLease
  | specDigestMismatch
  | leaseExpired
  | runtimeAdmission
  | executionFailure
  | maxLifetimeElapsed
  | stateCommitFailed
  | authorizationEnded
  deriving DecidableEq, Repr

def realizationFailed : Nat := 1
def authorizationEnded : Nat := 2

/-- lib.rs:2215-2221: storage -> its own code, message containing
  "execution authorization" -> authorization_ended, everything else ->
  realization_failed. -/
def wireCode : Cause → Nat
  | .authorizationEnded => authorizationEnded
  | _ => realizationFailed

/-- What AGENTS.md asks: format invalid, admission failure, execution failure
  and Contract failure must not be folded into one error. -/
def specCode : Cause → Nat
  | .malformedLease => 10
  | .specDigestMismatch => 11
  | .leaseExpired => 12
  | .runtimeAdmission => 13
  | .executionFailure => 14
  | .maxLifetimeElapsed => 15
  | .stateCommitFailed => 16
  | .authorizationEnded => 17

theorem format_folds_into_execution :
    wireCode .malformedLease = wireCode .executionFailure := rfl

theorem digest_mismatch_folds_into_execution :
    wireCode .specDigestMismatch = wireCode .executionFailure := rfl

theorem expiry_is_reported_as_a_failure :
    wireCode .maxLifetimeElapsed = realizationFailed := rfl

theorem commit_failure_folds_into_execution :
    wireCode .stateCommitFailed = wireCode .executionFailure := rfl

/-- The property AGENTS.md states, refuted. -/
theorem distinct_causes_share_the_wire_code :
    ∃ a b : Cause, a ≠ b ∧ wireCode a = wireCode b :=
  ⟨.malformedLease, .executionFailure, by decide, rfl⟩

/-- Only "authorization ended" is separable. -/
theorem only_authorization_is_separable (c : Cause) (h : wireCode c ≠ realizationFailed) :
    c = .authorizationEnded := by
  cases c <;> simp_all [wireCode, realizationFailed, authorizationEnded]

/-- The fix: one code per class. -/
theorem spec_code_is_injective : Function.Injective specCode := by
  intro a b h
  cases a <;> cases b <;> simp_all [specCode]

end HostedFailureCode


/-! ## 34. lib/objects/src/portable_bundle.rs :: version/profile dispatch -/
namespace BundleDispatch

def profileV3 : String := "ato.portable-application/1"
def profileV4 : String := "ato.portable-application/2"

/-- bundle.rs::validate_shape sees only index.version. -/
def acceptsV2 (version : Nat) : Bool := version == 2

/-- portable_bundle.rs::validate_shape_and_payloads ties version to profile. -/
def acceptsPortable (version : Nat) (profile : Option String) : Bool :=
  match version with
  | 3 => profile == some profileV3
  | 4 => profile == some profileV4
  | _ => false

/-- v3 refuses the v4-only transport manifest and Instance snapshot. -/
def acceptsV3Shape (profile : Option String) (portability : Bool) (snapshot : Bool) : Bool :=
  acceptsPortable 3 profile && !portability && !snapshot

/-- v4 requires the dependency transport manifest. -/
def acceptsV4Shape (profile : Option String) (portability : Bool) : Bool :=
  acceptsPortable 4 profile && portability

theorem v2_and_portable_are_disjoint (version : Nat) (profile : Option String) :
    acceptsV2 version = true -> acceptsPortable version profile = false := by
  intro h
  have hv : version = 2 := by simpa [acceptsV2] using h
  simp [acceptsPortable, hv]

theorem v3_requires_the_v3_profile (profile : Option String) :
    acceptsPortable 3 profile = true -> profile = some profileV3 := by
  intro h; simpa [acceptsPortable] using h

theorem v4_requires_the_v4_profile (profile : Option String) :
    acceptsPortable 4 profile = true -> profile = some profileV4 := by
  intro h; simpa [acceptsPortable] using h

theorem other_versions_are_refused (version : Nat) (profile : Option String)
    (h3 : version ≠ 3) (h4 : version ≠ 4) :
    acceptsPortable version profile = false := by
  unfold acceptsPortable
  split
  . contradiction
  . contradiction
  . rfl

theorem unknown_version_is_unsupported (version : Nat)
    (hv3 : version ≠ 3) (hv4 : version ≠ 4) :
    acceptsPortable version none = false :=
  other_versions_are_refused version none hv3 hv4

theorem v3_refuses_instance_snapshot (profile : Option String) (portability : Bool) :
    acceptsV3Shape profile portability true = false := by
  simp [acceptsV3Shape]

theorem v4_refuses_without_the_transport_manifest (profile : Option String) :
    acceptsV4Shape profile false = false := by
  simp [acceptsV4Shape]

/-- Positive controls: a matching pair is accepted, so the model is not
    vacuously strict. -/
theorem v3_accepts_its_own_profile :
    acceptsV3Shape (some profileV3) false false = true := by decide

theorem v4_accepts_its_own_profile :
    acceptsV4Shape (some profileV4) true = true := by decide

/-- Cross-labelling is refused in both directions. -/
theorem v3_shape_refuses_the_v4_profile :
    acceptsV3Shape (some profileV4) false false = false := by decide

theorem v4_shape_refuses_the_v3_profile :
    acceptsV4Shape (some profileV3) true = false := by decide

end BundleDispatch

/-! ## 35. lib/objects/src/portable_bundle.rs :: sorted arrays and payload digests -/
namespace PortableShape

/-- portable_bundle.rs::is_strictly_sorted, as a boolean over Nat references
    (the real code compares the canonical sha256 references, whose byte order is
    the digest order). -/
def strictlySorted : List Nat -> Bool
  | [] => true
  | [_] => true
  | a :: b :: t => decide (a < b) && strictlySorted (b :: t)

theorem strictly_sorted_rejects_a_duplicate (a : Nat) (t : List Nat) :
    strictlySorted (a :: a :: t) = false := by
  simp [strictlySorted]

theorem strictly_sorted_orders_the_first_pair (a b : Nat) (t : List Nat) :
    strictlySorted (a :: b :: t) = true -> a < b := by
  intro h
  simp [strictlySorted] at h
  exact h.1

/-- PortableApplicationBundle::descriptor / payload_bytes binary-search the array
    that validate_shape_and_payloads has already required to be strictly sorted.
    This linear model is the specification those searches implement. -/
def search (x : Nat) : List Nat -> Option Nat
  | [] => none
  | a :: t => if a == x then some a else search x t

theorem search_complete (x : Nat) (l : List Nat) : x ∈ l -> search x l = some x := by
  intro h
  induction l with
  | nil => exact absurd h (by simp)
  | cons a t ih =>
    by_cases ha : a = x
    · subst ha; simp [search]
    · have hx : x ∈ t := by
        rcases List.mem_cons.mp h with h' | h'
        · exact absurd h'.symm ha
        · exact h'
      have hne : (a == x) = false := by simpa using ha
      simp [search, hne]
      exact ih hx

/-- With sortedness the found element is the searched one, so the search may not
    silently return a neighbour (the reason unsorted arrays are rejected). -/
theorem search_is_the_searched_element (x y : Nat) (l : List Nat)
    (hk : ∀ z, z ∈ l -> x = z -> False) (h : search x l = some y) : y = x := by
  induction l with
  | nil => simp [search] at h
  | cons a t ih =>
    by_cases ha : a = x
    · subst ha
      simp [search] at h
      exact h.symm
    · have hne : (a == x) = false := by simpa using ha
      simp [search, hne] at h
      exact ih (fun z hz hx => hk z (List.mem_cons_of_mem _ hz) hx) h

/-- portable_bundle.rs accepts a payload only when both the declared length and
    the SHA-256 digest match. -/
theorem accepted_payload_has_the_declared_length (len size digest actual : Nat)
    (h : ((len == size) && (actual == digest)) = true) : len = size := by
  simp only [Bool.and_eq_true] at h
  simpa using h.1

theorem accepted_payload_has_the_declared_digest (len size digest actual : Nat)
    (h : ((len == size) && (actual == digest)) = true) : actual = digest := by
  simp only [Bool.and_eq_true] at h
  simpa using h.2

theorem a_wrong_digest_is_refused (len size digest actual : Nat)
    (h : actual ≠ digest) :
    ((len == size) && (actual == digest)) = false := by
  have hd : (actual == digest) = false := by simpa using h
  simp [hd]

/-- The two inclusion checks validate_portable_application_closure performs
    (declared subset of reachable, reachable subset of declared) express set
    equality. -/
theorem both_inclusions_mean_equality (a b : List Nat)
    (hab : ∀ x, x ∈ a -> x ∈ b) (hba : ∀ x, x ∈ b -> x ∈ a) :
    ∀ x, x ∈ a ↔ x ∈ b :=
  fun x => ⟨hab x, hba x⟩

end PortableShape

/-! ## 36. lib/objects/src/bundle.rs :: v2 closure visited kinds -/
namespace BundleClosureV2

/-- Refs are natural numbers. residual x = some g means the object at x is a
    canonical computation object whose residual reference is g; the object at g
    may itself be a computation object. out x is what a registered
    ComputationReferences implementation emits as ObjectLink::Computation. -/
structure Store where
  residual : Nat -> Option Nat
  out : Nat -> List Nat

/-- bundle.rs::closure. One result map keyed by content reference serves both
    ObjectLink::Content and ObjectLink::Computation, and the Computation arm
    starts with: if result.contains_key(&content) { continue; } -/
def closureBug (fuel : Nat) (store : Store) (queue seen : List Nat) : List Nat :=
  match fuel with
  | 0 => seen
  | fuel + 1 =>
    match queue with
    | [] => seen
    | x :: rest =>
      if x ∈ seen then
        closureBug fuel store rest seen
      else
        match store.residual x with
        | some g => closureBug fuel store (rest ++ store.out x) (g :: x :: seen)
        | none => closureBug fuel store rest (x :: seen)

/-- The traversal the closure invariant needs: "already walked as a computation"
    and "already present as a content object" are separate sets, so a residual
    that happens to be another computation's content reference cannot hide that
    computation's own outgoing references. Returns (walked, content). -/
def closureFixed (fuel : Nat) (store : Store) (queue walked content : List Nat) :
    List Nat × List Nat :=
  match fuel with
  | 0 => (walked, content)
  | fuel + 1 =>
    match queue with
    | [] => (walked, content)
    | x :: rest =>
      if x ∈ walked then
        closureFixed fuel store rest walked content
      else
        match store.residual x with
        | some g =>
          closureFixed fuel store (rest ++ store.out x) (x :: walked) (g :: content)
        | none => closureFixed fuel store rest (x :: walked) content

def reachedFixed (fuel : Nat) (store : Store) (roots : List Nat) : List Nat :=
  let pair := closureFixed fuel store roots [] []
  pair.1 ++ pair.2

/-- Witness: object 0 has residual 1 and its extractor names computation 1;
    object 1 has residual 2. The true closure of 0 is {0, 1, 2}. -/
def bugResidual : Nat -> Option Nat
  | 0 => some 1
  | 1 => some 2
  | _ => none

def bugOut : Nat -> List Nat
  | 0 => [1]
  | _ => []

def bugStore : Store := { residual := bugResidual, out := bugOut }

theorem bug_store_shapes :
    bugStore.residual 0 = some 1 ∧ bugStore.residual 1 = some 2 := by decide

/-- The code's traversal never walks computation 1, because its content
    reference was already inserted as 0's residual. Object 2 (1's residual) is
    therefore absent although it is reachable. -/
theorem v2_closure_drops_a_reachable_object_BUG :
    (2 ∈ closureBug 8 bugStore [0] []) = false := by decide

theorem fixed_closure_keeps_it :
    (2 ∈ reachedFixed 8 bugStore [0]) = true := by decide

/-- import_bundle accepts exactly when the declared set and the traversed set are
    equal in both directions. -/
def bundleAccepts (declared reachable : List Nat) : Bool :=
  (reachable.all fun x => x ∈ declared) && (declared.all fun x => x ∈ reachable)

theorem the_short_closed_bundle_is_accepted :
    bundleAccepts [0, 1] (closureBug 8 bugStore [0] []) = true := by decide

/-- The invariant OBJECT_BUNDLE.md states for import ("complete closure"): the
    accepted closure is closed under the reference relation. -/
def closedOn (nodes : List Nat) (store : Store) (s : List Nat) : Bool :=
  nodes.all fun x =>
    if x ∈ s then
      (match store.residual x with
        | some g => g ∈ s
        | none => true) &&
      (store.out x).all fun y => y ∈ s
    else true

theorem accepted_bundle_is_not_closed_BUG :
    closedOn [0, 1, 2] bugStore (closureBug 8 bugStore [0] []) = false := by decide

theorem the_fixed_traversal_is_closed :
    closedOn [0, 1, 2] bugStore (reachedFixed 8 bugStore [0]) = true := by decide

theorem the_fixed_traversal_still_covers_the_buggy_one :
    (closureBug 8 bugStore [0] []).all (fun x => x ∈ reachedFixed 8 bugStore [0]) = true := by
  decide

end BundleClosureV2

/-! ## 37. lib/objects/src/portable_bundle.rs :: declared size bounds -/
namespace BundleBounds

/-- portable_bundle.rs::MAX_BUNDLE_BYTES. -/
def maxBundleBytes : Nat := 768 * 1024 * 1024
/-- portable_bundle.rs::MAX_DECODED_BYTES. -/
def maxDecodedBytes : Nat := 256 * 1024 * 1024
/-- the 512 MiB portable OCI transport bound the comment names. -/
def maxArchiveBytes : Nat := 512 * 1024 * 1024
/-- portable_bundle.rs::MAX_OCI_ARCHIVE_BASE64_BYTES. -/
def maxArchiveBase64 : Nat := maxArchiveBytes / 3 * 4 + 4

/-- canonical base64 length of n bytes. -/
def base64Len (n : Nat) : Nat := 4 * ((n + 2) / 3)

theorem archive_bound_is_the_base64_of_512_mib :
    maxArchiveBase64 = base64Len maxArchiveBytes := by decide

theorem a_full_archive_leaves_less_than_64_mib_of_base64 :
    maxArchiveBase64 + base64Len (64 * 1024 * 1024) > maxBundleBytes := by decide

theorem both_declared_maxima_do_not_fit :
    base64Len maxArchiveBytes + base64Len maxDecodedBytes > maxBundleBytes := by decide

theorem the_remaining_object_budget :
    maxBundleBytes - maxArchiveBase64 = 89478484 := by decide

/-- So an offline bundle that carries a maximum-size OCI archive cannot also
    carry a maximum-size object: the per-part limits are not jointly reachable
    and ensure_bundle_size refuses the envelope first. -/
theorem per_part_limits_are_not_jointly_attainable :
    maxArchiveBase64 + base64Len (64 * 1024 * 1024) > maxBundleBytes ∧
    maxArchiveBase64 < maxBundleBytes := by decide

end BundleBounds

/-! ## ato-api (cross-repo) :: resident Instance restart after a Runner recovery report

`src/services/always_on.ts` (`scheduleInstanceRestart`, `reconcileAlwaysOnInstances`),
`src/routes/runner_lease_state.ts` (`POST /v1/runner-leases/:id/recovery`) and
`src/services/network_authorization.ts` (fixed TCP bindings). Found on the self-hosted
Mail guest on 2026-11-20: after a host restart the resident Instance never came back.

The model keeps only what decides whether a new Run is launched: the policy row, the
live Run and who holds the fixed socket. Backoff delays and the healthy-interval reset
of the failure counter are abstracted away; they change when a launch happens, not
whether one happens. -/
namespace AlwaysOnRecovery

inductive Status where
  | idle | waiting | launching | starting | running | degraded
  deriving DecidableEq, Repr

structure World where
  status : Status
  lastRun : Option Nat
  healthy : Bool
  failures : Nat
  liveRun : Option Nat
  socket : Option Nat
  deriving DecidableEq, Repr

def maxFailures : Nat := 5

/-- The statuses in which a terminal report for `lastRun` counts as already scheduled. -/
def alreadyScheduled (includeStarting : Bool) : Status → Bool
  | .waiting | .launching | .degraded => true
  | .starting => includeStarting
  | _ => false

/-- `scheduleInstanceRestart`. `run = none` is the launch-failure call from the sweep. -/
def schedule (includeStarting : Bool) (w : World) (run : Option Nat) (count : Bool) : World :=
  let foreign := match run, w.lastRun with
    | some r, some l => r != l
    | _, _ => false
  let duplicate := match run with
    | some r => w.lastRun == some r && !w.healthy && alreadyScheduled includeStarting w.status
    | none => false
  if foreign || duplicate then w
  else if count && w.failures + 1 > maxFailures then
    { w with status := .degraded, failures := w.failures + 1, healthy := false }
  else
    { w with status := .waiting, failures := if count then w.failures + 1 else w.failures,
             healthy := false }

/-- The recovery report for a Run the Runner confirmed stopped. The three flags are the
    three behaviours that were missing: requeue the policy, treat `starting` as not yet
    scheduled, release the fixed socket. -/
def recover (requeue includeStarting release : Bool) (w : World) (r : Nat) : World :=
  let w := { w with liveRun := none }
  let w := if release && w.socket == some r then { w with socket := none } else w
  if requeue then schedule includeStarting w (some r) false else w

/-- One `reconcileAlwaysOnInstances` pass that would start Run `fresh`. -/
def sweep (includeStarting : Bool) (w : World) (fresh : Nat) : World :=
  if w.status != .waiting then w
  else if w.socket.isSome then schedule includeStarting w none true
  else { w with status := .starting, lastRun := some fresh, liveRun := some fresh }

/-- What the Coordinator knows just before a host restart: a Run that is starting or
    running, bound to the policy, holding the fixed socket or none. -/
def beforeRestart (w : World) (r : Nat) : Prop :=
  w.liveRun = some r ∧ w.lastRun = some r ∧
  (w.status = .running ∨ w.status = .starting) ∧
  (w.socket = none ∨ w.socket = some r)

instance (w : World) (r : Nat) : Decidable (beforeRestart w r) := by
  unfold beforeRestart; infer_instance

def relaunches (requeue includeStarting release : Bool) (w : World) (r fresh : Nat) : Bool :=
  (sweep includeStarting (recover requeue includeStarting release w r) fresh).liveRun.isSome

/-- BUG 1 (fixed in ato-api 5d8619578): the recovery report left the policy `running`
    with no Run, and the sweep only looks at `waiting`. -/
theorem recovery_strands_running_BUG :
    ∃ w r, beforeRestart w r ∧ relaunches false true true w r (r + 1) = false :=
  ⟨⟨.running, some 1, true, 0, some 1, none⟩, 1, by decide, by decide⟩

/-- BUG 2 (fixed in ato-api 2094897c2): a Run that ends before it was ever ready is
    read as a duplicate report, so the policy stays `starting` forever. -/
theorem unready_run_end_ignored_BUG :
    ∃ w r, beforeRestart w r ∧ relaunches true true true w r (r + 1) = false :=
  ⟨⟨.starting, some 1, false, 0, some 1, none⟩, 1, by decide, by decide⟩

/-- BUG 3 (fixed in ato-api f662bd5c6): the confirmed stop did not release the fixed
    socket, so every later launch is refused as `fixed_tcp_allocation_busy`. -/
theorem socket_blocks_first_relaunch_BUG :
    ∃ w r, beforeRestart w r ∧ relaunches true false false w r (r + 1) = false :=
  ⟨⟨.running, some 1, true, 0, some 1, some 1⟩, 1, by decide, by decide⟩

/-- BUG 3 is permanent, not a delay: while the socket stays held no number of sweeps
    ever starts a Run. -/
theorem held_socket_never_relaunches (includeStarting : Bool) (fresh : Nat) :
    ∀ (n : Nat) (w : World), w.liveRun = none → w.socket.isSome = true →
      (Nat.repeat (fun w => sweep includeStarting w fresh) n w).liveRun = none ∧
      (Nat.repeat (fun w => sweep includeStarting w fresh) n w).socket.isSome = true := by
  intro n
  induction n with
  | zero => intro w h1 h2; exact ⟨h1, h2⟩
  | succ n ih =>
    intro w h1 h2
    have := ih w h1 h2
    simp only [Nat.repeat]
    obtain ⟨a, b⟩ := this
    generalize Nat.repeat (fun w => sweep includeStarting w fresh) n w = v at a b
    unfold sweep schedule
    simp only [b]
    split
    · exact ⟨a, b⟩
    · simp only [Bool.false_or, ite_true]
      split
      · exact ⟨a, b⟩
      · split <;> exact ⟨a, b⟩

/-- HOLDS (the fix): with all three behaviours, the first sweep after a confirmed
    recovery starts a new Run, whatever the failure history. -/
theorem fixed_recovery_relaunches (w : World) (r fresh : Nat) (h : beforeRestart w r) :
    relaunches true false true w r fresh = true := by
  obtain ⟨hl, hr, hs, hk⟩ := h
  cases w with
  | mk status lastRun healthy failures liveRun socket =>
    simp only at hl hr hs hk
    subst hl hr
    rcases hs with hs | hs <;> rcases hk with hk | hk <;> subst hs hk <;>
      simp [relaunches, recover, schedule, sweep, alreadyScheduled]

/-- HOLDS: the restart caused by a host restart is not charged to the app. -/
theorem fixed_recovery_keeps_failures (w : World) (r : Nat) (h : beforeRestart w r) :
    (recover true false true w r).failures = w.failures := by
  obtain ⟨hl, hr, hs, hk⟩ := h
  cases w with
  | mk status lastRun healthy failures liveRun socket =>
    simp only at hl hr hs hk
    subst hl hr
    rcases hs with hs | hs <;> rcases hk with hk | hk <;> subst hs hk <;>
      simp [recover, schedule, alreadyScheduled]

/-- Mutation check of the specification: of the eight combinations of the three
    behaviours, exactly the fixed one restarts every resident Instance. Dropping any
    single fix is caught by this theorem, so none of the three is redundant. -/
theorem only_the_fixed_combination_is_live (requeue includeStarting release : Bool) :
    (∀ w r, beforeRestart w r → relaunches requeue includeStarting release w r (r + 1) = true) ↔
      (requeue = true ∧ includeStarting = false ∧ release = true) := by
  constructor
  · intro h
    have h1 := h ⟨.running, some 1, true, 0, some 1, none⟩ 1 (by decide)
    have h2 := h ⟨.starting, some 1, false, 0, some 1, none⟩ 1 (by decide)
    have h3 := h ⟨.running, some 1, true, 0, some 1, some 1⟩ 1 (by decide)
    revert h1 h2 h3
    cases requeue <;> cases includeStarting <;> cases release <;> decide
  · rintro ⟨rfl, rfl, rfl⟩ w r hw
    exact fixed_recovery_relaunches w r (r + 1) hw

end AlwaysOnRecovery

/-! ## ato-api (cross-repo) :: volume operation dispatch after a confirmed stop

`src/services/dynamic_state/volume_operations.ts` (`advanceVolumeOperation`, waiting
branch). Found on production on 2026-11-20: a checkpoint requested about ten seconds
after a stop failed `state_volume_runner_unavailable`; the same request succeeded fifty
seconds later.

A stopping Run gives its writer back before its lease ends, so for a while the resident
Runner's only slot is still held by the very Run the operation asked to stop. The model
keeps the operation status, who holds the writer, whether that lease is still open and
whether the Runner is durably usable (owner, pool, capability, active, not drained).
Heartbeat freshness is not modelled: an offline Runner still fails the operation. -/
namespace VolumeOperationDispatch

inductive Status where
  | waiting | running | failed
  deriving DecidableEq, Repr

inductive Writer where
  | free | app | operation
  deriving DecidableEq, Repr

structure World where
  status : Status
  writer : Writer
  appLeaseOpen : Bool
  runnerUsable : Bool
  deriving DecidableEq, Repr

/-- One `advanceVolumeOperation` call. `waitForSlot` is the fix: a full slot keeps the
    operation waiting instead of failing it. -/
def poll (waitForSlot : Bool) (w : World) : World :=
  if w.status != .waiting then w
  else if !w.runnerUsable then { w with status := .failed, writer := if w.writer == .operation then .free else w.writer }
  else if w.writer == .app then w
  else if w.appLeaseOpen then
    if waitForSlot then { w with writer := .operation }
    else { w with status := .failed, writer := .free }
  else { w with status := .running, writer := .operation }

inductive Event where
  | poll            -- the owner's poll, the cron or the Runner's poll
  | writerReleased  -- the stopping Run commits or releases its state
  | leaseEnded      -- the Runner reports the Run stopped
  | runnerRetired   -- the Runner is drained, archived or loses the capability
  deriving DecidableEq, Repr

def step (waitForSlot : Bool) (w : World) : Event → World
  | .poll => poll waitForSlot w
  | .writerReleased => if w.writer == .app then { w with writer := .free } else w
  | .leaseEnded => { w with appLeaseOpen := false }
  | .runnerRetired => { w with runnerUsable := false }

def run (waitForSlot : Bool) (w : World) : List Event → World
  | [] => w
  | e :: rest => run waitForSlot (step waitForSlot w e) rest

/-- A checkpoint requested while the App's Run is live on a usable Runner. -/
def requested : World := ⟨.waiting, .app, true, true⟩

/-- BUG (ato-api `fix/volume-checkpoint-export-races`): the poll that lands between the
    writer release and the end of the lease fails the operation for good, although the
    Runner is usable and the stop is completing normally. -/
theorem slot_of_stopping_run_fails_operation_BUG :
    (run false requested [.writerReleased, .poll, .leaseEnded, .poll]).status = .failed := by
  decide

/-- The failure is the timing of the poll alone: the same stop with the poll after the
    lease ended dispatches the operation. -/
theorem same_stop_later_poll_dispatches :
    (run false requested [.writerReleased, .leaseEnded, .poll]).status = .running := by
  decide

/-- HOLDS (the fix): on a Runner that stays usable no interleaving of polls with the
    stop's two reports fails the operation. -/
theorem fixed_never_fails_on_usable_runner (events : List Event)
    (h : ∀ e ∈ events, e ≠ .runnerRetired) :
    ∀ w, w.runnerUsable = true → w.status ≠ .failed →
      (run true w events).status ≠ .failed := by
  induction events with
  | nil => intro w _ hs; exact hs
  | cons e rest ih =>
    intro w hu hs
    have hrest : ∀ e ∈ rest, e ≠ .runnerRetired := fun e he => h e (List.mem_cons_of_mem _ he)
    have he : e ≠ .runnerRetired := h e (List.mem_cons_self ..)
    apply ih hrest
    · cases e <;> simp [step, poll] at * <;> (repeat' split) <;> simp_all
    · cases e <;> simp [step, poll] at * <;> (repeat' split) <;> simp_all

/-- HOLDS: once both reports of the stop arrived, the next poll dispatches. -/
theorem fixed_dispatches_after_stop (w : World)
    (hs : w.status = .waiting) (hu : w.runnerUsable = true)
    (hw : w.writer ≠ .app) (hl : w.appLeaseOpen = false) :
    (poll true w).status = .running := by
  cases w with
  | mk status writer appLeaseOpen runnerUsable =>
    simp only at hs hu hw hl
    subst hs hu hl
    cases writer <;> simp_all [poll]

/-- HOLDS: every order of the stop's reports and any number of early polls ends with
    the operation running. -/
theorem fixed_stop_then_checkpoint_runs :
    (run true requested [.poll, .writerReleased, .poll, .poll, .leaseEnded, .poll]).status = .running ∧
    (run true requested [.poll, .leaseEnded, .poll, .writerReleased, .poll]).status = .running := by
  decide

/-- HOLDS (negative case kept by the fix): a Runner that is durably unusable still
    fails the operation and gives the writer back. -/
theorem fixed_retired_runner_still_fails (w : World)
    (hs : w.status = .waiting) (hu : w.runnerUsable = false) :
    (poll true w).status = .failed ∧ (poll true w).writer ≠ .operation := by
  cases w with
  | mk status writer appLeaseOpen runnerUsable =>
    simp only at hs hu
    subst hs hu
    cases writer <;> simp [poll]

/-- HOLDS: while the previous Run still holds the writer nothing is dispatched or
    failed, with or without the fix (the handover order is unchanged). -/
theorem waits_for_writer (waitForSlot : Bool) (w : World)
    (hs : w.status = .waiting) (hu : w.runnerUsable = true) (hw : w.writer = .app) :
    poll waitForSlot w = w := by
  cases w with
  | mk status writer appLeaseOpen runnerUsable =>
    simp only at hs hu hw
    subst hs hu hw
    simp [poll]

end VolumeOperationDispatch

/-! ## ato-api (cross-repo) :: the Runner's report against a concurrent poll

`completeVolumeOperation` and the `running` branch of `advanceVolumeOperation` in the
same file. Found on production on 2026-11-20: the Runner logged the checkpoint as
succeeded, the API recorded the operation `interrupted` ("the Runner ended the
operation without reporting") two seconds later, and the slot's checkpoint columns had
nevertheless advanced.

The report used to be a sequence of independent writes: record the checkpoint, end the
lease, then take the operation out of `running`. `GET /state/operations/:id` advances
the operation, and it interrupts a running operation whose lease has ended and whose
writer is free. The checkpoint commit already released the writer, so the poll only
needs to land after "end the lease". -/
namespace VolumeOperationReport

inductive Status where
  | running | succeeded | interrupted
  deriving DecidableEq, Repr

structure World where
  status : Status
  leaseEnded : Bool
  checkpointRecorded : Bool
  deriving DecidableEq, Repr

inductive Event where
  | poll              -- GET /state/operations/:id, or the cron
  | leaseExpired      -- the lease ends without a report (Runner died, lease expired)
  | recordCheckpoint  -- report, write 1 (unfixed)
  | endLease          -- report, write 2 (unfixed)
  | finish            -- report, write 3 (unfixed): CAS out of `running`
  | report            -- the fixed report: one transaction
  deriving DecidableEq, Repr

def step (w : World) : Event → World
  | .poll =>
    if w.status == .running && w.leaseEnded then { w with status := .interrupted } else w
  | .leaseExpired => { w with leaseEnded := true }
  | .recordCheckpoint => { w with checkpointRecorded := true }
  | .endLease => { w with leaseEnded := true }
  | .finish => if w.status == .running then { w with status := .succeeded } else w
  | .report =>
    if w.status == .running then ⟨.succeeded, true, true⟩ else w

def run (w : World) : List Event → World
  | [] => w
  | e :: rest => run (step w e) rest

/-- The Runner committed the checkpoint; its lease is live and it is about to report. -/
def committed : World := ⟨.running, false, false⟩

/-- What the owner and the export read must agree: a checkpoint is recorded as the
    volume's current one exactly when its operation succeeded. -/
def consistent (w : World) : Bool :=
  w.checkpointRecorded == (w.status == .succeeded)

def unfixedEvent : Event → Bool
  | .report => false
  | _ => true

def fixedEvent : Event → Bool
  | .recordCheckpoint | .endLease | .finish => false
  | _ => true

/-- BUG: a poll between the report's second and third write records a successful
    checkpoint as interrupted while its checkpoint columns stay advanced. -/
theorem poll_between_report_writes_interrupts_success_BUG :
    ∃ events, events.all unfixedEvent = true ∧ ¬ events.contains .leaseExpired ∧
      (run committed events).status = .interrupted ∧
      (run committed events).checkpointRecorded = true ∧
      consistent (run committed events) = false :=
  ⟨[.recordCheckpoint, .endLease, .poll, .finish], by decide, by decide, by decide, by decide, by decide⟩

/-- Without the poll in that window the same report succeeds. -/
theorem unfixed_report_without_poll_succeeds :
    run committed [.poll, .recordCheckpoint, .poll, .endLease, .finish, .poll] = ⟨.succeeded, true, true⟩ := by
  decide

/-- HOLDS (the fix): with the report as one transaction, status and checkpoint columns
    agree after every interleaving of polls, lease expiry and (repeated) reports. -/
theorem fixed_always_consistent (events : List Event) (h : events.all fixedEvent = true) :
    ∀ w, consistent w = true → consistent (run w events) = true := by
  induction events with
  | nil => intro w hw; exact hw
  | cons e rest ih =>
    intro w hw
    simp only [List.all_cons, Bool.and_eq_true] at h
    apply ih h.2
    cases w with
    | mk status leaseEnded checkpointRecorded =>
      cases e <;> cases status <;> cases leaseEnded <;> cases checkpointRecorded <;>
        simp_all [step, consistent, fixedEvent]

/-- HOLDS: while the lease has not ended by itself, polls cannot change the outcome
    of a report: the operation ends `succeeded`. -/
theorem fixed_polls_cannot_interrupt_report (before after : Nat) :
    (run committed (List.replicate before .poll ++ .report :: List.replicate after .poll)).status
      = .succeeded := by
  have hbefore : ∀ n, run committed (List.replicate n .poll ++ .report :: List.replicate after .poll)
      = run committed (.report :: List.replicate after .poll) := by
    intro n
    induction n with
    | zero => rfl
    | succ n ih => simpa [List.replicate_succ, run, step, committed] using ih
  have hafter : ∀ n, run ⟨.succeeded, true, true⟩ (List.replicate n .poll) = ⟨.succeeded, true, true⟩ := by
    intro n
    induction n with
    | zero => rfl
    | succ n ih => simpa [List.replicate_succ, run, step] using ih
  rw [hbefore before]
  simp only [run, step, committed]
  simpa using congrArg World.status (hafter after)

/-- HOLDS (negative case): a report that arrives after a recorded interruption changes
    nothing; the interrupted operation is not presented as a checkpoint. -/
theorem fixed_late_report_changes_nothing :
    run committed [.leaseExpired, .poll, .report] = ⟨.interrupted, true, false⟩ := by
  decide

/-- HOLDS: a lease that ended without any report is still recorded as interrupted. -/
theorem fixed_dead_runner_still_interrupted :
    (run committed [.leaseExpired, .poll]).status = .interrupted := by
  decide

end VolumeOperationReport

/-! ## ato-api (cross-repo) :: with-data export of a Runner-local volume

`src/services/portable_instance_export.ts` (`quiescePortableFilesystemState`,
`filesystemStateHead`) behind `POST /v1/portable-application-exports/instances/:id`.
Found on production on 2026-11-20: the export stopped the always-on App, waited for its
writer and only then failed `state_volume_checkpoint_required`.

A `runner_volume` slot is exportable only when its head is a checkpoint taken at the
current writer generation. Every Run takes a new generation when it acquires the
writer, and stopping a Run does not change the generation. -/
namespace VolumeExportGate

structure World where
  writerEpoch : Nat
  /-- The generation at which the head was recorded as a checkpoint, if ever. -/
  checkpointEpoch : Option Nat
  runLive : Bool
  deriving DecidableEq, Repr

def checkpointCurrent (w : World) : Bool := w.checkpointEpoch == some w.writerEpoch

/-- `acquireWriter` for a Run: the next generation. -/
def startRun (w : World) : World := { w with writerEpoch := w.writerEpoch + 1, runLive := true }

def stopRun (w : World) : World := { w with runLive := false }

/-- A successful checkpoint operation: it takes the next generation itself, from a
    confirmed-stopped volume, and records the head at that generation. -/
def checkpoint (w : World) : World :=
  if w.runLive then w
  else { w with writerEpoch := w.writerEpoch + 1, checkpointEpoch := some (w.writerEpoch + 1) }

inductive Reachable : World → Prop where
  | init : Reachable ⟨0, none, false⟩
  | startRun {w} : Reachable w → w.runLive = false → Reachable (startRun w)
  | stopRun {w} : Reachable w → Reachable (stopRun w)
  | checkpoint {w} : Reachable w → Reachable (checkpoint w)

structure Outcome where
  accepted : Bool
  /-- The export stopped a Run that was serving. -/
  stoppedRun : Bool
  after : World
  deriving DecidableEq, Repr

/-- Before the fix: stop, wait for the writer, then look at the checkpoint. -/
def exportStopFirst (w : World) : Outcome :=
  ⟨checkpointCurrent (stopRun w), w.runLive, stopRun w⟩

/-- The fix: look at the checkpoint first; stop only what can then be captured. -/
def exportCheckFirst (w : World) : Outcome :=
  if checkpointCurrent w then ⟨checkpointCurrent (stopRun w), w.runLive, stopRun w⟩
  else ⟨false, false, w⟩

/-- BUG: a reachable state in which the export stops the Run and then fails. -/
theorem export_stops_run_then_fails_BUG :
    ∃ w, Reachable w ∧ (exportStopFirst w).stoppedRun = true ∧ (exportStopFirst w).accepted = false :=
  ⟨startRun ⟨0, none, false⟩, .startRun .init rfl, by decide, by decide⟩

/-- The same holds after a checkpoint, as soon as a Run has started again. -/
theorem checkpoint_then_run_still_fails_BUG :
    ∃ w, Reachable w ∧ (exportStopFirst w).stoppedRun = true ∧ (exportStopFirst w).accepted = false :=
  ⟨startRun (checkpoint ⟨0, none, false⟩), .startRun (.checkpoint .init) (by decide), by decide, by decide⟩

/-- Stopping a Run never turns a stale checkpoint into a current one, so the answer is
    known before anything is stopped. -/
theorem stop_does_not_change_the_answer (w : World) :
    checkpointCurrent (stopRun w) = checkpointCurrent w := rfl

/-- In every reachable state the checkpoint generation is at most the writer's, and a
    live Run is strictly ahead of it. -/
theorem reachable_checkpoint_bound {w : World} (h : Reachable w) :
    ∀ c, w.checkpointEpoch = some c → c ≤ w.writerEpoch ∧ (w.runLive = true → c < w.writerEpoch) := by
  induction h with
  | init => intro c hc; simp at hc
  | @startRun w _ hlive ih =>
    intro c hc
    have := ih c hc
    simp only [startRun]
    exact ⟨Nat.le_succ_of_le this.1, fun _ => Nat.lt_succ_of_le this.1⟩
  | @stopRun w _ ih =>
    intro c hc
    have := ih c hc
    simp only [stopRun]
    exact ⟨this.1, fun h => by simp at h⟩
  | @checkpoint w _ ih =>
    intro c hc
    unfold checkpoint at hc ⊢
    split
    · next hl => simp only [hl, ite_true] at hc; exact ih c hc
    · next hl =>
      simp only [hl] at hc
      simp only [Bool.false_eq_true, ite_false, Option.some.injEq] at hc
      subst hc
      exact ⟨Nat.le_refl _, fun h => by simp_all⟩

/-- A serving Run always means the export is refused: there is never a reason to stop
    one for an export. -/
theorem live_run_is_never_exportable {w : World} (h : Reachable w) (hl : w.runLive = true) :
    checkpointCurrent w = false := by
  unfold checkpointCurrent
  cases hc : w.checkpointEpoch with
  | none => rfl
  | some c =>
    have := (reachable_checkpoint_bound h c hc).2 hl
    simp only [beq_eq_false_iff_ne, ne_eq, Option.some.injEq]
    exact Nat.ne_of_lt this

/-- HOLDS (the fix): a refused export leaves the Instance exactly as it was. -/
theorem fixed_refusal_has_no_side_effect (w : World) (h : (exportCheckFirst w).accepted = false) :
    (exportCheckFirst w).stoppedRun = false ∧ (exportCheckFirst w).after = w := by
  unfold exportCheckFirst at h ⊢
  split
  · next hc => simp [stop_does_not_change_the_answer, hc] at h
  · exact ⟨rfl, rfl⟩

/-- HOLDS: in reachable states the fixed export never stops a Run at all. -/
theorem fixed_never_stops_a_run {w : World} (h : Reachable w) :
    (exportCheckFirst w).stoppedRun = false := by
  unfold exportCheckFirst
  split
  · next hc =>
    cases hl : w.runLive with
    | false => rfl
    | true => rw [live_run_is_never_exportable h hl] at hc; exact absurd hc (by decide)
  · rfl

/-- HOLDS: the fix refuses exactly the exports that failed before; nothing that used to
    succeed is rejected. -/
theorem fixed_accepts_the_same_exports (w : World) :
    (exportCheckFirst w).accepted = (exportStopFirst w).accepted := by
  unfold exportCheckFirst exportStopFirst
  split
  · rfl
  · next hc => simp [stop_does_not_change_the_answer, hc]

/-- HOLDS: stop, checkpoint, export — the sequence the owner is told to use — is
    accepted. -/
theorem checkpoint_after_stop_is_exportable (w : World) :
    (exportCheckFirst (checkpoint (stopRun w))).accepted = true := by
  simp [exportCheckFirst, checkpoint, stopRun, checkpointCurrent]

end VolumeExportGate

/-! ## ato-api (cross-repo) :: the owner's Stop, Start and Keep-running controls

`src/routes/compute_instances.ts` (`POST /stop`, `POST /start`, `PATCH /runtime-policy`)
with `src/services/always_on.ts` and `src/services/execution_authorization.ts`. Found
on production on 2026-11-20: Stop then Start left an always-on Instance serving with
`desired_state: stopped`, and restoring the policy stopped the Run again.

This extends `AlwaysOnRecovery` with the owner's controls. A Run is renewable when it
was launched while the policy wanted it (`reserveExecutionAuthorization`); a Run
launched under a hard deadline cannot be upgraded in place. A Run of an Instance whose
intent is `stopped` is not restarted when it ends and is put to sleep when idle. -/
namespace AlwaysOnOwnerControls

inductive Desired where
  | running | stopped
  deriving DecidableEq, Repr

inductive Status where
  | idle | waiting | running
  deriving DecidableEq, Repr

structure Run where
  id : Nat
  renewable : Bool
  deriving DecidableEq, Repr

/-- An always-on Instance. -/
structure World where
  desired : Desired
  status : Status
  lastRun : Option Nat
  live : Option Run
  /-- How many times an owner control stopped a serving Run. -/
  outages : Nat
  deriving DecidableEq, Repr

/-- `stopInstanceRuntime`. -/
def stopRuntime (w : World) : World :=
  match w.live with
  | some _ => { w with live := none, outages := w.outages + 1 }
  | none => w

/-- `setRuntimePolicy` to a different intent: new generation, nothing bound. -/
def setPolicy (w : World) (d : Desired) : World :=
  { w with desired := d, status := if d == .running then .waiting else .idle, lastRun := none }

/-- `POST /stop`. -/
def stop (w : World) : World := setPolicy (stopRuntime w) .stopped

/-- `POST /start`: join the serving Run or launch `fresh`. With the fix the intent goes
    back to `running` first, so the Run is renewable and bound to the policy. -/
def start (fixed : Bool) (w : World) (fresh : Nat) : World :=
  if fixed && w.desired == .stopped then
    match w.live with
    | some r => { w with desired := .running, status := .running, lastRun := some r.id }
    | none => { w with desired := .running, status := .running, lastRun := some fresh,
                       live := some ⟨fresh, true⟩ }
  else
    match w.live with
    | some _ => w
    | none => { w with live := some ⟨fresh, w.desired == .running⟩ }

/-- `PATCH /runtime-policy` within always-on. -/
def patch (fixed : Bool) (w : World) (d : Desired) : World :=
  if !fixed then
    let w := stopRuntime w
    if w.desired == d then w else setPolicy w d
  else if w.desired == d then w
  else
    match d, w.live with
    | .running, some r =>
      if r.renewable then { w with desired := .running, status := .running, lastRun := some r.id }
      else setPolicy (stopRuntime w) d
    | _, _ => setPolicy (stopRuntime w) d

/-- The Run ends by itself (crash, host restart, hard deadline). Only an Instance
    whose intent is `running` is queued for a restart (`scheduleInstanceRestart`). -/
def runEnds (w : World) : World :=
  match w.live with
  | none => w
  | some r =>
    if w.desired == .running && (w.lastRun == some r.id || w.lastRun == none) then
      { w with live := none, status := .waiting }
    else { w with live := none }

/-- `reconcileAlwaysOnInstances`: adopt the serving Run or launch `fresh`. -/
def sweep (w : World) (fresh : Nat) : World :=
  if w.desired == .running && w.status == .waiting then
    match w.live with
    | some r => { w with status := .running, lastRun := some r.id }
    | none => { w with status := .running, lastRun := some fresh, live := some ⟨fresh, true⟩ }
  else w

/-- Resident: whatever ends the serving Run, the reconciler brings a Run back. -/
def resident (w : World) : Bool :=
  w.desired == .running && (sweep (runEnds w) 99).live.isSome

/-- An always-on Instance serving normally. -/
def serving : World := ⟨.running, .running, some 1, some ⟨1, true⟩, 0⟩

/-- BUG: Stop then Start leaves the Instance serving but not resident: the intent
    still says `stopped`, so the Run is not renewable and nothing restarts it. -/
theorem stop_start_is_not_resident_BUG :
    let w := start false (stop serving) 2
    w.live = some ⟨2, false⟩ ∧ w.desired = .stopped ∧ resident w = false ∧
      (sweep (runEnds w) 3).live = none := by
  decide

/-- BUG: restoring the intent afterwards stops the Run that is serving. -/
theorem restoring_policy_stops_the_run_BUG :
    let before := start false (stop serving) 2
    (patch false before .running).live = none ∧
      (patch false before .running).outages = before.outages + 1 := by
  decide

/-- BUG: a policy request that changes nothing stops the resident Run. -/
theorem unchanged_policy_request_stops_the_run_BUG :
    (patch false serving .running).live = none ∧ (patch false serving .running).outages = 1 := by
  decide

/-- HOLDS (the fix): Stop then Start is resident again, with a renewable Run bound to
    the policy, and it comes back after the Run ends. -/
theorem fixed_stop_start_is_resident (w : World) (fresh : Nat) :
    let after := start true (stop w) fresh
    after.live = some ⟨fresh, true⟩ ∧ after.desired = .running ∧
      after.lastRun = some fresh ∧ resident after = true := by
  cases w with
  | mk desired status lastRun live outages =>
    cases live <;> simp [start, stop, stopRuntime, setPolicy, resident, runEnds, sweep]

/-- HOLDS: a policy request that changes nothing changes nothing. -/
theorem fixed_unchanged_policy_is_a_noop (w : World) : patch true w w.desired = w := by
  simp [patch]

/-- HOLDS: returning to `running` keeps a Run that already holds renewable authority
    and binds the policy to it, without an outage. -/
theorem fixed_keep_running_adopts_renewable_run (w : World) (r : Run)
    (hd : w.desired = .stopped) (hl : w.live = some r) (hr : r.renewable = true) :
    let after := patch true w .running
    after.live = some r ∧ after.outages = w.outages ∧ after.lastRun = some r.id ∧
      resident after = true := by
  cases w with
  | mk desired status lastRun live outages =>
    simp only at hd hl
    subst hd hl
    simp [patch, hr, resident, runEnds, sweep]

/-- HOLDS (negative case): a Run launched under a hard deadline is not adopted. It is
    drained, and the reconciler replaces it with a renewable one. -/
theorem fixed_keep_running_replaces_hard_deadline_run (w : World) (r : Run) (fresh : Nat)
    (hd : w.desired = .stopped) (hl : w.live = some r) (hr : r.renewable = false) :
    let after := patch true w .running
    after.live = none ∧ after.status = .waiting ∧
      (sweep after fresh).live = some ⟨fresh, true⟩ := by
  cases w with
  | mk desired status lastRun live outages =>
    simp only at hd hl
    subst hd hl
    simp [patch, hr, stopRuntime, setPolicy, sweep]

/-- HOLDS: Stop still stops, and a stopped Instance is not restarted. -/
theorem fixed_stop_is_not_resident (w : World) (fresh : Nat) :
    (stop w).live = none ∧ (sweep (stop w) fresh).live = none := by
  cases w with
  | mk desired status lastRun live outages =>
    cases live <;> simp [stop, stopRuntime, setPolicy, sweep]

inductive Control where
  | stop | start | keepRunning | keepStopped | runEnds | sweep
  deriving DecidableEq, Repr

def apply (w : World) (fresh : Nat) : Control → World
  | .stop => stop w
  | .start => start true w fresh
  | .keepRunning => patch true w .running
  | .keepStopped => patch true w .stopped
  | .runEnds => runEnds w
  | .sweep => sweep w fresh

/-- What the owner's controls and the reconciler preserve. A serving Run always belongs
    to an Instance whose intent is `running`, is renewable, and is either bound to the
    policy or adopted by the next sweep; an Instance that wants a Run and has none is
    queued for one. -/
def supervised (w : World) : Bool :=
  match w.live with
  | none => w.desired == .stopped || w.status == .waiting
  | some r =>
    w.desired == .running && r.renewable &&
      (w.lastRun == some r.id || w.status == .waiting)

theorem fixed_control_keeps_supervision (w : World) (fresh : Nat) (c : Control)
    (h : supervised w = true) : supervised (apply w fresh c) = true := by
  cases w with
  | mk desired status lastRun live outages =>
    cases live with
    | none =>
      cases c <;> cases desired <;> cases status <;>
        simp [apply, stop, start, patch, runEnds, sweep, stopRuntime, setPolicy, supervised] at h ⊢
    | some run =>
      cases run with
      | mk id renewable =>
        cases desired with
        | stopped => simp [supervised] at h
        | running =>
          cases renewable with
          | false => simp [supervised] at h
          | true =>
            cases c <;> cases status <;>
              simp [apply, stop, start, patch, runEnds, sweep, stopRuntime, setPolicy, supervised] at h ⊢ <;>
              (try split) <;> simp_all

/-- HOLDS (the fix): no sequence of Stop, Start, Keep running, Run endings and sweeps
    reaches "serving but not resident". -/
theorem fixed_controls_keep_supervision (controls : List (Control × Nat)) :
    ∀ w, supervised w = true →
      supervised (controls.foldl (fun w c => apply w c.2 c.1) w) = true := by
  induction controls with
  | nil => intro w h; exact h
  | cons c rest ih =>
    intro w h
    exact ih _ (fixed_control_keeps_supervision w c.2 c.1 h)

/-- The unfixed Start breaks that invariant from a supervised state. -/
theorem unfixed_start_breaks_supervision_BUG :
    supervised (stop serving) = true ∧ supervised (start false (stop serving) 2) = false := by
  decide

end AlwaysOnOwnerControls
