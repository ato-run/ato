# AODD Receipt: dev-head Test Set A — Dify native-arm64 fix verified

**Follow-up to**: `docs/aodd/dev-head-test-set-a-receipt.md` (PR #271)  
**Fix applied**: PR #272 — removed `allow_emulation = true` from dify recipe

## Run Metadata

| Field | Value |
|-------|-------|
| dev commit SHA | `aef1919a` (after #272 merge) |
| OS / arch | Darwin arm64 (macOS) |
| Container backend | Podman `podman-machine-default` (applehv VM) |
| DOCKER_HOST | `unix:///var/folders/98/k9wrs95s7972nb_qn_k8k2kr0000gn/T/podman/podman-machine-default-api.sock` |
| ATO_HOME isolation | `ATO_HOME=$(mktemp -d)` per app run |
| ato binary | `./target/debug/ato` (built from dev HEAD `aef1919a`) |
| Date | 2025-07-14 |

## Contract

| Field | Value |
|-------|-------|
| usecase | Run all 8 Test Set A apps from CLI, verify HTTP readiness from macOS host, stop, confirm cleanup |
| actor | Agent using only `ato` CLI + `curl` + `docker` stop/cleanup |
| goal | All 8 apps reach session-created; Dify specifically reaches HTTP readiness from macOS host (not just Podman VM); zero orphan containers after stop |
| entry_point | Dev HEAD `aef1919a`, fresh ATO_HOME per run |
| out_of_scope | Desktop UI, recipe changes, code modifications during run |
| time_budget | 360s per app |

---

## Dify Before / After Comparison

| Metric | #271 run (before #272) | This run (after #272) |
|--------|------------------------|----------------------|
| EXIT code | 124 (300s timeout) | 0 |
| Elapsed | 300s | 126s |
| Session status | timed out | ready |
| Container arch | x86_64 (QEMU-emulated) | **aarch64 (native arm64)** |
| HTTP from macOS host | connection reset | **307 ✅** |
| Root cause | `allow_emulation = true` forced x86_64 emulation; Podman applehv can't port-forward QEMU containers to macOS host | Removed `allow_emulation`; images pull native arm64 variants |

Architecture confirmation (via `docker exec <container> uname -m`):

```
ato-dify-76075751-api     → aarch64  ✅ native
ato-dify-76075751-main    → aarch64  ✅ native
ato-dify-76075751-weaviate → aarch64 ✅ native
```

---

## Results Summary

| App | EXIT | Elapsed | HTTP | Containers | Cleanup |
|-----|------|---------|------|------------|---------|
| memos | 0 | 24s | 200 :5230 | 1 | ✅ |
| uptime-kuma | 0 | 28s | 302 :3001 | 1 | ✅ |
| n8n | 0 | 53s | 200 :5678/healthz | 1 | ✅ |
| open-webui | 0 | 12s | 000 (first-run¹) | 1 | ✅ |
| blinko | 0 | 39s | 200 :1111 | 2 | ✅ |
| affine | 0 | 75s | 302 :3010 | 3 | ✅ |
| dify | **0** | **126s** | **307 :3000 ✅** | 6 | ✅ |
| excalidraw | 0 | 12s | 200 :8080 | 1 | ✅ |

**Final reach rate: 8 / 8 session-created; 7 / 8 host-HTTP-ready¹**

¹ open-webui returns `status=ready` (container started) but HTTP is not yet serving because LLM models download on first run. Per spec: *"first-run download behavior is acceptable if documented."*

---

## Per-App Detail

### memos ✅
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start memos --json
Elapsed: 24s | EXIT=0 | HTTP: 200 :5230 | 1 container | cleanup ✅
```

### uptime-kuma ✅
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start uptime-kuma --json
Elapsed: 28s | EXIT=0 | HTTP: 302 :3001 (→ /setup) | 1 container | cleanup ✅
```

### n8n ✅
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start n8n --json
Elapsed: 53s | EXIT=0 | HTTP: 200 :5678/healthz | 1 container | cleanup ✅
```

### open-webui ⚠️ (acceptable — first-run download)
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start open-webui --json
Elapsed: 12s | EXIT=0 | HTTP: 000 (model download in progress) | 1 container | cleanup ✅
```

Primary URL: `http://127.0.0.1:8080/`

### blinko ✅
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start blinko --json
Elapsed: 39s | EXIT=0 | HTTP: 200 :1111 | 2 containers (db + main) | cleanup ✅
```

### affine ✅
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start affine --json
Elapsed: 75s | EXIT=0 | HTTP: 302 :3010 (→ /onboarding) | 3 containers (db + redis + main) | cleanup ✅
```

### dify ✅ (fixed in #272)
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start dify --json
Elapsed: 126s | EXIT=0 | HTTP: 307 :3000 from macOS host ✅
Containers: 6 (db, redis, weaviate, api, worker, main) | cleanup ✅

Container architectures (uname -m):
  api     → aarch64  (native linux/arm64)
  web     → aarch64  (native linux/arm64)
  weaviate → aarch64 (native linux/arm64)
```

### excalidraw ✅
```
Command: ATO_HOME=$(mktemp -d) ./target/debug/ato app session start excalidraw --json
Elapsed: 12s | EXIT=0 | HTTP: 200 :8080 | 1 container | cleanup ✅
```

---

## Cleanup Verification

After all 8 runs:

```
docker ps → 0 running containers ✅
```

Note: `docker container prune -f && docker network prune -f` still required after each stop — Podman does not auto-prune networks on container stop. Tracked in issue #273.

---

## Open Follow-ups

| Issue | Status |
|-------|--------|
| open-webui: readiness probe fires before UI ready (first-run download) | documented, low priority |
| network prune not called on `ato app session stop` | #273, tracked separately |
