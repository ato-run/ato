# OCI `run_once` → Podman: Manual Smoke Verification

> **Historical manual test.** This documents a removed OCI recipe execution
> path and is not current architecture authority.

This document covers manual verification of the OCI `run_once` lifecycle
against a real Podman instance, complementing the automated fake-provider
unit tests in
`crates/ato-cli/src/adapters/runtime/executors/oci_multi_service.rs`.

The synthetic recipe lives at
`samples/recipes/oci-run-once-smoke/capsule.toml`:

```text
db (postgres:16-alpine)  ──readiness── pg_isready
        │
        ▼
init (busybox:1.36)      ── exit 0 ─── completion = readiness
        │
        ▼
app (nginx:alpine)       ───────────── GET / returns 200
```

The init container runs `sh -c "echo run_once-smoke: init step completed
&& exit 0"`. Its exit-0 is the readiness condition for `app`.

---

## Prerequisites

| Requirement | Notes |
|-------------|-------|
| Podman ≥ 4.x installed | `podman --version` |
| macOS: Podman machine running | `podman machine start` |
| Linux: rootless Podman configured | `podman info --format '{{.Host.Security.Rootless}}'` → `true` |
| `ato` CLI built from source | `cargo build -p ato-cli` |

---

## Run

```sh
cargo run -p ato-cli --bin ato -- run samples/recipes/oci-run-once-smoke
```

### Expected console signal

```text
🔗 Creating OCI network: ato-oci-run-once-smoke-<sfx>
⬇  [db] Pulling OCI image: postgres:16-alpine
📦 [db] Creating container: ato-oci-run-once-smoke-db-<sfx>
▶  [db] Starting container
⏳ [db] Waiting for readiness
⬇  [init] Pulling OCI image: busybox:1.36
📦 [init] Creating container: ato-oci-run-once-smoke-init-<sfx>
▶  [init] Starting container
⏳ [init] Waiting for init container to complete
✅ [init] Init container completed successfully
⬇  [app] Pulling OCI image: nginx:alpine
📦 [app] Creating container: ato-oci-run-once-smoke-app-<sfx>
▶  [app] Starting container
⏳ [app] Waiting for readiness
🌐 OCI service available at http://127.0.0.1:<port>/
```

### Acceptance checklist

- [ ] `db` reaches readiness (Postgres `pg_isready` returns 0).
- [ ] `init` runs to completion and is removed before `app` is created
      (verify by `podman ps -a` showing no init container after the run).
- [ ] `app` becomes ready and `GET http://127.0.0.1:<port>/` returns 200.
- [ ] `ato ps` lists only `db` and `app` (never `init`).
- [ ] `ato stop --all` shuts the session down cleanly with no errors
      about a missing init container.

---

## Failure-mode spot checks

### Non-zero exit blocks `app`

Edit the recipe's init `cmd` to:

```toml
cmd = ["sh", "-c", "echo failing && exit 7"]
```

Expected:
- `db` starts.
- `init` exits 7.
- `app` is never created.
- Final error contains `oci_run_once_failed`.
- `db` is stopped and removed during cleanup.
- `podman network ls` shows no orphan `ato-oci-run-once-smoke-*` network.

### Timeout

Edit the recipe's init `cmd` to:

```toml
cmd = ["sh", "-c", "sleep 600"]
```

Then run with a short timeout:

```sh
ATO_OCI_RUN_ONCE_TIMEOUT_SECS=3 \
  cargo run -p ato-cli --bin ato -- run samples/recipes/oci-run-once-smoke
```

Expected:
- After ~3 s, error contains `oci_run_once_timeout`.
- Init container is force-removed.
- `db` is cleaned up.

---

## AODD receipt template

```yaml
recipe: oci-run-once-smoke
date: <YYYY-MM-DD>
ato_version: <version>
podman_version: <version>

stuck_moments: []   # populate with any agent friction observed

result:
  status: <complete | degraded | suspicious | failed>
  notes: |
    <free-form notes>
```

A `complete` receipt is the release gate for `feat/runtime-oci-run-once`.
