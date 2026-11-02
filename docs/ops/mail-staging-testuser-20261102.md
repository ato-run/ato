# Staging mailbox test@atousercontent.com (2026-11-02)

Status: **staging PASS.** A hosted staging Run now contains a usable mailbox
`test@atousercontent.com`, and the browser check signed in and opened its
welcome message. This is a **staging-only mailbox in the demo Run**; external
mail delivery for the real domain has not been touched.

## Run

| Field | Value |
|---|---|
| Bundle | `bnd_01M6DGBAAPY4Y8T0J5DRHSDCZR` (`sha256:3ddb4f26…`) |
| ContractRef | `sha256:409253a3…`, `fully_satisfied: true` |
| Derivation | `sha256:18da95ee…` (Stalwart + Missive + a seeding service) |
| Import | `pai_01M6DGBGWS0W2B86T3Y32BA4RQ` |
| Run | `run_01M6DGBH1YDTTDZ7TM9MMYYAXB` |
| Lease | `01M6DGBH1Y4MVXTYNNSPP61ASJ` |
| App URL | `https://01m6dgf04cn7vtcybf350vxq0a.stg-app.ato.run/` |
| K | `webmail-health` 200, `webmail-ui` 200 — satisfied |
| Receipt | [run.json](evidence/mail-browser-staging-testuser-20261102/run.json) |

## How the account is created

The capsule adds a third, long-running service `seeder` (public
`python@sha256:20a82d…` image). It waits for Stalwart's HTTP listener, creates
the domain `atousercontent.com` and the user `test` through the JMAP registry,
delivers one welcome message over the group's SMTP listener, moves it to Inbox,
then keeps its declared `seeder.internal` port (9999) listening so the service
group stays ready. The capsule carries the test-only credentials and the
recovery-admin bootstrap; they are not recorded here.

## Browser check

`scripts/ops/mail-browser-check.mjs` against the app URL (owner session as
Bearer; test account `test@atousercontent.com`):

- `signedIn: true`
- `expectedTextFound: true` ("Welcome to test@atousercontent.com")
- `messageOpened: true`, `messageBodyFound: true`
- Evidence: [login.png](evidence/mail-browser-staging-testuser-20261102/login.png),
  [inbox.png](evidence/mail-browser-staging-testuser-20261102/inbox.png),
  [message.png](evidence/mail-browser-staging-testuser-20261102/message.png),
  [result.json](evidence/mail-browser-staging-testuser-20261102/result.json)

## Scope and limits

- The mailbox lives only in this staging Run's Stalwart store. It is reachable
  through the staging app URL for the Run owner, not as a public mail account.
- **No MX, DNS, inbound SMTP port, outbound relay or production mail-server
  change was made.** Sending mail from this mailbox to the Internet is not
  configured either; the client can sign in and read the seeded message.
- Images were pulled from the ephemeral Cloudflare quick-tunnel registry; that
  tunnel remains a staging workaround.
- Real use of `test@atousercontent.com` for external mail would require the
  production mail server / MX path and is a separate, approval-gated change.
