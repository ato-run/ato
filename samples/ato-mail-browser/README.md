# ato-mail-browser — browser acceptance slice

Status: **shape-tested; not deployed.** This is the smallest capsule that puts
the mail client in a browser through an Ato Run. It is not the production
capsule: the mail contract lives in [`samples/ato-mail`](../ato-mail/README.md)
(D1), which adds fixed-TCP mail Ports, TLS observations and Bindings.

## What it composes

| Component | Role |
|---|---|
| `stalwart` | mail server; JMAP listener on the internal `stalwart.jmap` Port |
| `missive` | the browser mail client; the only `app.http` Surface |
| `mail_data` | Stalwart's RocksDB store (state slot) |

The same images as D1 (see [`../ato-mail/images/`](../ato-mail/images/README.md)):
the thin Stalwart image and the patched Missive image, `linux/amd64`.

## K

Only two observations, both on the client Surface:

| Requirement | Path | Status | Why |
|---|---|---|---|
| `webmail-health` | `/health` | 200 | the client's web server is up |
| `webmail-ui` | `/login` | 200 | the login page the browser entry point redirects to |

This K deliberately does **not** claim SMTP, IMAP, TLS, delivery, persistence or
443. A satisfying receipt means "the client is served and its surface answers",
nothing more. It declares no fixed-TCP Ports, no TLS observations and no
Bindings, so it can run on a Runner that does not yet provide the HostBoundary
work D1 needs.

## Running it for the browser check

1. The Runner needs the two images preloaded (same digest pins as D1).
2. Start the Run and take its public Surface URL.
3. Seed the Stalwart store with the staging test domain and account. The old
   staging fixture did this with a state seed; sign-in cannot work on an empty
   store. Do **not** put the test password in this capsule.
4. Run the browser gate:
   ```sh
   OUT_DIR=... MAIL_CLIENT_URL=<run-url> MAIL_USER=demo@<test-domain> \
     MAIL_PASSWORD=<test password> MAIL_EXPECT_TEXT=Inbox \
     node scripts/ops/mail-browser-check.mjs
   ```
   See [the runbook](../../docs/ops/mail-browser-verification-runbook.md) for pass
   criteria and failure interpretation.

## Local evidence for this path

The Ato runtime path was exercised locally on 2026-11-02 with an equivalent
two-service group (public Stalwart image + locally built patched Missive):
`ato run` served the client and the host browser signed in
([record](../../docs/ops/mail-browser-ato-run-local-host-20261102.md)). This sample
exists so that the same browser check can run on a Runner without waiting for
the TLS/fixed-TCP work.

## Limitations

- Sign-in requires a seeded account; the capsule carries no credentials.
- No mail transport is declared or observed; the client may render a mailbox
  even when SMTP/IMAP delivery is broken.
- The images are preloaded by digest; a digest/platform mismatch is an
  admission failure, not a Contract failure.

## Local ato run note

On a Linux host the capsule can be checked with `ato run` after substituting local
image digests. One environment caveat: when the CLI itself runs inside a
container as **root** (the macOS Docker-socket workaround), a writable state slot
is rejected with `oci_writable_user_unauthorized` (the state directory would be
owned by 0:0). Run the CLI as the host user/uid 1000, as the Runner does, or
check an equivalent no-state demo. The browser path itself (Missive + Stalwart
over the internal JMAP Port) was verified locally that way on 2026-11-02.
