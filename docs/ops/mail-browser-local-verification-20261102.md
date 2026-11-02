# Browser mail client — local verification (2026-11-02)

Status: local PASS for the **browser client + Stalwart JMAP** composition.
Not an Ato Run, not staging, no production change. This records the furthest
browser-level evidence available without the dedicated Runner (the pinned
images are preloaded there and are not pullable here).

## What was verified

A real browser (headless Chrome 156 driven over CDP) opened a webmail client,
signed in with a mailbox account, and rendered the mailbox with a message that
had arrived over SMTP:

| Step | Result |
|---|---|
| `GET /login` | 200, "Login - Missive" |
| `POST /login` with the test account | 303 to `/inbox` |
| `GET /inbox` (authenticated) | 200, mailbox list: Inbox, Drafts, Sent Items, Junk Mail (2), Deleted Items |
| message list | subject "Hello from the browser", sender, body preview visible |
| client served from the Run's Surface path | `/health` 200; `/` is 303 to `/login` |

Evidence: [login.png](evidence/mail-browser-20261102/login.png) and
[junk-mail.png](evidence/mail-browser-20261102/junk-mail.png).

## Composition used

The pinned GHCR images are not pullable with the available credential, so the
browser path was exercised with an equivalent local pair:

- **Stalwart** `stalwartlabs/stalwart:v0.16.22` (public image; the staging
  fixture's Stalwart layer derives from this upstream release), HTTP listener on
  8080, store in a named volume.
- **Missive** built from the pinned commit `7716106` (the same commit the
  pinned image labels). D1's image also carries the
  `missive-nondefault-port` compatibility patch; the local unpatched build
  therefore needed a port-80 front for JMAP discovery (see findings).
- Both containers on one Docker network: Stalwart as `stalwart:8080`,
  Missive with `jmap_url = "http://stalwart:8080"` and Stalwart with
  `STALWART_PUBLIC_URL = "http://stalwart:8080"`.
- A `demo.test` domain and `demo@demo.test` user created through Stalwart's
  JMAP registry (admin bootstrap via `STALWART_RECOVERY_ADMIN`), and one
  message delivered with SMTP to port 25.

No secret is reproduced here: the accounts and passwords were test-only and
exist only in the throwaway local volume.

## Findings that changed the sample

1. **K3 must observe `/login`, not `/`.** Missive's root answers 303 to
   `/login`; the sample now declares `webmail-ui` on `/login` (200).
2. **`STALWART_PUBLIC_URL` must equal Missive's `jmap_url`.** Stalwart builds
   the JMAP session/EventSource URLs from this value; a mismatch made Missive's
   EventSource target the container hostname over https and the inbox page
   returned 502.
3. **The non-default-port patch is required for `:8080`.** Unpatched Missive
   derives its redirect allowlist from the URL authority including the port, so
   JMAP discovery redirects are rejected ("unknown host"). The staged image
   carries `missive-nondefault-port.patch`, which strips the port from the
   redirect host; D1's `jmap_url = "http://stalwart:8080"` relies on it.
4. A locally delivered message landed in **Junk Mail** without spam/DKIM
   configuration. Staging must not read "no message in Inbox" as delivery
   failure.

## What this does not prove

- The **Ato Run path**: hosted ready URL, Runner ingress, fixed TCP, PROXY v2,
  state mount, Bindings, K5 certificates.
- Login or mailbox use **inside the Ato app** (only a manual Docker pair).
- Anything on staging or production. `fully_satisfied` and the mail-domain 443
  remain as described in the sample README and ADR-043.

## Reproduction outline

```sh
# 1. Stalwart (public upstream image, same release as the staging fixture)
docker network create mail-demo
docker run -d --name mail --network mail-demo --network-alias stalwart \
  -p 18080:8080 -p 2525:25 \
  -e STALWART_RECOVERY_ADMIN=admin:test-only \
  -e STALWART_PUBLIC_URL=http://stalwart:8080 \
  -v "$PWD/mail-config.json:/etc/stalwart/config.json:ro" \
  -v mail-data:/var/lib/stalwart stalwartlabs/stalwart:v0.16.22

# 2. Missive from the pinned commit (adds the D1 compatibility patches)
docker build -f Dockerfile.missive -t missive:7716106 .
docker run -d --name missive --network mail-demo -p 18081:8080 \
  -v "$PWD/missive-config.toml:/home/missive/config.toml:ro" missive:7716106

# 3. create domain/user through the JMAP registry, send one SMTP message,
#    then open http://127.0.0.1:18081/login in a browser and sign in.
```

The exact browser capture used Chrome `--headless=new --remote-debugging-port`
and the CDP `Network.setCookie` + `Page.captureScreenshot` calls; the
screenshots above are the raw captures (1280x900).

## Clean-up

The containers and the named volume are local scratch only; they were removed
after capture. The production container `ato-mail-stalwart` was not touched.
