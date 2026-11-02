# Remote Linux Ato Run browser check (2026-11-02)

Status: local PASS on a **remote Linux host** (the staging runner host
`ubuntu-sugamo`, x86_64). This is still an ephemeral `ato run`, not a hosted
staging Run with a lease/receipt; it is the first time the stateful browser
slice ran on Linux with the CLI as a non-root user.

## Result

- Built the current CLI from `feat/mail-capsule-sample` on the host, built the
  Stalwart thin image and the patched Missive image (linux/amd64), pinned them in
  a loopback registry and packed a stateful variant of
  `samples/ato-mail-browser` (K: `/health`, `/login`; state `mail_data`).
- `ato run` satisfied K and served `http://127.0.0.1:37943` on the host.
- Seeded the demo account through Stalwart's JMAP registry (admin bootstrap) from
  a helper container on the Run's network.
- From the host macOS Chrome, through an SSH local forward
  (`ssh -L 18090:127.0.0.1:37943 ubuntu-sugamo`), the browser check signed in:
  `signedIn: true`, `inboxUrl: http://127.0.0.1:18090/inbox`,
  `inboxTitle: "Inbox - Missive"`.
- Evidence: [login.png](evidence/mail-browser-staging-host-20261102/login.png),
  [inbox.png](evidence/mail-browser-staging-host-20261102/inbox.png),
  [result.json](evidence/mail-browser-staging-host-20261102/result.json).

## What it establishes

- The stateful browser slice works on a real Linux host when the CLI runs as a
  non-root user (the macOS Docker-socket workaround could not do this;
  `oci_writable_user_unauthorized` there).
- The image build recipes work on amd64; the flattened Stalwart image (export/
  import removes the anonymous VOLUME declarations) is required, because the
  upstream thin image inherits `VOLUME ["/etc/stalwart"]`, which the adapter
  refuses.
- The browser harness needed a body-readiness wait after the title changes
  (`document.body` can still be null while the inbox streams); fixed in the
  harness.

## What it does not establish

- A hosted staging Run (no API upload, lease, attempt, or receipt on
  `staging.api.ato.run`).
- Fixed-TCP mail Ports, TLS/K5, Bindings, 443, or the full D1 contract.
- That the staging hosted runners have these images; they do not (the images
  exist only in the host's loopback registry).

## Inbox with a delivered message (2026-11-02)

After the browser check, a message was delivered through the Run's internal SMTP
listener and moved from Junk to Inbox via the JMAP registry, then the harness was
re-run with `MAIL_EXPECT_TEXT="Hello from the browser"`:
`expectedTextFound: true`, `signedIn: true`.
Evidence: [inbox-with-message.png](evidence/mail-browser-staging-host-20261102/inbox-with-message.png).

## Reading a message in the browser (2026-11-02)

With `MAIL_OPEN_FIRST=1` and
`MAIL_EXPECT_BODY="This message arrived through SMTP"`, the harness opened the
message in the browser and observed its body:
`messageOpened: true`, `messageBodyFound: true`,
`expectedTextFound: true`. Evidence:
[message-open.png](evidence/mail-browser-staging-host-20261102/message-open.png).
