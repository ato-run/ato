# Public SMTP staging acceptance — prerequisites and blockers, 2026-09-20

The next acceptance after the Stalwart + Missive fixture is a real exchange
with Gmail over public SMTP, limited to inbound port 25 on staging. This note
records why it has not been run, what was measured, and what has to exist
before it can be.

It approves nothing. Production, Discover, Public Try, public 465/587/993, and
direct MX egress from the Runner remain out of scope.

## Result: blocked on hosts, not on code

No host available today can receive or send SMTP on port 25.

| Host | Arch | Inbound 25 | Outbound 25 | Notes |
| --- | --- | --- | --- | --- |
| `ubuntu-sugamo-staging` (holds the mail Instance and its images) | amd64 | closed | blocked | SoftBank residential line (`softbank060148110224.bbtec.net`); Japanese OP25B. Gmail would refuse this sender range regardless. |
| Hetzner dedicated `65.109.37.38` | amd64 | blocked | blocked | Measured with a live listener bound to `0.0.0.0:25` and no host firewall (`ufw` inactive, `INPUT` policy ACCEPT): a second host still got `No route to host`. The block is on Hetzner's network, not the box. |
| `oci-linux-test` | aarch64 | n/a | blocked | Wrong architecture for the three pinned amd64 images; the unprivileged account cannot bind 25 either. |

Method: an ephemeral `nc` listener on the Hetzner box, dialled from a second
host, then removed. Outbound was probed against
`gmail-smtp-in.l.google.com:25`. No service was left listening.

## What has to happen first

1. **Hetzner port 25 unblock.** Requested through Hetzner Robot; it needs
   account ownership, so it is the operator's action, not automatable here.
   Until it is granted, `65.109.37.38` cannot be a public MX.
2. **Reverse DNS.** The box currently answers
   `static.38.37.109.65.clients.your-server.de`. A public MX needs a PTR that
   matches the EHLO name it presents.
3. **The Instance has to move.** The mail fixture's three pinned images are
   preloaded on the Sugamo Runner, which cannot serve public SMTP. The Hetzner
   box would need the same images, enrolment as a staging Runner with a fixed
   TCP allocation on `:25`, and an ingress slot.
4. **A controlled relay.** Every host blocks outbound 25, so the decision is to
   run an authenticated relay on the unblocked host: AUTH required, one allowed
   recipient (the connected Gmail address), everything else refused.
5. **DNS for a dedicated staging subdomain** — MX, A, SPF (including the relay
   address), DKIM, DMARC at `p=none`. No AAAA: IPv6 SMTP is not in scope.
   Existing production mail DNS is not touched.
6. **A human to send the inbound message.** The available Gmail tooling can
   create a draft and read a mailbox, but cannot send. Acceptance A's first
   step is an operator action; verification of what arrives is not.

## What is already in place

The Runner and control plane can already describe who connected. A Port may
declare `client_address_transport = "proxy_protocol_v2"`, and the fixed TCP
listener then writes one PROXY protocol v2 header, built from the addresses the
kernel reports, before any payload. A header the client sends itself arrives
second and is payload to the service. The field is optional everywhere, so a
Contract that omits it keeps its exact `ContractRef`.

That is covered by unit tests and a loopback L4 test only. It has never faced a
real Internet peer, and the fixture does not yet declare it on any Port,
because there is no public SMTP Port to declare it on.

## Still open, independent of the host

- Stalwart's `trusted-proxy` configuration for the exact Runner source, so a
  client-supplied header is never honoured.
- How an unprivileged Runner binds 25 — socket activation, a limited L4 proxy,
  or a narrow nftables redirect — chosen on least privilege, not convenience.
- Sending from Missive itself. The recorded acceptance covers Submission-path
  delivery and Missive display, not composing and sending from Missive.
- Negative tests: unauthenticated external-to-external relay, unknown local
  recipient, malformed sessions, connection and rate limits.
- Abnormal termination with real Stalwart data, as distinct from a clean stop
  and restart.
