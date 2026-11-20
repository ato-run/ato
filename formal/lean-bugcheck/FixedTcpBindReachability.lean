/-!
# ato-api selfhost: a fixed TCP listener the validator cannot reach

Sources: ato-api `selfhost/network.mjs` (`configureNetwork`),
`selfhost/bind-reachability.mjs`; ato `deploy/selfhost/compose.yaml`
(`validator` uses `network_mode: service:api`; `api` is attached only to
`control`, which is `internal: true`).

The validator probes each fixed listener for its TLS observations from the
Coordinator's network namespace. On an internal network it has no route
beyond the subnets its interfaces are attached to, and a loopback address is
its own namespace's, not the Runner's.

Addresses are natural numbers; a subnet is a base and a size.
-/
namespace FixedTcpBindReachability

structure Subnet where
  base : Nat
  size : Nat
deriving DecidableEq, Repr

/-- A host address of the subnet: not its network or broadcast address. -/
def Subnet.hosts (s : Subnet) (ip : Nat) : Bool :=
  s.base < ip && ip + 1 < s.base + s.size

/-- The validator's network namespace. -/
structure Namespace where
  /-- Subnets of its non-loopback interfaces. -/
  attached : List Subnet
  loopback : Subnet

/-- What a probe from that namespace can reach on the Runner host. -/
def reachable (ns : Namespace) (ip : Nat) : Bool :=
  ns.attached.any (·.hosts ip)

/-- A later TLS observation of a listener bound to `ip`. -/
def observationSucceeds (ns : Namespace) (ip : Nat) : Bool := reachable ns ip

/-- Before: any IPv4 address except 0.0.0.0. -/
def acceptBug (_ : Namespace) (_ : Option (List Nat)) (ip : Nat) : Bool := ip != 0

/-- Fixed: inside an attached subnet, or exactly one of the host operator's
  listed addresses when ATO_FIXED_TCP_BIND_ADDRESSES is set. -/
def acceptFixed (ns : Namespace) (listed : Option (List Nat)) (ip : Nat) : Bool :=
  match listed with
  | some addresses => addresses.contains ip
  | none => reachable ns ip

/-- The distribution: `control` is 172.18.0.0/16 (as 16-bit host numbers under
  an abstract base), loopback is separate. -/
def distribution : Namespace :=
  ⟨[⟨1000000, 65536⟩], ⟨5000000, 16777216⟩⟩

def controlGateway : Nat := 1000001
def otherBridge : Nat := 2000001
def loopbackAddress : Nat := 5000001

/-- BUG: a listener on another bridge, or on loopback, is accepted and can
  never be observed. -/
theorem unreachable_bind_is_accepted_BUG :
    acceptBug distribution none otherBridge = true
      ∧ observationSucceeds distribution otherBridge = false
      ∧ acceptBug distribution none loopbackAddress = true
      ∧ observationSucceeds distribution loopbackAddress = false := by decide

/-- FIXED: with no list, whatever is accepted can be observed. -/
theorem fixed_accepted_bind_is_observable (ns : Namespace) (ip : Nat)
    (h : acceptFixed ns none ip = true) : observationSucceeds ns ip = true := by
  simpa [acceptFixed, observationSucceeds] using h

theorem fixed_rejects_the_incident :
    acceptFixed distribution none otherBridge = false
      ∧ acceptFixed distribution none loopbackAddress = false
      ∧ acceptFixed distribution none controlGateway = true := by decide

/-- Fails closed: no usable network and no list accepts no fixed listener. -/
theorem fixed_fails_closed (loopback : Subnet) (ip : Nat) :
    acceptFixed ⟨[], loopback⟩ none ip = false := by
  simp [acceptFixed, reachable]

/-- The list is exact: an address is accepted only when the operator named it. -/
theorem fixed_list_is_exact (ns : Namespace) (addresses : List Nat) (ip : Nat) :
    acceptFixed ns (some addresses) ip = true ↔ ip ∈ addresses := by
  simp [acceptFixed]

end FixedTcpBindReachability
