# Self-hosted mail as an Ato app — plan (2026-11-02)

Status: plan for review. Nothing in this document has been implemented or
deployed. No production change is authorized by this document.

## Where we are

A Stalwart v0.16.22 container (`ato-mail-stalwart`, pinned upstream digest
`sha256:d898e81b…`) runs **directly in Docker** on the production Runner host
`65.109.37.38` (`hetzner-ato-runner`), outside Ato. Verified on 2026-11-02:

- inbound and outbound SMTP for `mail.ato.run` and `atousercontent.com`
  (Gmail → Ato and Ato → Gmail; DKIM/SPF/DMARC pass; Gmail inbox placement);
- PTR `mail.ato.run`;
- Let's Encrypt certificate obtained by the host Caddy (an import line appended
  to the generated Caddyfile) and copied into Stalwart by a daily timer.

This container is the **rollback target** for every phase below. Its
configuration, data, listeners and certificate sync stay intact and
restartable until clean-up; no phase repurposes it.

The goal is to run the same service **as an Ato app**: always-on, fixed
public TCP ports, persistent state, Ato-managed lifecycle.

## Capsule model: one K for the mail service, components composed in D

The mail service is **one Capsule**, but one Capsule is not one container.
The Contract K states what must be true of the service, in product-agnostic
terms; each Derivation D composes the components that realize it.

### K — service conditions (product-agnostic)

| # | Condition |
|---|---|
| K1 | SMTP on public TCP 25 accepts mail for the declared domains. |
| K2 | Authenticated submission (465) and mailbox access (993) work over TLS. |
| K3 | The HTTPS admin / JMAP endpoint answers. |
| K4 | Mailboxes and the outbound queue survive a Run restart. |
| K5 | Every public port presents a valid certificate for the declared host name, verified per protocol (below). |
| K6 | Outbound delivery succeeds under the declared conditions (Stage 1: through the granted relay). |

K names no product (Stalwart, Caddy) and no placement. Today's Contract
verifiers cover HTTP (`ato.contract.http@1`), workspace and scenario
observations. K1, K2 and K5 need a TCP/TLS observation; that belongs to the
Protocol/Adapter side as a product-agnostic observation, not a mail-specific
Kernel feature.

### K5 — certificate verification per protocol

The certificate is checked as **presented on the public port**, from outside
the host, not by reading the exported files. Each observation records the
leaf serial, SAN, notBefore/notAfter and chain validation result, and
compares the serial with the current export `meta.json`.

| Port | How TLS starts | Observation |
|---|---|---|
| 25, 587 | Plain SMTP, then `STARTTLS` | Connect, read the 220 greeting, `EHLO`, require `STARTTLS` in the EHLO reply, issue `STARTTLS`, complete the handshake, verify the certificate. A port that does not offer `STARTTLS` fails K5. |
| 465, 993 | Implicit TLS | TLS handshake immediately on connect, verify the certificate, then read the protocol greeting (SMTP 220 / IMAP `* OK`) inside TLS. |
| 443 | HTTPS | Ordinary HTTPS request to the declared host name, verify the certificate and the response. |

Verification means: chain to a public trust root, SAN covers the declared
host name, current time within validity, notAfter more than 14 days ahead
(warning below), and serial equal to the exported certificate. Expiry or SAN
checks alone are not sufficient.

### D1 — first Derivation (Stage 1)

| Component | Placement | Treatment in Ato |
|---|---|---|
| Stalwart | Dedicated Runner | The Capsule's execution component |
| Caddy | Existing host | External Ingress / certificate source; delivered as a secret Binding (G2) |
| SMTP relay | Independent service on the host | External-communication Binding (G1, `ato.tcp-egress@1`) |
| Persistent data | Runner Volume | Save / restore target |
| DNS / PTR | Cloudflare / Hetzner | Environment-dependent external Binding |

**D1 is not a self-contained bundle.** It depends on host-provided Bindings
(Caddy certificate, SMTP relay) and on external DNS/PTR. It is reproducible
only where equivalent Bindings are rebound; that is intended, per "rebind
external state".

### D2 — later: Caddy as an internal component

A later D may move Caddy into the Capsule as a component of a service group.
That needs its own port design: forward from the existing host Caddy, a
dedicated IP, or L4 routing. It does not by itself solve certificates for
SMTP STARTTLS / IMAPS, which still need a certificate shared with the mail
component. D2 is added as a new D under the same K; D1 is not rewritten.

D2 preconditions (not D1's): the Runner runs **multiple components of one
Capsule together** (service-group composition) with fixed TCP and
persistent state, and the port design above is chosen.

### Receipts

Each of these is verified with a Receipt tied to the Run attempt, not by
log inspection: state retention (K4), restart, outbound delivery (K6) and
certificate renewal reflected on every port (K5).

### Not added

No new Capsule format and no mail-specific Kernel feature. D1 uses the
existing fixed TCP, `ato.tcp-egress@1`, Runner Volume and Binding paths.

### Capability to confirm

D1's mail component is Stalwart. Because Stalwart cannot consume
`ato.tcp-egress@1` itself, D1 also carries the existing generic transport
Adapter (see Implementation findings); that two-service group already passed
staging on 2026-09-20 and is not a new requirement. Composing Caddy is a D2
precondition and is not required for D1.

For D1, confirm by real-host tests (Phase 2 staging, Phase 4 production
test domain) that the dedicated Runner runs it always-on with fixed TCP on
the standard mail ports, PROXY v2, a persistent state slot, the certificate
Binding and the relay grant at the same time.

## Implementation findings (2026-11-02)

Checked against `origin/stg` (`e2e6c3386`), the 2026-09-20 staging
acceptance (`samples/stalwart-missive-mail`, archived records) and the pinned
Stalwart v0.16.22 source.

1. **Outbound needs the generic transport Adapter as a second component.**
   Stalwart opens SMTP connections itself and does not use the SOCKS endpoint
   that `ato.tcp-egress@1` provides (archived connectivity decision,
   2026-09-20). The existing byte-oriented Adapter
   (`ato-fixed-socks5-tls-adapter`, one compiled destination, verified TLS)
   is what consumes the grant. D1 is therefore Stalwart + that Adapter. This
   is not a new requirement: the same Stalwart + Adapter (+ Missive) service
   group already passed staging on 2026-09-20 with fixed TCP, a persistent
   state slot, stop/restart and checkpoint/restore. Composing Caddy (D2)
   remains separate.
2. **The certificate can reach Stalwart without a new Runner mount.**
   Stalwart's `Certificate.certificate` / `privateKey` accept
   `EnvironmentVariable` as well as `Text` and `File`. Delivering the pair as
   `ato.secret@1` Bindings (env) reuses the existing secret path; durable
   Binding values are replaced by an owner re-run with new values, which
   starts a new Run. Decided 2026-11-02: use this path (see G2).
3. **Fixed TCP PROXY v2 exists** (`client_address_transport`,
   `ecb0f6a4d`), so G4 needs configuration, not code.
4. **Contract observations today:** `ato.contract.http@1`,
   `ato.contract.scenario@1/2`, workspace and instance-snapshot. K1/K2/K5
   need a new TLS observation; it touches the Contract grammar, the bound
   Contract, the Rust verifier and receipt evidence, so it is its own slice.
   The design is settled in "S1 design decisions" below.
5. **Known staging limitations still open:** no id-mapped OCI state mount
   (thin uid-1000 Stalwart image), images preloaded rather than pullable
   (GHCR `write:packages` missing), checkpoints over 64 MiB out of scope.

### S1 design decisions (settled 2026-11-02)

These decide the open questions that must be settled before the TLS
observation is written. The implementation follows them; nothing here is
deployed.

- **Observation shape.** `ato.contract.tls@1` names a logical port (like
  `ato.contract.http@1`), the host name the certificate must cover
  (`server_name`), and how TLS starts (`start = "implicit" | "smtp-starttls"`).
  The runtime resolves the Port to the endpoint it actually realized and
  connects there. Neither an address nor a Runner is in K.
- **What K states.** A chain to a public root, an SAN covering `server_name`,
  and observation time inside the validity period. It does **not** pin the
  leaf serial or any expected certificate: the serial changes on every
  renewal. The serial, SAN, validity, chain length and trust-anchor label are
  receipt evidence; K5 compares the serial with the current export in ops,
  outside the Contract.
- **PROXY v2 stays an ingress concern.** The verifier is an ordinary client of
  the Port. On a fixed-TCP public listener the Runner terminates the client
  connection and inserts PROXY v2 toward the workload; the verifier neither
  sends nor knows about the header. CLI-local endpoints are direct mappings
  and have no header either.
- **Trust anchors.** Public roots only (the Mozilla set), never configurable
  by K and not part of K. The probe has a test-only anchor seam for unit
  tests; production callers pass the public set.
- **ContractRef stability.** The bound TLS fields are optional and omitted when
  absent, so every Contract and Derivation written before this capability
  keeps byte-identical canonical bytes and therefore its existing
  `ContractRef`/`DerivationRef`.
- **Receipt version.** A receipt that carries certificate evidence is
  `ato.contract-verification-receipt/3`; /1 and /2 stay valid and unchanged
  for receipts without it.
- **Hosted observation transport.** The hosted validator observes TCP only on
  endpoints the control plane names for the Run (the public fixed-TCP
  allocations). This slice accepts that list in the validator job; supplying
  it and accepting receipt /3 is a follow-up `ato-api` change. Until then a
  TLS requirement on a hosted Run is unobserved and fails closed — it is never
  assumed satisfied.

### Implementation slices

| # | Slice | Repo | Environment |
|---|---|---|---|
| S1 | `ato.contract.tls@1` observation: `start = "implicit" \| "smtp-starttls"`, server name, chain/SAN/validity, presented serial in evidence; CLI and hosted verifiers from the same Rust authority | ato | local + CI |
| S2 | Certificate as `ato.secret@1` Bindings; exporter with allowlist, validation and owner re-run | ato / host scripts | local, then staging |
| S3 | D1 sample `samples/ato-mail`: Stalwart + Adapter, fixed TCP 25/465/587/993 with PROXY v2, state slot, egress grant to the independent relay, K1–K6 | ato | staging dedicated Runner |
| S4 | Independent outbound relay service (separate from the rollback container) and its Stage 1 grant | host config | staging, then production after approval |
| S5 | Cutover / rollback scripts implementing the required steps, with queue and mailbox consistency records | ato `docs/ops` + scripts | staging rehearsal |

Production steps (Phase 3 onward) stay gated on separate approval.

## Hetzner outbound mail-port status (checked 2026-11-02)

- Robot account: an "Unlock mail ports" request was sent on 2026-11-02 and
  granted. The Hetzner-locked outgoing rules "Block mail ports" (TCP 25,465,
  IPv4 and IPv6) are gone. The server's Robot firewall now has only "Allow all"
  in both directions, and its default policy is `discard`.
- Hetzner's documentation describes the default 25/465 block, the unblock
  conditions (about one month as a customer and a paid first invoice, then a
  case-by-case limit request), and that the block is **per account**: a server
  transferred to another account inherits that account's state. Port 587 is
  never blocked.
- Hetzner publishes **no sending-volume limit** after unblocking and no
  documented abuse consequence. Plan for the worst case: abuse complaints
  against this IP can re-block the ports or lock the host. The host also runs
  production guest runs and CPU VMs, so per-account send limits are mandatory
  before anyone but the operator gets a mailbox.
- Re-check before each phase: the Robot firewall rules, outbound 25 to a
  well-known MX, and the absence of new Robot notices.

## Decision: add a dedicated Runner, do not replace the shared one

The host's shared Runner is `ato runner serve` 0.8.0 (`ato_managed`, 4 slots).
It serves production guest runs and sits beside eight production CPU VMs.
Current `ato` has no `runner` command, so it cannot be upgraded in place, and
the API refuses to add HostBoundary to `ato_managed` shared slots. Fixed TCP,
always-on and network controls exist only in
`apps/connected-realization-worker`, and staging validated them on a
**separate dedicated Runner** (2026-09-19, `ubuntu-sugamo`).

Therefore: install `ato-connected-realization-worker` on this host as a new,
separately enrolled, owner-scoped Runner (`user_managed`, one slot, explicit
placement only), with its own UID, work root and credential. The shared Runner
and CPU VMs stay untouched. Migrating the shared Runner is a separate project.

## G1 — outbound SMTP, in stages

### Stage 1 (initial operation): existing fixed SMTP relay path

Use the transport already accepted on staging (2026-09-20): the capsule-owned
fixed-target transport Adapter with an `ato.tcp-egress@1` grant for **one
exact `/32` and port**, and Stalwart's `MtaRoute` relay with SMTP AUTH and
certificate verification. No arbitrary-destination egress is enabled.

The relay target is a **new, independent** host-side SMTP relay outside the
app, owned by the operator. It is **not** the existing Stalwart container:
that container is the rollback target and must stay restorable as a full
mail server.

- separate service (own unit/container, config and queue directory); no
  public listeners; submission on one non-public address and port reachable
  only from the dedicated Runner's egress path; AUTH required; sender
  domains restricted to the app's domains; per-sender rate limits;
- it delivers directly to MX from `65.109.37.38` with the existing PTR and
  SPF. DKIM signing stays in the app, so deliverability does not change
  during the move;
- it is brought up and tested while the container still serves the domains,
  and it does not bind ports the container uses.

The app's grant names exactly this relay's address and port. Revoking the
grant (or stopping the relay) stops all outbound mail from the app.

Stage 1 does not use a third-party relay service.

### Stage 2: direct SMTP to arbitrary public IPs — separate RFC

Draft as its own RFC (network-controls owner). It stays **disabled** and is
not exposed as a general feature until all of the following are specified,
implemented and accepted:

| Area | Required specification |
|---|---|
| Scope | Dedicated (owner-scoped, one-slot) Runners only; never `ato_managed` or shared slots. |
| Destination control | Destination port 25 only; public unicast addresses only (deny RFC 1918, CGNAT, loopback, link-local, multicast, the host's own addresses, and Ato control-plane ranges); resolution and connection performed by the Runner, not the workload. |
| Connection limits | Per-Instance concurrent connections and new connections per minute, enforced by the Runner. |
| Volume limits | Per-Instance messages or connections per hour and per day; hard stop on exceed; rejected attempts are visible to the owner and the operator. |
| Audit | Per-connection record (Instance, Run, destination, time, bytes, result) retained for abuse handling, without message content. |
| Revocation | Operator and owner revoke the grant; existing connections are torn down within a bounded time; the grant is fenced by generation like fixed TCP. |
| Abuse response | A documented path from a provider complaint to revocation, and evidence retained for the provider. |

Until Stage 2 is accepted, the app keeps using the Stage 1 relay.

## G2 — host-managed certificate, delivered as a secret Binding (decided 2026-11-02)

Decision: the certificate and key reach Stalwart as `ato.secret@1`
Bindings, read by Stalwart through `EnvironmentVariable` references. No new
Runner mount is added. The earlier read-only host directory design (option
b) is withdrawn.

### Ownership and data flow

```
Caddy (user caddy) ── obtains/renews ──▶ Caddy storage (never shared or mounted)
        ato-cert-export (root, oneshot) ── reads only the allowlisted domain's cert+key
              │ validates (key matches, SAN, validity), records meta.json
              ▼ owner re-run of the Instance with new Binding values
        API (durable Binding config, encrypted, Instance/Contract-bound)
              ▼ one-time secret grant per Run
        dedicated Runner ── env ──▶ Stalwart Certificate {EnvironmentVariable}
```

- Caddy remains the only ACME client. Its data directory is never shared,
  mounted or made group-readable.
- `ato-cert-export` handles only an allowlisted `(instance, domain)` pair from
  a root-owned file.
- The values are stored only as the API's encrypted durable Binding config
  and delivered per Run through the existing one-time secret grant; they are
  not written into the Capsule, the state slot or logs.
- The exporter needs an owner credential that can re-run this Instance. Its
  scope, storage (root-only file) and rotation are an open item for S2;
  if no suitably narrow credential exists, renewal is an operator-run
  command until one does.

### Renewal and reflection

1. Caddy renews in place (about 30 days before expiry).
2. `ato-cert-export` runs from a systemd timer (hourly) and on demand. It
   acts only when the source SHA-256 changes, and validates first.
3. It re-runs the Instance with the new Binding values. The new Run starts
   with the new certificate; the fixed TCP generation fences the hand-over.
4. Evidence: the export record and the K5 observation of the presented
   serial on every port after the new Run is ready.

### Failure handling and recovery

| Failure | Behaviour | Recovery |
|---|---|---|
| Caddy renewal fails | The running Run keeps its certificate; the exporter alerts when notAfter is under 14 days. | Fix Caddy (DNS, port 80/443, rate limits); export reruns on the next change. |
| Export validation fails | No re-run; alert. | Investigate the source; rerun manually. |
| Re-run fails or the new Run is not ready | The API keeps the previous Run's outcome visible; alert on serial mismatch. | Re-run with the previous values (kept by the exporter as `*.prev`, root-only). |
| Expired certificate served | Incident. | Re-run with the latest valid pair from Caddy. |
| Caddyfile regenerated by `ato runner setup` (import line lost) | Renewal for the mail domain stops. | Interim: the exporter checks that the mail site is configured and alerts. Long term: register the mail site through the host's own Caddy configuration mechanism. |

## G3–G5 (unchanged)

| # | Item | Direction |
|---|---|---|
| G3 | Fixed TCP on 25/465/587/993 | Allowlist exactly these addresses on the dedicated Runner only; operator allocation per Instance. |
| G4 | Client IP preservation | Use `client_address_transport = "proxy_protocol_v2"`; configure Stalwart trusted proxy networks for the Runner hop. |
| G5 | Staging has no public port 25 | Covered by the pre-cutover verification below. |

## Pre-cutover verification (expanded)

The aim is to prove the public path and the rollback **before** moving the
existing domains.

### Port 25 and the public path, without touching existing MX records

1. **Staging, dedicated staging Runner:** the whole app on high fixed-TCP
   ports, PROXY v2 to Stalwart, the Stage 1 relay path to a controlled sink,
   certificate Binding rotation (forced re-export), and Run restart under
   always-on.
2. **Production host, parallel test domain over IPv6:** the host has
   `2a01:4f9:5a:2295::/64`, and the Robot firewall allows IPv6 both ways.
   Allocate one IPv6 address to the dedicated Runner's fixed TCP and publish
   a separate test domain (for example `mx-test.atousercontent.com`), with an
   MX that has only an AAAA record and its own certificate through G2.
   - Inbound: Gmail delivers over IPv6 to the app's public port 25 → proves
     the real public SMTP path, PROXY v2 and the certificate Binding on the
     production host, while `mail.ato.run` and `atousercontent.com` stay on
     the container.
   - Outbound from the app goes through the Stage 1 relay, so no new sending
     IP or IPv6 PTR is needed for this test.
   - External probes from several networks (for example MXToolbox) against
     the IPv6 port 25.
3. **Optional:** if IPv6 is not acceptable, an additional IPv4 address for
   the server (paid) allows the same test on IPv4 with its own PTR.

### Rollback procedure (required steps)

Restarting the old container is **not** a rollback: mail received and queue
entries created by the app after cutover exist only in the app's state and
would be lost. Rollback therefore moves the app's state back to the old
environment.

Precondition for the whole rollback window: the app runs the **same
Stalwart image digest** as the container, so the data store is readable by
both without migration. A Stalwart upgrade under the app is allowed only
after the rollback window closes.

1. **Stop new acceptance and sending.** Close the app's public listeners
   (25/465/587/993/443) by stopping the fixed TCP allocation; remote MTAs
   queue and retry on temporary failure. Stop the app's outbound queue
   processing, then stop the Run so the store is quiescent. Record the
   time.
2. **Check queue and store consistency.** From the quiescent state, record
   the outbound queue (message IDs, recipients, next-attempt, per-recipient
   delivered/pending status) and per-mailbox message counts and highest
   IDs. Confirm no delivery was in flight at stop (relay log for the window
   shows no open transaction). Take a checksum-verified snapshot of the
   Runner Volume.
3. **Transfer state from the new environment to the old one.** Move the
   current `/srv/ato-mail/data` aside (kept, not deleted), restore the
   snapshot into `/srv/ato-mail/data` with the container's ownership, keep
   the container's `/srv/ato-mail/etc`, and start the container with the
   pinned digest. Release the fixed TCP ports before the container binds
   them.
4. **Verify no loss and no double send.** Mailbox counts and highest IDs
   equal the step-2 record; every queue entry from step 2 is present with
   the same per-recipient status, so already-delivered recipients are not
   retried; messages sent to test addresses during the window arrive
   exactly once (Message-ID check on the receiving side); inbound mail
   deferred during steps 1–3 arrives after restart. Re-run K1–K6 against
   the container.

Cutover is the same procedure in the other direction (stop the container,
check, seed the Instance volume from `/srv/ato-mail/data`, start the app).

### Rollback rehearsal

1. Write the runbook with exact commands for cutover and the rollback
   procedure above.
2. Rehearse it on staging with real mailboxes, including mail received and
   a non-empty outbound queue (unreachable test destination) at the moment
   of rollback. Measure the time.
3. Rehearse it on production with the test domain from step 2 above. Move
   the test domain onto the app, deliver mail, run the rollback procedure,
   and confirm messages received in between are present after rollback.
   Measure the downtime.
4. Pass criteria: no lost messages and no duplicate delivery in either
   direction, queue state carried over, total downtime within the agreed
   window, and the served certificate, PTR and DKIM unchanged after
   rollback.

Senders queue and retry during a short port-25 outage, so a bounded cutover
window does not lose inbound mail. The rehearsal must confirm the bound.

## Phases

1. **Design (no deploy).** K and D1 written as a Capsule (no new format);
   the generic TCP/TLS observation for K1/K2/K5; G1 Stage 2 RFC draft; G2
   exporter and certificate Binding design (this document); independent
   relay design; runbook draft.
2. **Staging.** Pre-cutover verification step 1, plus the staging rollback
   rehearsal. Confirms the D1 Runner capability (one component, fixed TCP,
   persistent Volume and Bindings together) and issues the K4/K5/K6 and
   restart Receipts.
3. **Production dedicated Runner install.** Build the reviewed worker from
   `main`; install as a new systemd unit and user; enroll as the owner's
   dedicated Runner; allowlist only the test IPv6 address first.
4. **Production parallel test.** Pre-cutover verification step 2 and the
   production rollback rehearsal on the test domain.
5. **Cutover window.** Move `mail.ato.run` and `atousercontent.com` onto the
   app. The container is stopped, not modified: its image digest,
   `/srv/ato-mail/etc` and a final copy of `/srv/ato-mail/data` are kept.
   Rollback follows the rollback procedure (state moves back from the app);
   it is never a plain container restart. The Stage 1 relay is the separate
   service from G1.
6. **Clean-up after a stable week.** Remove the Caddy import line once G2
   uses the host mechanism. Retiring the stopped container is a separate,
   explicitly approved step.

## Risks

- The host carries production guest runs and CPU VMs. The new Runner must be
  resource-capped and must not change iptables rules owned by the shared
  Runner or Docker.
- Abuse complaints can get the whole host blocked. Per-sender limits on the
  Stage 1 relay are a precondition for any mailbox not owned by the operator.
- Re-running the shared Runner setup regenerates the Caddyfile and drops the
  mail site until G2 moves it to the host mechanism.
