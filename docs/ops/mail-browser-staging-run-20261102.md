# Browser mail client on a hosted staging Run (2026-11-02)

Status: **staging PASS.** The browser slice ran as a hosted staging Run
(`managed` placement) and its Contract reached `fully_satisfied`. The browser
check signed in and rendered the mailbox through the Run's public app URL.

## Run evidence

| Field | Value |
|---|---|
| Bundle | `bnd_01M6DFH0DX4KT5ZPXNM50535Q0` (`sha256:ed60f3fe…`) |
| ContractRef | `sha256:409253a3…` |
| Derivation | `sha256:add46a0e…` (OCI service group, linux/amd64) |
| Import | `pai_01M6DFH501K6TFPB9RGR8HSFZM` |
| Run | `run_01M6DFH52GJA1SVDVF4KMQ09WD` |
| Lease | `01M6DFH52GK700PDMDBQ4HNT0W` |
| Attempt | `pav_01M6DFH5V7RE0RSB9S5K0PYHPP` |
| Surface | `ready`, endpoint `https://s0-rstg002.ato.run` |
| App URL | `https://cinst-5xiptbfrwsswoict.stg-app.ato.run/` |
| K | `webmail-health` `/health` 200, `webmail-ui` `/login` 200 — both satisfied |
| Receipt | `fully_satisfied: true` ([run.json](evidence/mail-browser-staging-20261102/run.json)) |

## Browser check

`scripts/ops/mail-browser-check.mjs` against the app URL, with the owner
session supplied as a Bearer header and the test-only recovery-admin account:

- `signedIn: true`
- `inboxUrl: https://cinst-5xiptbfrwsswoict.stg-app.ato.run/inbox`
- title (app proxy display name): "ato-mail browser slice: Missive over Stalwart JMAP"
- Evidence: [login.png](evidence/mail-browser-staging-20261102/login.png),
  [inbox.png](evidence/mail-browser-staging-20261102/inbox.png),
  [result.json](evidence/mail-browser-staging-20261102/result.json)

The staging app proxy requires the owner's session; the harness therefore gained
`MAIL_AUTHORIZATION` (Bearer), rather than copying a browser cookie. The
password is test-only and is not recorded here.

## How the images reached the managed Runner

The two images (flattened Stalwart, patched Missive) are not in any registry the
Runner can reach. They were exposed for this Run through an **ephemeral
Cloudflare quick tunnel** to the host's loopback registry and pinned by digest in
the capsule:

`compared-initiatives-scenario-albany.trycloudflare.com/staging-mail/{stalwart,missive}@sha256:…`

The Runner pulled them over public HTTPS (allowed egress) and the receipt's
execution evidence names both image digests and container ids. This is a staging
workaround, not a distribution design: quick tunnels have no uptime guarantee and
must be replaced by a real registry for any durable environment.

## What this proves / does not prove

Proves: the browser slice is hostable as a **hosted staging Run** with
Run/lease/attempt and a satisfied receipt; the client is reachable through the
staging app URL and a real browser signs in and renders the mailbox.

Does not prove: the full D1 contract (fixed TCP 25/465/587/993, PROXY v2, TLS/K5,
Bindings, 443), a seeded user account (the recovery admin was used), or durable
image distribution. Those remain the D1/staging gate.
