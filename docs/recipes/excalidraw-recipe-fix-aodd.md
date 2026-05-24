# Excalidraw recipe fix AODD — Test Set A reach rate 8 / 8

**Branch:** `test/excalidraw-recipe-fix-aodd` (this PR)
**Base:** `dev` @ `1758a981` + local `codex/desktop-sample-state-bindings` (now also includes Excalidraw recipe-only fix)
**Supersedes:** PR #263
**Date:** 2026-05-25

## Headline

```text
Excalidraw reach session-created: ✅ NEW — HTTP 200 on :8080 in 6s
Test Set A reach rate:            8 / 8 (every app reaches session-created)
Regression check:                 zero — all 7 previously-passing apps still pass
```

## Root cause + fix (recipe-only)

The original recipe pinned `excalidraw/excalidraw:0.17.6`. Docker Hub returns
`manifest unknown` for that tag — the image only publishes:
- `latest` (recently maintained, 2026-05-06 push, 41MB)
- 2021-era `sha-*` dev builds (12MB skeletons, not real releases)

`ghcr.io/excalidraw/excalidraw` returned HTTP 401 with bearer challenge —
unpublished or private, no fallback.

**Two recipe-only fixes in `samples/recipes/excalidraw/capsule.toml`:**

### 1. Digest-pin the image

Per the task brief's fix-priority (semver > digest > latest > third-party),
the upstream image doesn't publish semver, and `ghcr.io` isn't an option,
so the recipe pins by sha256 digest of the 2026-05-06 `latest` build:

```toml
image = "excalidraw/excalidraw@sha256:0faa2324e70d2331952550c0f29ea20af63ffcfd146fbb2ffd5bacdc7f8d8a6b"
```

`version` was bumped `0.17.6` → `0.18.0` (the manifest validator requires
semver; recipe-metadata version is independent of image tag).

### 2. Move container port off the privileged range

The upstream image runs nginx on port 80. The orchestrator publishes main
services at Fixed mode (`host_port = container_port`), so session-start
errored:

```text
Docker responded with status code 500:
"listen tcp 127.0.0.1:80: bind: permission denied"
```

macOS doesn't permit non-root processes to bind privileged ports (<1024).
The recipe's `cmd` override sed-rewrites nginx's default.conf before exec:

```toml
cmd = ["sh", "-c", "sed -i 's/listen[[:space:]]*80;/listen 8080;/' /etc/nginx/conf.d/default.conf && exec nginx -g 'daemon off;'"]
port = 8080
readiness_probe = { http_get = "/", port = "8080" }
```

Keeps us on the upstream image (task brief: no third-party forks).

## Excalidraw verified

```text
$ ATO_HOME=$(mktemp -d) ato app resolve excalidraw --json
{ "resolution": { "kind": "sample_recipe", "source": "sample_recipe", ... } }    ✓

$ ATO_HOME=$(mktemp -d) ato app session start excalidraw --json
{ ... full session envelope ... }
elapsed: 6s

$ podman ps
ato-excalidraw-1dd30267-main  excalidraw/excalidraw@sha256:0faa...  Up 3s  127.0.0.1:8080->8080/tcp

$ curl -I http://localhost:8080/
HTTP/1.1 200 OK
Server: nginx/1.27.5
Date: Sun, 24 May 2026 21:21:50 GMT
Content-Type: text/html
Content-Length: 6843
```

Cleanup: 0 orphan containers after stop.

## Full Test Set A reach rate

| App | Elapsed | Containers | HTTP | session-created |
|---|---|---|---|---|
| memos | 6s | 1 | 200 | ✅ |
| uptime-kuma | 15s | 1 | 302 | ✅ (expected /setup redirect) |
| n8n | 19s | 1 | 404 on /; 200 on /healthz | ✅ |
| open-webui | 2s | 1 | 000 (first-run download; no probe) | ✅ |
| blinko | 22s | 2 | 200 | ✅ |
| affine | 55s | 3 | 302 | ✅ (expected /onboarding redirect) |
| dify | 87s | 6 | 200 | ✅ (worker has RabbitMQ retry-loop — separate follow-up) |
| **excalidraw** | **6s** | **1** | **200** | **✅ NEW** |

**8 / 8 reach session-created.** Cleanup: zero orphan containers after every run.

## Regression check (vs PR #263)

| Property | PR #263 | This AODD |
|---|---|---|
| Excalidraw reach session-created | ❌ image tag | ✅ recipe-only fix |
| AFFiNE reach session-created | ✅ | ✅ (no regression) |
| Dify reach session-created | ✅ | ✅ (faster: 87s vs ~4m, images cached) |
| Blinko reach session-created | ✅ | ✅ (no regression) |
| Memos reach session-created | ✅ | ✅ (no regression) |
| All prior wins | OK | OK |

## Scope discipline

Per task brief: this slice is **recipe-only**. The diff is:
```
samples/recipes/excalidraw/capsule.toml | 23 +++++++++++++++++++----
1 file changed, 19 insertions(+), 4 deletions(-)
```

No orchestrator-layering changes. No Dify worker work. No Desktop parity work.
That keeps the 8/8 reach rate's causal attribution clean — Excalidraw passes
because of the recipe fix, nothing else.

## Follow-ups (carried over, not in this slice)

1. **Dify worker RabbitMQ** — recipe-runtime fix
2. **Drive AFFiNE / Dify / Excalidraw through Desktop** to confirm UI parity with CLI
3. **`ato ps --json` Desktop-session unification** (pending from #257)
4. **Upstream cause propagation in preflight** (pending from #255)
5. **bollard's docker.sock auto-detection** (workaround: DOCKER_HOST=podman)

## Final report (per brief format)

```text
AODD complete.

Headline:
  Excalidraw recipe fix: PASS (digest-pin + port 80→8080)
  Test Set A reach rate: 8 / 8 session-created
  No regression for AFFiNE / Dify / Blinko / Memos / uptime-kuma / n8n / open-webui

Reach rate (all CLI direct):
  memos       ✅ 6s   HTTP 200 :5230
  uptime-kuma ✅ 15s  HTTP 302 :3001
  n8n         ✅ 19s  HTTP 404 / (200 /healthz)
  open-webui  ✅ 2s   HTTP 000 (first-run download by design)
  blinko      ✅ 22s  HTTP 200 :1111
  affine      ✅ 55s  HTTP 302 :3010
  dify        ✅ 87s  HTTP 200 :3000
  excalidraw  ✅ 6s   HTTP 200 :8080   ← NEW

Cleanup: zero orphan containers after every run.

Root cause: missing/unpullable Excalidraw image tag (excalidraw/excalidraw:0.17.6
returned `manifest unknown`) + port 80 hits macOS privileged-bind restriction.

Fix: recipe-only — digest-pin the 2026-05-06 latest image, override nginx
listen port to 8080, update readiness_probe to match.

Validation:
  - ato app resolve excalidraw → kind=sample_recipe
  - ato app session start excalidraw → 6s, HTTP 200, nginx/1.27.5
  - regression sweep: 7 / 7 prior apps still pass
  - cleanup: 0 orphans after each stop

Final reach rate: 8 / 8

Receipts:
  - .tmp/aodd-receipts/excalidraw-recipe-fix/excalidraw.yaml
  - .tmp/aodd-receipts/excalidraw-recipe-fix/regression-batch.yaml

Consolidated doc:
  - docs/recipes/excalidraw-recipe-fix-aodd.md

Next slice candidates:
  1. Dify worker RabbitMQ recipe-runtime follow-up
  2. Drive AFFiNE / Dify / Excalidraw through Desktop UI for parity with CLI
  3. ato ps Desktop session ledger unification (#257)
  4. Upstream cause propagation in preflight (#255)
  5. bollard's docker.sock auto-detection
```

## Environment

```text
Worktree:    .worktrees/excalidraw-recipe-fix-aodd
Branch:      test/excalidraw-recipe-fix-aodd
Source:      built from local codex/desktop-sample-state-bindings (recipe fix this slice)
Binaries:    target/release/ato 0.5.2 (rebuilt to embed updated capsule.toml via include_str!)
ATO_HOME:    multiple mktemp dirs per-app (hermetic)
DOCKER_HOST: unix:///var/folders/.../podman/podman-machine-default-api.sock
podman:      applehv machine running
```
