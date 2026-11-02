# Ato runtime browser check — local (2026-11-02)

Status: **local PASS for the Ato runtime path** (`ato run`, OCI service group,
Linux). This is the first time the mail client was reached in a browser through
an Ato Run rather than a manual Docker pair. It is **not** staging and **not**
production: the Run was realized by the local CLI on a Linux Docker host (a
container sharing the host Docker socket), with locally built/derived images.

## Result

- `ato run` started the two-service group, every declared observation
  (`webmail-health` `/health` 200, `webmail-ui` `/login` 200) was satisfied,
  and the CLI kept serving at `http://127.0.0.1:55705`.
- The browser check (`scripts/ops/mail-browser-check.mjs`) then opened that URL,
  signed in as `demo@demo.test`, and rendered the inbox:
  `loginTitle: "Login - Missive"`, `inboxTitle: "Inbox - Missive"`,
  `signedIn: true`, `inboxUrl: http://127.0.0.1:55705/inbox`.
- Evidence: [login.png](evidence/mail-browser-ato-run-20261102/login.png),
  [inbox.png](evidence/mail-browser-ato-run-20261102/inbox.png),
  [result.json](evidence/mail-browser-ato-run-20261102/result.json).

## Why the Run needed prepared images

The committed D1 sample pins images that are preloaded on the dedicated staging
Runner and are not pullable here. For the local Ato Run the same components were
rebuilt/derived, and that exposed two requirements the staging image
preparation must satisfy:

1. **No anonymous volumes.** The upstream Stalwart image declares
   `VOLUME ["/etc/stalwart", "/var/lib/stalwart"]`. The OCI Adapter refuses
   anonymous volumes (`oci_volume_unauthorized`), so the local image is a
   flattened export/import of the upstream rootfs with the entrypoint set
   explicitly. The staging thin Stalwart image is prepared similarly.
2. **No file capability that the Runner's `cap-drop=ALL` cannot honour.**
   Executing the upstream binary under `--cap-drop=ALL` failed with
   `exec ... operation not permitted` until `setcap -r` removed the
   `cap_net_bind_service` xattr (the same step the staging fixture records).
3. **The image must define an ENTRYPOINT.** The OCI Adapter uses the declared
   `argv` as the container command; an image that only has a CMD makes
   `argv = ["serve", ...]` the executable itself. The locally built Missive was
   committed with `ENTRYPOINT ["/usr/local/bin/missive"]` (the pinned D1
   image has it in its Dockerfile).
4. **Missive needs the nondefault-port patch** for `jmap_url =
   "http://stalwart:8080"`, exactly as the sample README already says.

## What was used

- A Linux container with the host Docker socket, Rust, Docker CLI 27, Chromium
  and Node 22 (the CLI must be a Linux binary because `ato run` refuses OCI
  routes on macOS).
- The current CLI built from the S3 worktree (`cargo build -p ato-cli --bin ato`).
- A **local-only** demo capsule (`.tmp`, not committed) derived from the D1
  sample: same two services and the same browser Surface, but images replaced by
  loopback-registry digests, no fixed-TCP mail Ports and no TLS observations
  (the local runtime does not publish them). K therefore covered only
  `webmail-health` and `webmail-ui`.
- A demo domain/account seeded through Stalwart's JMAP registry from a helper
  container on the group network.

## What this proves, and what it does not

Proves: the D1 component composition (Stalwart JMAP + Missive Web Surface) is
realizable by the Ato OCI service-group runtime; K3's browser observations pass
on a real Run; a person can reach the client and sign in through the Run URL.

Does not prove: the staging Runner realization, fixed-TCP mail Ports, PROXY v2,
TLS/K5, the state mount, Bindings delivery, 443/Caddy, or restart/delivery
(K4/K6). The first staging Run remains the gate for those, and the runbook is the
check to execute there.

## Reproduction outline (Linux Docker host)

```sh
# 1. Linux CLI environment with the host Docker socket
docker run -d --name ato-linux --network host \
  -v /var/run/docker.sock:/var/run/docker.sock -v "$PWD:$PWD" -w "$PWD" \
  rust:1-bookworm sleep infinity
docker exec ato-linux sh -c 'apt-get update && apt-get install -y docker.io'
# use a Docker CLI >= 1.44 (apt's 20.10 is too old for Docker 29)

# 2. build the CLI and prepare the images (Stalwart flattened/cap-stripped,
#    Missive from the pinned commit + the two patches), push them to a loopback
#    registry and pin the digests in a local-only capsule

# 3. run and drive it
ATO_HOME=/host/shared/ato-home /path/to/ato run demo.capsule
CHROME_BIN=/usr/bin/chromium MAIL_CLIENT_URL=http://127.0.0.1:<port> \
  MAIL_USER=demo@demo.test MAIL_PASSWORD=<test> \
  OUT_DIR=/host/shared/evidence node scripts/ops/mail-browser-check.mjs
```

The URL is bound inside the Docker VM, so the browser must run in that Linux
environment (here: Chromium in the same container); exposing it to the macOS
host needs a forwarding step that was not required for this evidence.
