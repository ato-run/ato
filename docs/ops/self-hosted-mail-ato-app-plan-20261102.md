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

This container is the **rollback target** for every phase below and stays
intact until clean-up.

The goal is to run the same service **as an Ato app**: always-on, fixed
public TCP ports, persistent state, Ato-managed lifecycle.

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

The relay target is a host-side SMTP relay outside the app, owned by the
operator:

- the existing Stalwart container, reduced to an **outbound-only relay**: no
  public listeners, submission on one non-public port reachable only from the
  dedicated Runner's egress path, AUTH required, sender domains restricted to
  the app's domains, per-sender rate limits;
- it keeps doing direct MX delivery from `65.109.37.38` with the existing PTR,
  SPF and DKIM, so deliverability does not change during the move.

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

## G2 — host-managed certificate, read-only to the dedicated Runner (option b)

### Ownership and data flow

```
Caddy (user caddy) ── obtains/renews ──▶ /var/lib/caddy/.../certificates/.../mail.ato.run/
                                                   │ (never shared or mounted)
        ato-cert-export (root, oneshot) ── copies only the target domain's cert+key
                                                   ▼
        /var/lib/ato-certs/<runner-id>/<domain>/{cert.pem,key.pem,meta.json}
            root:<runner-group> 0750 directory, 0640 files
                                                   │ read-only bind mount
        dedicated Runner ── certificate Binding ──▶ app (read-only path)
```

- Caddy remains the only ACME client. Its data directory is never shared,
  mounted or made group-readable.
- `ato-cert-export` copies **only** an allowlisted `(runner-id, domain)` pair.
  The allowlist lives in a root-owned file, so the Runner cannot request
  another domain's key.
- Files are written to a temporary file in the same directory, `fsync`ed and
  renamed atomically, together with `meta.json` (serial, notAfter, SHA-256 of
  the certificate chain and of the key). Readers never see a partial pair.
- The dedicated Runner's user can read this directory and nothing in
  Caddy's storage.

### Renewal and reflection

1. Caddy renews in place (about 30 days before expiry).
2. `ato-cert-export` runs from a systemd timer (hourly) and on demand. It
   exports only when the source SHA-256 changes, and verifies first that
   the key matches the certificate and that the chain covers the domain.
3. The Runner watches `meta.json`. On a new serial it signals the
   certificate Binding consumer: either the app's declared reload operation,
   or a graceful Run restart under the always-on policy. Stalwart loads
   certificates only at start, so the initial integration restarts the
   Run; the fixed TCP generation fences the hand-over.
4. Evidence: the export record and the served certificate's serial on each
   public port, probed after reflection.

### Failure handling and recovery

| Failure | Behaviour | Recovery |
|---|---|---|
| Caddy renewal fails | The last good pair keeps being served; the exporter alerts when notAfter is under 14 days. | Fix Caddy (DNS, port 80/443, rate limits); export reruns on the next change. |
| Export validation fails (mismatched key, wrong SAN, parse error) | Nothing is replaced; the alert is raised. | Investigate the source; rerun the export manually. |
| Reflection fails (Run does not restart, or the port still serves the old serial) | The Run keeps the old certificate; an alert fires on the serial mismatch. | Restart the Run manually; if it cannot become ready, roll back to the previous pair (kept as `*.prev`). |
| Expired certificate served | Treated as an incident. | Restore the latest valid pair from the export directory or Caddy, then restart. |
| Caddyfile regenerated by `ato runner setup` (import line lost) | Renewal for the mail domain stops. | Interim: the exporter checks that the mail site is configured and alerts. Long term: register the mail site through the host's own Caddy configuration mechanism instead of the generated file. |

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

### Rollback rehearsal

1. Write the runbook with exact commands for cutover and rollback. Cutover:
   back up `/srv/ato-mail`, stop the container, seed the Instance state,
   start the app. Rollback: stop the app, take a final snapshot of the app's
   state, restore it into `/srv/ato-mail/data`, start the container.
2. Rehearse it on staging with real mailboxes. Measure the time.
3. Rehearse it on production with the test domain from step 2 above. Move
   the test domain onto the app, deliver mail, roll back to a container
   started from the same state, and confirm messages received in between
   are present after rollback. Measure the downtime.
4. Pass criteria: no lost messages across both directions, total downtime
   within the agreed window, and the served certificate, PTR and DKIM
   unchanged after rollback.

Senders queue and retry during a short port-25 outage, so a bounded cutover
window does not lose inbound mail. The rehearsal must confirm the bound.

## Phases

1. **Design (no deploy).** G1 Stage 2 RFC draft; G2 exporter and
   certificate Binding design (this document); runbook draft.
2. **Staging.** Pre-cutover verification step 1, plus the staging rollback
   rehearsal.
3. **Production dedicated Runner install.** Build the reviewed worker from
   `main`; install as a new systemd unit and user; enroll as the owner's
   dedicated Runner; allowlist only the test IPv6 address first.
4. **Production parallel test.** Pre-cutover verification step 2 and the
   production rollback rehearsal on the test domain.
5. **Cutover window.** Move `mail.ato.run` and `atousercontent.com` onto the
   app; the container becomes the Stage 1 outbound relay (no public
   listeners).
6. **Clean-up after a stable week.** Remove the Caddy import line once G2
   uses the host mechanism.

## Risks

- The host carries production guest runs and CPU VMs. The new Runner must be
  resource-capped and must not change iptables rules owned by the shared
  Runner or Docker.
- Abuse complaints can get the whole host blocked. Per-sender limits on the
  Stage 1 relay are a precondition for any mailbox not owned by the operator.
- Re-running the shared Runner setup regenerates the Caddyfile and drops the
  mail site until G2 moves it to the host mechanism.
