# ato-mail — D1 sample (Stalwart + Missive webmail + fixed transport Adapter)

Status: **local build verified; not deployed.** The bundle builds and validates
offline, and `apps/portable-application/tests/ato_mail_sample.rs` keeps the facts
below true. Nothing here has run on a Runner, staging or production: the two
images are preloaded on the dedicated staging Runner and are not pulled by the
build or the test. No production change is authorized by this directory.

This is slice S3 of the self-hosted mail plan
(`docs/ops/self-hosted-mail-ato-app-plan-20261102.md`): Derivation D1 of "run
the mail service as an Ato app". It is one Capsule with one K; D1 composes three
components: the mail server, the browser mail client, and the transport Adapter
(Stalwart cannot consume the egress grant itself).

## D1 components

| Component | Placement | Treatment in Ato | Here |
|---|---|---|---|
| Stalwart v0.16.22 | Dedicated Runner | the Capsule's execution component | `stalwart` service |
| SMTP relay transport | Service group, same Runner | Adapter consuming the egress Binding | `relay` service |
| Caddy (certificate source) | Existing host | secret Bindings (`mail_cert_pem` / `mail_key_pem`) | bindings only |
| SMTP relay service | Independent host service | `ato.tcp-egress@1` Binding (Stage 1) | `smtp_egress` binding |
| Persistent data | Runner Volume | `ato.state.filesystem@1` slot | `mail_data` → `/var/lib/stalwart` |
| DNS / PTR | Cloudflare / Hetzner | environment-dependent, outside this bundle | not represented |

D1 is deliberately **not self-contained**. It is reproducible only where the
same Bindings and DNS exist; that is the intended "rebind external state".

## Manifest map

| Item | Value |
|---|---|
| Schema | `ato.capsule/2`, one Derivation `oci-service-group` (linux/amd64) |
| Surface | `app.http` at `/healthz/live` (D1 has no HTML UI) |
| Stalwart image | `ghcr.io/koh0920/stalwart-staging@sha256:25723c49…629b` (Stalwart v0.16.22, commit `474dd02`) |
| Adapter image | `ghcr.io/koh0920/ato-fixed-socks5-tls-adapter@sha256:c7142ba9…b557` |
| Mail Ports | `stalwart.smtp` 25, `stalwart.submissions` 465, `stalwart.submissions-starttls` 587, `stalwart.imaps` 993 |
| Surface Port | `app.http` 8080 (JMAP/admin listener) |
| Internal Port | `relay.smtp` 2525 (Stalwart to Adapter, inside the group) |
| PROXY v2 | the four mail Ports (`client_address_transport = "proxy_protocol_v2"`) |
| State | `mail_data` → `/var/lib/stalwart`, read-write, Stalwart only |
| Bindings | `relay_auth`, `mail_cert_pem`, `mail_key_pem` (Stalwart); `smtp_egress` (Adapter) |
| Image build | `images/` — the Dockerfiles, Missive patches and digests that produced the two pins |
| Effects | `requires-confirmation` (implied by `ato.tcp-egress@1`) |

Ports are **logical**. The Runner allocates the public fixed-TCP addresses
(operator-managed, per Instance); K never names one. On staging those public
ports are high, non-standard numbers (G5: staging has no public 25); the guest
ports above stay standard, so K does not change between staging and cutover.

## K1–K6: what this sample decides, and what it does not

| # | Condition (plan) | Decided by this sample | Receipt-backed |
|---|---|---|---|
| K1 | SMTP on 25 accepts mail for the declared domains | **nothing.** No observation sends a message or checks acceptance | **NO** |
| K2 | Authenticated submission 465 and mailbox access 993 over TLS | Certificate and TLS only. No observation authenticates or reads a mailbox | **NO** |
| K3 | The HTTPS admin / JMAP endpoint answers | Three unauthenticated HTTP observations on `app.http` (below). 443 is ADR-043 and is absent | partly: HTTP yes when run; 443 **NO** |
| K4 | Mailboxes and the outbound queue survive a Run restart | **nothing** (a restart creates a new Run; see the design below) | **NO** |
| K5 | Every public port presents a valid certificate for the declared host name | Four `ato.contract.tls@1` observations, one per mail Port, each with its own start mode | yes (when run) |
| K6 | Outbound delivery succeeds through the granted relay | **nothing** (the Adapter consumes the grant, but no observation checks delivery) | **NO** |

**A TLS handshake is not a functional mail check.** K5 is the only condition
this sample can decide, plus part of K3. K1, K2, K4 and K6 need the capability
designed under "Non-TLS functional checks" below; until it exists, no receipt
for this sample can honestly claim them, and a green TLS result must never be
reported as "mail works".

## Certificate observations (K5, and the TLS half of K2)

| Logical Port | `start` | Why |
|---|---|---|
| `stalwart.smtp` (25) | `smtp-starttls` | plaintext 220 greeting, `EHLO`, `STARTTLS`, then the certificate. A port that does not offer STARTTLS fails |
| `stalwart.submissions` (465) | `implicit` | TLS is the first thing on the connection |
| `stalwart.submissions-starttls` (587) | `smtp-starttls` | 587 is a STARTTLS submission port; observing it as implicit would contradict the listener this sample configures |
| `stalwart.imaps` (993) | `implicit` | TLS is the first thing on the connection |

Declared host name: **`mx-test.atousercontent.com`** — the staging test domain
from the plan's pre-cutover verification. Every mail Port must present a
certificate whose SAN covers it. Changing the host name changes the certificate
the staging Runner must present on all four Ports.

K states only: chain to a public root, SAN covering the host name, observation
time inside validity. The leaf serial is **not** in K (it changes on every
renewal); it is receipt evidence and is compared with the certificate export in
ops. PROXY v2 is an ingress concern: the Runner terminates the client connection
and inserts the header toward the workload, and the verifier neither sends nor
knows about it.

## Browser mail client (K3), and what the observations prove

D1's Web Surface is **Govcraft Missive**, a JMAP webmail client. It is the only
service with `app.http`. The browser opens the Run's public URL and gets the
client; Missive's own server then talks to Stalwart's JMAP listener
(`http://stalwart:8080`, the `stalwart.jmap` Port) inside the service group.
The browser never contacts the JMAP listener directly, and no JMAP URL is in K.

Two unauthenticated `GET`s decide the browser half of K3:

| Requirement | Path | Status | Why |
|---|---|---|---|
| `webmail-health` | `/health` | 200 | Missive's own health route: the web server is up |
| `webmail-ui` | `/` | 200 | the HTML entry point the browser opens |

Weaknesses, stated rather than hidden:

- A 200 on `/` proves the client's HTML is served, **not** that a user can sign
  in, read a mailbox or send mail. Those are separate functional conditions
  (below) and are not Receipt-backed yet.
- No body digest is pinned. The SPA entry point can change between builds; K
  pins that the client answers, not its bytes.
- Stalwart's own `/healthz/*` and JMAP discovery are no longer observed: the
  only HTTP Surface is Missive and the hosted relay observes the Surface Port.
  Stalwart liveness is implied by Missive starting, not asserted by K.
- Any HTTP server can return 200. These observations prove the Surface is the
  expected client; a static page that merely *says* "mail" would not satisfy the
  functional conditions, which is why they are designed separately.

## Browser access vs the mail-domain 443

Two different URLs matter:

- **The browser gets the client from the Run's public URL** — the Runner's
  ingress / hosted ready URL for the `app.http` Surface. That is how a person
  opens Missive in a browser; it does not depend on the mail domain's 443 and it
  is not in K.
- **`mail.ato.run:443` (and the JMAP/admin endpoint on the mail domain)** stay
  served by the host Caddy, outside D1. Observing that endpoint needs the
  external-endpoint Binding from
  `docs/rfcs/draft/ADR-043-external-endpoint-observation-binding.md`, which is
  not implemented.

Consequences, recorded so nobody has to rediscover them:

- this sample declares **no** 443 requirement and no binding-backed Port;
- the **bundle's own K** (four TLS + two HTTP observations) can reach
  `fully_satisfied` once a Run serves them — and that is all it means. It does
  **not** mean the mail service works: K1/K2/K4/K6 are not in this K yet, and
  the plan's mail Capsule stays unsatisfied until 443 and the functional
  observations exist too. A green receipt here must never be reported as "mail
  is working";
- when ADR-043 lands, D1 adds one Port (`edge.https` with an `edge_https`
  binding) and one TLS requirement. The mail-Port observations do not change.

## Stage-1 relay (S4) and the placeholders

The `relay` service is the existing generic transport Adapter: one compiled
destination, TLS verified against an explicit CA and server name, consumed
through `ato.tcp-egress@1`. It has no SMTP parser, no DNS, no MX lookup and no
dynamic destination, and it never bypasses certificate verification.

The sink is **S4's** independent relay service, not the rollback container. Its
address is environment data, so `capsule.toml` carries clearly-marked
placeholders that the operator replaces:

| Sample env (in `capsule.toml`) | Placeholder | Operator supplies |
|---|---|---|
| `SMTP_RELAY_TARGET` | `203.0.113.10:2525` (documentation range) | the Stage-1 sink's public IPv4 and submission port; must equal the address and port of the operator's `smtp_egress` grant exactly |
| `SMTP_RELAY_TLS_SERVER_NAME` | `smtp-relay.placeholder.invalid` | the DNS name on the sink's certificate (a DNS name, never an IP) |
| `SMTP_RELAY_CA_PEM` | `/etc/ssl/certs/ca-certificates.crt` | the PEM CA bundle for the sink; keep the system bundle for a publicly trusted sink, or add `ca/<file>.pem` to this workspace (mounted at `/app`) and point here for a private CA |

The adapter rejects a private, loopback, link-local, CGNAT, benchmark or
reserved target, so the placeholder cannot silently point somewhere local.

## Staging configuration (operator steps)

1. **Bindings.** Deliver `relay_auth` (relay SMTP AUTH secret),
   `mail_cert_pem` / `mail_key_pem` (the certificate pair exported from the
   host Caddy, plan G2) as `ato.secret@1` values, and an
   `ato.tcp-egress@1` grant for exactly `SMTP_RELAY_TARGET`. No secret is
   committed here.
2. **Images.** Preload both digest-pinned images on the dedicated Runner. The
   Stalwart image is the thin uid-1000 build that keeps the pinned upstream
   binary but drops `cap_net_bind_service`; the current Runner unpacks a state
   seed as its host uid and has no id-mapped OCI state mount.
3. **Fixed TCP.** Allocate four public fixed-TCP addresses for the mail Ports.
   Staging uses high public ports (G5); the guest ports stay standard.
4. **Registry configuration.** Stalwart keeps listeners, certificates, domains,
   accounts and MTA routes in its dynamic registry, not in
   `stalwart-config.json` (which only selects the RocksDB store under the
   state mount). Apply them with the idempotent script:

```sh
# credentials come from the environment, never from a committed file
STALWART_ADMIN_USER=... STALWART_ADMIN_PASSWORD=... \
STALWART_USER_PASSWORD=... RELAY_AUTH_USERNAME=... \
STALWART_TRUSTED_PROXY_NETWORKS=<runner-hop-cidr>,... \
python3 configure_stalwart.py --url http://127.0.0.1:8080/jmap/

python3 configure_stalwart.py --dry-run   # prints the plan, calls nothing
```

   It ensures: the staging domain, one staging-only test account, a
   `Certificate` whose `certificate` / `privateKey` are **EnvironmentVariable
   references** to `ATO_BINDING_MAIL_CERT_PEM` / `ATO_BINDING_MAIL_KEY_PEM`
   (resolved by Stalwart when it loads TLS, plan G2), the five listeners with
   their TLS start modes, per-listener `overrideProxyTrustedNetworks` for the
   Runner hop (G4), and the relay `MtaRoute` (AUTH username from env, secret as
   an EnvironmentVariable reference to `ATO_BINDING_RELAY_AUTH`) plus the
   outbound strategy that keeps local domains local. Listener matching is **by
   port**: the pinned build's bootstrap already binds some of these ports, and a
   second listener cannot bind the same socket.
5. **Staging-only account and domain.** Domain `mx-test.atousercontent.com`;
   one account named by `STALWART_TEST_USER` whose password comes from the
   environment. It exists only for the staging acceptance and is removed with
   the rest of the test fixtures; no plaintext password and no private key are
   committed to this repository.

## Non-TLS functional checks — design (not implemented)

The product rule is that TLS success alone never satisfies K1, K2, K4 or K6.
This section states the smallest capability that can honestly decide them, and
what must fail closed. **Nothing here is implemented in this sample**, and no
placeholder check is added: a fake assertion that a TLS handshake can satisfy is
worse than an admitted gap.

### Why the existing verifiers cannot decide them

- `ato.contract.tls@1` stops after the handshake (implicit) or after the
  STARTTLS exchange. It never sends a message, authenticates, or reads a
  mailbox. For SMTP it does read the `220` greeting and the `EHLO` reply on the
  way to `STARTTLS`, but that is protocol *presence*, not acceptance: a server
  that answers `250` to everything still passes.
- `ato.contract.http@1` and `ato.contract.scenario@1` are HTTP-only and
  target `app.http`. Scenarios additionally refuse `authorization` and
  `cookie` headers, and their steps are pinned in K by digest, so a credential
  could only enter K itself, which the product rule forbids. They also require
  `isolation.fresh_instance = true`, which a mail server with a persistent
  state slot cannot promise.
- An in-app diagnostic endpoint ("mail works: true") is rejected as evidence: it
  lets the workload assert its own functional correctness, and it is a
  mail-specific surface the Kernel would have to know about.

### Required capability: credentialed, protocol-level observations

One new verifier, one closed probe registry, no new Kernel noun:

```toml
[[contract.observation]]
id = "smtp-accept"
use = "ato.contract.probe@1"      # opaque to the Kernel, like every verifier
port = "stalwart.smtp"            # a logical Port the Derivation declares
probe = "smtp-accept"             # id from a closed, protocol-neutral registry
parameters = "mail_probe"         # ato.secret@1 Binding id; value opaque to K
```

- K names a **Port**, a **probe id** and a **Binding id**. It names no address,
  user, password, domain or product. A probe id outside the registry is refused
  when K is bound, exactly as an unknown verifier is today.
- Probe implementations live beside `lib/tls-observe` (an Adapter/Protocol
  crate, e.g. `lib/probe`), never in the Kernel: the Kernel records an opaque
  observation outcome, the probe owns its protocol semantics.
- The Binding's value is a JSON document the probe defines (recipient domain,
  probe mailbox user and password, and for K6 the operator's relay identity). It
  is delivered through the same one-time secret grant as workload environment,
  but injected **only into the probe process** — never into the workload, never
  into K, never into a log or a receipt.
- Registration is closed and versioned (`smtp-accept@1`,
  `smtp-submit-auth@1`, `imap-fetch@1`, `jmap-queue-drain@1`), so the grammar
  never gains a domain noun and a new probe is a new Adapter slice.

### The probes, and the negative cases they must include

| Probe | Decides | Steps | Mandatory negative case |
|---|---|---|---|
| `smtp-accept@1` | K1 | connect (plaintext plus STARTTLS, or implicit, per Port), `EHLO`, `MAIL FROM`, `RCPT TO:<declared recipient>`, `DATA` a probe message carrying a fresh `Message-ID`, expect `250` | a `RCPT TO` for an undeclared domain must be **rejected** |
| `smtp-submit-auth@1` | K2 (submission) | implicit TLS, `EHLO`, `AUTH` with the Binding credentials, submit to a declared recipient, expect accepted for delivery | `AUTH` with a deliberately wrong secret must be **rejected**; a server that accepts any password fails |
| `imap-fetch@1` | K2 (mailbox) | implicit TLS, `LOGIN`, `SELECT INBOX`, fetch by the `Message-ID` written by the probes above, compare the header | `LOGIN` with a deliberately wrong secret must be **rejected** |
| `jmap-queue-drain@1` | K6 (app side) | authenticated JMAP `EmailSubmission/get` for the submitted message: it must be out of the queue | an unauthenticated call must be **rejected** |

The read-back is what makes the set hard to spoof: `smtp-accept@1` alone can be
passed by a `250`-everything stub, but `imap-fetch@1` must then return the exact
message the probe wrote, which is a store. Combining them means "accepts mail
for the declared domain **and stores it where the declared account can read
it**", which is the condition K1 and K2 actually state.

### K4 (restart persistence) and K6 (delivery) are not single-observation facts

- **K4.** A restart is owner stop then Run again, which produces a **new Run**
  and a new attempt. No single attempt can observe its own restart, so the plan's
  "a Receipt tied to the Run attempt" cannot express K4 as written. Two honest
  options, for the plan owner to choose:
  1. *Instance-scoped evidence.* The write probe runs in attempt N; the owner
     restarts; the read probe runs in attempt N+1 of the same Instance against
     the same declared Ports; only an attempt started after a matching write
     record may report K4, and its receipt cites both attempts
     (`evidence.restart = { previous_attempt, previous_receipt }`).
  2. *Runtime restart primitive.* The runtime restarts the workload inside one
     attempt and re-runs the read probe. This adds a lifecycle operation to the
     Runner, not to the Kernel.
  Until one is chosen, K4 stays unobserved and therefore not satisfied.
- **K6.** `jmap-queue-drain@1` proves the app handed the message to the relay;
  it does not prove the relay delivered it. Proving delivery needs evidence
  *from the sink*: an authorized external endpoint (ADR-043's
  `ato.endpoint.tcp@1` pattern, extended to an authenticated HTTP receipt) or an
  S5 operator check recorded as ops evidence and **not** as a Receipt. This
  sample claims neither.

### Receipt evidence and fail-closed behaviour

- Evidence per observation: probe id, logical Port, resolved protocol, a
  canonical **transcript digest** (no credentials, no message bodies), the
  `Message-ID` and recipient used, the assertion results, and the verdict.
  Credentials never appear in a receipt, a log or a Record.
- Distinct verdict codes, mirroring the TLS probe: `probe_observation_missing`
  (no Binding, no Port, probe not run), `probe_credentials_rejected`,
  `probe_negative_case_failed`, `probe_transcript_mismatch`,
  `probe_timeout`, `probe_unsupported`. A receipt that omits a declared
  observation cannot verify, and `fully_satisfied` stays false.
- Fail closed everywhere: an unresolvable Binding means unobserved, never
  "assumed"; a connection refused or timed out abandons the attempt (not the
  app's Contract failure), the same classification the TLS observation already
  uses.
- **Remaining gap, stated:** even a full probe set proves behaviour for the
  declared domain, not public deliverability, DKIM/SPF/DMARC correctness, or
  abuse resistance. Those stay with S4/S5 and with the ops evidence the plan
  already requires.

### Smallest possible first increment (if one is wanted)

If a single slice should land before the probe registry: teach
`ato.contract.scenario@1` to take its `Authorization` from a named Binding
instead of refusing it, and to allow `fresh_instance = false` when the
Application declares a state slot. That yields an authenticated JMAP admin check
(K3's admin half, mailbox listing) with no credentials in K — but it still
cannot decide K1, K2 (SMTPS/IMAPS), K4 or K6, because scenarios are HTTP-only.
It is a complement, not a substitute.

## Local verification

```sh
# offline: no image pull, no container, no network dependency
cargo test -p ato-portable-application --test ato_mail_sample --offline
cargo fmt --all -- --check
cargo clippy -p ato-portable-application --all-targets --offline -- -D warnings

# optional: build the portable bundle itself (verified offline, 2026-11-02;
# it does not pull or start an image)
cargo run -p ato-cli --bin ato --offline -- pack samples/ato-mail --output .tmp/ato-mail.capsule
```

The test asserts: the four certificate requirements with their Ports and start
modes; the three HTTP requirements (path and status, no body digest, no `401`);
PROXY v2 on the four mail Ports and on nothing else; the guest ports; the two
pinned images; the single state slot and its mount; all four Bindings, their
protocols, and that each is visible to exactly the one service that needs it.


## Local browser verification (2026-11-02)

The pinned images are not pullable here, so the browser path was exercised with
an equivalent local pair:

- `stalwartlabs/stalwart:v0.16.22` (the public image the staging fixture's
  Stalwart layer derives from), HTTP listener on 8080, with
  `STALWART_PUBLIC_URL` and Missive's `jmap_url` both
  `http://stalwart:8080`. The two must match: Stalwart builds the session URLs
  from `STALWART_PUBLIC_URL`, and Missive only follows redirects back to its own
  `jmap_url` host.
- Missive built from the pinned commit `7716106` with the two compatibility
  patches (the non-default-port patch is what allows `jmap_url` to carry
  `:8080`; without it Missive rejects the JMAP discovery redirect).
- A `demo.test` domain and `demo@demo.test` user, one message delivered over
  SMTP, then a real browser (headless Chrome over CDP): the login page rendered,
  sign-in returned 303 to `/inbox`, and the mailbox list plus the delivered
  message rendered in the client.

This is browser-level evidence for the client/backend composition. It is **not**
an Ato Run and not staging evidence. Commands, evidence PNGs and limits:
`docs/ops/mail-browser-local-verification-20261102.md`.

## Browser-only slice

\`samples/ato-mail-browser\` is the smallest capsule that serves this client in a
browser (HTTP observations only, no fixed TCP/TLS/Bindings). It exists so the
browser gate can run on a Runner before D1's HostBoundary work; it is not the
mail contract. See its README.

## Known limits

- **Browser use is not Contract-verified.** The sample asserts that the client is
  served (`/` and `/health`); sign-in, mailbox access and sending are manual
  staging steps until the credentialed observation capability exists.
- **Not deployed.** Build and validation only. The bundle's own six
  observations are the only ones it can decide; K1, K2, K4, K6 and the 443 half
  of K3/K5 are not in this K (see above), so a green receipt for this bundle is
  **not** evidence that mail works.
- **443 needs ADR-043** (external endpoint Binding). Not implemented.
- **The relay is S4.** This sample only names its address as environment data;
  queue-drain and delivery evidence is design only.
- **Images are preloaded**, not pullable from GHCR everywhere: the available
  credential lacked `write:packages`. The sample is therefore not portable to a
  Runner without those exact digests.
- **No id-mapped OCI state mount.** The thin Stalwart image selects uid 1000 to
  match the Runner's seed extraction; a recorded staging limitation, not a
  portable-application contract.
- **The registry script has never run.** `configure_stalwart.py` was derived
  from the pinned source and dry-run locally; its first real execution is the
  staging acceptance. A mismatch there is a bug in the script.
- **Checkpoints over 64 MiB** are out of scope for the Runner, which bounds the
  K4 rehearsal.
