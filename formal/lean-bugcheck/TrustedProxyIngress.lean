/-!
# ato-api selfhost: trusted-proxy ingress can be spoofed by a local process

Sources: ato-project-docs `docs/rfcs/draft/independent-selfhost.md` (the
authentication section), the ato-api `trusted_proxy` / `tailscale_whois`
sign-in path, and ato `deploy/selfhost/` (Caddy, the `web` container and the
published `127.0.0.1:443` listener).

What is abstracted:
* The PROXY header and `X-Ato-Client-Address` are one declared client
  address. `none` means that no header was supplied.
* `tailscaled` produces a forwarded remote client's declared tailnet address
  and, in the fixed design, is the sidecar that can reach the dedicated
  listener. Caddy's extra hop inside the `web` namespace is not a separate
  actor; its behaviour is covered by `tailscaled`.
* The owner's linked tailnet login and another user's login are represented by
  `ownerTailnet` and `otherTailnet`. WhoIs of every other address is `none`.
* `ingressGateway` is the source observed by a published host port for both a
  host process and the tailscaled daemon. `webLoopback` is the source on the
  `web` namespace's loopback.
* Reachability is an assumption, not a model of Docker, Caddy or Tailscale
  namespace isolation. Before the fix, app containers cannot reach the host
  loopback. After the fix, the dedicated listener is assumed reachable only
  by `tailscaled`, while the public listener is assumed reachable by the
  host-side actors but not by an app container or a remote device directly.
-/
namespace TrustedProxyIngress

inductive Actor where
  | tailscaled
  | localProcess
  | appContainer
  | remoteClient
deriving DecidableEq, Repr

inductive Address where
  | ingressGateway
  | ownerTailnet
  | otherTailnet
  | appNet
  | webLoopback
deriving DecidableEq, Repr

inductive User where
  | owner
  | other
deriving DecidableEq, Repr

/-- Tailscale WhoIs: only the linked owner and the other user are known. -/
def whois : Address -> Option User
  | .ownerTailnet => some .owner
  | .otherTailnet => some .other
  | .ingressGateway => none
  | .appNet => none
  | .webLoopback => none

/-- Before is the single published Caddy listener; after is the decided pair. -/
inductive Design where
  | before
  | after
deriving DecidableEq, Repr

inductive Listener where
  | singleListener
  | publicListener
  | dedicatedListener
deriving DecidableEq, Repr

/-- The source address seen at a listener. The published host port sees both
  host-side actors through the Docker ingress gateway. -/
def tcpSource : Design -> Actor -> Listener -> Address
  | .after, .tailscaled, .dedicatedListener => .webLoopback
  | .after, .appContainer, .publicListener => .appNet
  | _, _, _ => .ingressGateway

/-- The public listener is the recovered direct path for host-side actors.
  A remote tailnet device has no direct path; tailscaled forwards it. -/
def publicReachable : Actor -> Bool
  | .tailscaled => true
  | .localProcess => true
  | .appContainer => false
  | .remoteClient => false

/-- Listener reachability. The single distributed listener is reachable by
  host-side actors. After the fix, only tailscaled can reach the dedicated
  listener on the `web` namespace loopback. -/
def reaches : Design -> Actor -> Listener -> Bool
  | .before, .tailscaled, .singleListener => true
  | .before, .localProcess, .singleListener => true
  | .after, actor, .publicListener => publicReachable actor
  | .after, .tailscaled, .dedicatedListener => true
  | _, _, _ => false

/-- The distributed listener accepts PROXY only from its source allow list;
  the fixed dedicated listener always requires PROXY, and the fixed public
  listener never accepts it. -/
def acceptsProxy : Design -> Listener -> Bool
  | .before, .singleListener => true
  | .after, .dedicatedListener => true
  | _, _ => false

/-- The source allow list for PROXY acceptance. The fixed dedicated listener
  is source-gated as well, but its only reachable actor is tailscaled. -/
def sourceAllowed : Design -> Listener -> Address -> Bool
  | .before, .singleListener, source => source == .ingressGateway
  | .after, .dedicatedListener, source => source == .webLoopback
  | _, _, _ => false

/-- The dedicated fixed listener refuses a connection without PROXY. -/
def requiresProxy : Design -> Listener -> Bool
  | .after, .dedicatedListener => true
  | _, _ => false

/-- The address the API uses for WhoIs. `none` means refused or unreachable,
  or (for a listener that requires PROXY) a missing declared address. -/
def reportedAddress (design : Design) (actor : Actor) (listener : Listener)
    (declared : Option Address) : Option Address :=
  if reaches design actor listener then
    match declared with
    | some address =>
      if acceptsProxy design listener && sourceAllowed design listener
          (tcpSource design actor listener) then
        some address
      else
        some (tcpSource design actor listener)
    | none =>
      if requiresProxy design listener then
        none
      else
        some (tcpSource design actor listener)
  else
    none

/-- The session user is determined only by WhoIs of the reported address. -/
def sessionUser (design : Design) (actor : Actor) (listener : Listener)
    (declared : Option Address) : Option User :=
  match reportedAddress design actor listener declared with
  | some address => whois address
  | none => none

/-- BUG: on the distributed listener a local host process sends a PROXY
  header declaring the owner's tailnet address. Its TCP source is the shared
  ingress gateway, so the allow list accepts the header and WhoIs returns the
  owner. -/
theorem local_process_becomes_owner_BUG :
    reaches .before .localProcess .singleListener = true
      ∧ reportedAddress .before .localProcess .singleListener
          (some .ownerTailnet) = some .ownerTailnet
      ∧ sessionUser .before .localProcess .singleListener
          (some .ownerTailnet) = some .owner := by
  decide

/-- FIXED: no actor other than tailscaled can obtain the owner session on any
  listener, whatever declared address it supplies. -/
theorem fixed_no_non_proxy_actor_becomes_owner
    (actor : Actor) (h : actor ≠ .tailscaled)
    (listener : Listener) (declared : Option Address) :
    sessionUser .after actor listener declared ≠ some .owner := by
  cases actor with
  | tailscaled => exact False.elim (h rfl)
  | localProcess =>
    cases listener <;> cases declared <;>
      simp [sessionUser, reportedAddress, reaches, publicReachable,
        acceptsProxy, sourceAllowed, requiresProxy, tcpSource, whois]
  | appContainer =>
    cases listener <;> cases declared <;>
      simp [sessionUser, reportedAddress, reaches, publicReachable]
  | remoteClient =>
    cases listener <;> cases declared <;>
      simp [sessionUser, reportedAddress, reaches, publicReachable]

/-- FIXED, still works: tailscaled forwards the owner's remote device to the
  dedicated listener with its real tailnet address, so the owner signs in. -/
theorem fixed_owner_device_still_signs_in :
    reaches .after .tailscaled .dedicatedListener = true
      ∧ reportedAddress .after .tailscaled .dedicatedListener
          (some .ownerTailnet) = some .ownerTailnet
      ∧ sessionUser .after .tailscaled .dedicatedListener
          (some .ownerTailnet) = some .owner := by
  decide

/-- FIXED: the public listener ignores the declared PROXY address; supplying a
  header produces the same reported address as supplying no header. -/
theorem fixed_public_listener_ignores_proxy
    (actor : Actor) (declared : Option Address) :
    reportedAddress .after actor .publicListener declared
      = reportedAddress .after actor .publicListener none := by
  cases actor <;> cases declared <;>
    simp [reportedAddress, reaches, publicReachable, acceptsProxy,
      sourceAllowed, requiresProxy, tcpSource]

/-- FIXED: the dedicated listener is reachable only by tailscaled. Therefore
  the PROXY header it requires can only be produced by the trusted sidecar. -/
theorem fixed_dedicated_listener_only_proxy
    (actor : Actor)
    (h : reaches .after actor .dedicatedListener = true) :
    actor = .tailscaled := by
  cases actor <;> simp [reaches] at h ⊢

/-- The concrete spoof on the public listener is rejected: the local process
  is reported as the ingress gateway, for which WhoIs has no user. -/
theorem fixed_local_process_cannot_spoof_on_public :
    sessionUser .after .localProcess .publicListener
      (some .ownerTailnet) = none := by
  decide

/-- The dedicated listener does not fall back to the TCP source when PROXY is
  absent. -/
theorem fixed_dedicated_listener_requires_proxy :
    reportedAddress .after .tailscaled .dedicatedListener none = none := by
  decide

/-- OPTIONAL: a source-address allow list cannot separate tailscaled from a
  local process before the fix, because both connections arrive from the same
  Docker ingress gateway. -/
theorem allow_list_cannot_separate_tailscaled_and_local_process
    (allow : Address -> Bool) :
    allow (tcpSource .before .tailscaled .singleListener) = true
      ↔ allow (tcpSource .before .localProcess .singleListener) = true := by
  simp [tcpSource]

end TrustedProxyIngress
