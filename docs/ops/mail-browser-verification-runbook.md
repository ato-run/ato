# Browser mail client verification runbook

Status: tooling ready and locally validated; **not yet run against an Ato Run or
staging**. This is the Phase 5 gate for "a person can open the mail client in a
browser", which a Contract observation cannot express (a receipt cannot assert
an authenticated browser session).

## What it does

`scripts/ops/mail-browser-check.mjs` drives a real headless Chrome through the
DevTools protocol against the URL a person would open:

1. opens `MAIL_CLIENT_URL` and records the login page (title, URL, screenshot);
2. fills the server-rendered login form and submits it;
3. waits for the mailbox page, checks it looks like a mailbox (not a 502 or an
   error page), optionally requires a text (for example the mailbox name or a
   seeded subject);
4. writes `login.png`, `inbox.png` and `result.json` to `OUT_DIR`.

Node 22 built-ins only: no npm install, no downloaded browser. Chrome/Chromium
must exist on the machine (`CHROME_BIN` overrides the default paths).

## When to run

After the mail Capsule's Run is ready and its K3 observation is satisfied. If
the full D1 Run is not available yet (it needs fixed-TCP Ports and TLS), the
[browser slice](../../samples/ato-mail-browser/README.md) declares only the browser
Surface and can be used for this gate first:

- dedicated staging Runner, sample `samples/ato-mail` (PR ato-run/ato#1668),
- the Run's **public URL** for the `app.http` Surface (hosted ready URL /
  Runner ingress), not the mail domain's 443,
- a staging test account created by `configure_stalwart.py`
  (`demo@<test-domain>` in this runbook; never a real account).

## Command

```sh
OUT_DIR=/var/lib/ato/mail-browser/<run-id> \
MAIL_CLIENT_URL=https://<run-public-url>/ \
MAIL_USER=demo@<test-domain> \
MAIL_PASSWORD='<staging test password>' \
MAIL_EXPECT_TEXT='Inbox' \
node scripts/ops/mail-browser-check.mjs
```

- Credentials come from environment variables only; the script never prints
  them and they are not written to `result.json`.
- `MAIL_LOGIN_ONLY=1` captures only the login page (useful when the Run is up
  but the test account is not seeded yet).
- Exit codes: `0` pass, `2` the page rendered but the expectation failed
  (for example the mailbox is empty), `1` infrastructure/UI failure.

## Pass criteria

| Check | Evidence |
|---|---|
| The Run's K3 observations are satisfied | receipt `/3`: `webmail-health` and `webmail-ui` satisfied on `app.http` |
| A browser reaches the client | `login.png`, `result.json` `loginTitle`/`loginUrl` |
| The test account can sign in | exit `0`, `signedIn: true`, `inboxUrl` under the Run URL |
| The mailbox UI renders | `inbox.png`; `MAIL_EXPECT_TEXT` found when set |
| The Run/lease/attempt that was verified | record `run_id`, `lease_id`, `attempt_id` next to the evidence |

Store the three files under `docs/ops/evidence/mail-browser-<date>/` with the
Run identifiers in a sibling note. Do not commit the test password.

## Interpreting failures

- **Login page does not render** (`result.json` has only `loginTitle`):
  the Surface is wrong or the client did not start. Compare with K3: a
  satisfied `webmail-ui` but no browser title means the browser reached
  something other than the client (proxy/ingress mismatch).
- **Sign-in fails / no mailbox page**: the JMAP backend is unreachable or the
  account is not seeded. Check `STALWART_PUBLIC_URL` equals Missive's
  `jmap_url`; a mismatch sends session URLs to the container hostname.
- **Mailbox renders but a message is missing**: check Junk Mail and the relay
  path. A locally delivered message without spam/DKIM configuration can land in
  Junk; do not read that as a delivery failure without the sink evidence.
- **Chrome exits / no DevTools port**: set `CHROME_BIN` and confirm the host
  permits headless Chrome (staging Runner hosts need Chrome installed if the
  check runs there).

## Local dry run (no Ato Run)

The script can be validated against a local Docker pair (public Stalwart image
plus a locally built Missive at the pinned commit). That is what was done on
2026-11-02: login 200, sign-in 303 to `/inbox`, mailbox rendered, screenshots
captured. See
[browser mail client local verification](mail-browser-local-verification-20261102.md).

## Why this cannot be done fully locally on macOS

`ato run` refuses OCI routes on a non-Linux Docker host ("OCI Container routes
require a native Linux Docker host"), so the mail services (OCI images) cannot be
realized by the local runtime on this workstation. The first Ato Run browser
check therefore needs the Linux dedicated Runner (staging, Phase 5). This
runbook is the exact gate to execute there.
