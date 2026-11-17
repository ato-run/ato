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
