# Host browser against the local Ato Run (2026-11-02)

Status: local PASS. A normal desktop browser (the host macOS Chrome, not the
container's Chromium) opened the webmail client served by an `ato run` and
signed in. This is still a local Run, not staging, but it is the first time the
client was used from a browser outside the machine that realized the Run.

## Why a forwarder was needed

The local CLI realized the OCI service group inside Docker (Linux), and the Run
Surface was published on `127.0.0.1:55857` **inside the Docker VM**, so the macOS
host could not open it directly. A small socat sidecar was attached to two
networks:

- a normal bridge network, so its published port `18090` is forwarded by Docker
  Desktop to the host (a container only on the Ato internal network cannot
  publish a port);
- the Run's internal network, to reach the Missive container at `<ip>:8080`.

```sh
docker network create mail-hostnet
docker run -d --name mail-fwd --network mail-hostnet -p 18090:18090 \
  alpine/socat TCP-LISTEN:18090,fork,reuseaddr TCP:<missive-ip>:8080
docker network connect <run-network> mail-fwd
```

This forwarding is a **local-only workaround**; hosted Runs use the Runner
ingress / hosted ready URL and do not need it.

## Result

The host browser check (`scripts/ops/mail-browser-check.mjs`, host Chrome):

| Step | Result |
|---|---|
| `GET http://127.0.0.1:18090/login` | 200, "Login - Missive" |
| sign in as `demo@demo.test` | 303 to `/inbox` |
| `/inbox` | 200, "Inbox - Missive", mailbox list rendered |
| result | `signedIn: true`, `inboxUrl: http://127.0.0.1:18090/inbox` |

Evidence: [login.png](evidence/mail-browser-ato-run-host-20261102/login.png),
[inbox.png](evidence/mail-browser-ato-run-host-20261102/inbox.png),
[result.json](evidence/mail-browser-ato-run-host-20261102/result.json).

## What it proves and what it does not

Proves: the Surface an Ato Run publishes is usable from a normal browser outside
the Run's host, with a real sign-in and mailbox render; the browser harness works
against a non-container browser as well.

Does not prove: staging/production ingress, fixed-TCP mail Ports, TLS/K5, state,
Bindings, 443. Those remain the staging Runner gate.
