/-
  ato-run/ato @ 8b058e5 — Lean 4 models of small security-critical decision logic.
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
