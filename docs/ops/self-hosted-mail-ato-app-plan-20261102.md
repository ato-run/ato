# Self-hosted mail as an Ato app — plan (2026-11-02)

Status: plan for review. Nothing in this document has been deployed.

## Where we are

A Stalwart v0.16.22 container (`ato-mail-stalwart`, pinned upstream digest
`sha256:d898e81b…`) runs **directly in Docker** on the production Runner host
`65.109.37.38` (`hetzner-ato-runner`), outside Ato. Verified on 2026-11-02:

- inbound and outbound SMTP for `mail.ato.run` and `atousercontent.com`
  (Gmail → Ato and Ato → Gmail; DKIM/SPF/DMARC pass; Gmail inbox placement);
- PTR `mail.ato.run`; Hetzner unlocked outbound 25/465 for the account;
- Let's Encrypt certificate obtained by the host Caddy (import line appended
  to the generated Caddyfile) and copied into Stalwart by a daily timer.

The goal of this plan is to run the same service **as an Ato app**:
always-on, fixed public TCP ports, persistent state, Ato-managed lifecycle.

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
and CPU VMs stay untouched. A later migration of the shared Runner is a
separate project.

## Gaps to close before the app can replace the container

| # | Gap | Why it blocks | Proposed direction |
|---|---|---|---|
| G1 | **Outbound SMTP to arbitrary MX hosts.** Egress grants are exact CIDR (`/1`–`/32`) + port through the SOCKS broker; Stalwart cannot use SOCKS (2026-09-20 decision). | Direct delivery needs TCP 25 to any public address. | Design a generic "transparent TCP egress by port" grant (destination port only, public addresses only, deny private/link-local/loopback, per-Instance rate/connection limits, audited). Not mail-specific; must go through the network-controls RFC owner. |
| G2 | **TLS certificate for the app's ports.** Caddy owns 80/443 on the host. | The app cannot complete HTTP-01/TLS-ALPN-01 itself. | Option a: DNS-01 inside the app with a narrowly scoped Cloudflare token supplied as a secret Binding. Option b: a host-provided certificate Binding (Caddy → Runner → app, read-only, rotated). Prefer b if reusable for other apps. |
| G3 | **Fixed TCP on privileged public ports** (25/465/587/993) on this host. | The Runner allowlist and API allocation must cover exactly these addresses. | Add `65.109.37.38:{25,465,587,993}` to the dedicated Runner's fixed-TCP allowlist only; operator allocation per Instance. |
| G4 | **Client IP preservation.** | Spam checks and abuse control need the real peer. | Already implemented (`client_address_transport = "proxy_protocol_v2"`, ato `ecb0f6a4d`); configure Stalwart trusted proxy networks for the Runner hop. |
| G5 | **Staging cannot receive public port 25** (staging Runners are residential/OCI). | No end-to-end staging proof on real SMTP. | Stage everything except public SMTP on a staging dedicated Runner (fixed TCP on high ports, egress to a controlled sink); do the public-port proof only during the production cutover window. |

G1 is the only item that needs new platform design; G2 needs a choice.

## Phases

1. **Design (no deploy).** Draft RFC for G1 and pick G2. Review.
2. **Staging.** Dedicated staging Runner with the current worker build;
   Stalwart packaged as a portable `ato.capsule/2` OCI app (pinned digest,
   state on `/var/lib/stalwart`, always-on, fixed TCP on high ports, PROXY v2).
   Verify restart, checkpoint/restore, egress via the G1 implementation to a
   controlled SMTP sink, certificate rotation via G2.
3. **Production Runner install.** Build the reviewed worker from `main`,
   install as a new systemd unit/user on `65.109.37.38`, enroll as the owner's
   dedicated Runner, allowlist G3 addresses. Shared Runner untouched.
4. **Cutover window.** Back up `/srv/ato-mail`, stop `ato-mail-stalwart`,
   seed the Instance state from the backup, start the app, verify inbound
   (MX), outbound (Gmail), IMAPS/submission with the real certificate.
   Rollback: stop the app, start the container from the same backup.
5. **Clean-up.** Remove the Docker container, the Caddy import line and the
   sync timer only after the app has run cleanly for a week.

## Risks

- The host carries production guest runs and CPU VMs; the new Runner must be
  resource-capped and must not change iptables rules owned by the shared
  Runner or Docker.
- Abuse complaints on this IP can get the whole host blocked by Hetzner;
  per-account send limits (G1 rate limits) are a precondition for opening
  the service to anyone but the operator.
- Re-running the shared Runner setup regenerates the Caddyfile and drops the
  mail import line until G2 replaces it.
