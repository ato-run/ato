# OCI Compose → Podman: Manual Smoke Verification

> **Historical manual test.** The `ato run . --oci-compose` path and referenced
> crate layout are not part of the current CLI.

This document covers manual verification of the `ato run . --oci-compose` path
against a real Podman instance, complementing the automated fake-provider unit
tests in `crates/ato-cli/src/adapters/runtime/executors/oci_compose_runner.rs`.

---

## Prerequisites

| Requirement | Notes |
|-------------|-------|
| Podman ≥ 4.x installed | `podman --version` |
| macOS: Podman machine running | `podman machine start` |
| Linux: rootless Podman configured | `podman info --format '{{.Host.Security.Rootless}}'` → `true` |
| `ato` CLI built from source | `cargo build -p ato-cli` |

---

## Automated opt-in test

An `#[ignore]` integration test is provided in `oci_compose_runner.rs`.
Enable it with:

```sh
ATO_TEST_REAL_PODMAN=1 cargo test -p ato-cli real_podman -- --ignored --nocapture
```

The test:
- Creates a temporary directory with a minimal `docker-compose.yml` (two
  `alpine:3.19` services: `db` and `app`).
- Calls `execute_compose_run(...)` directly (no CLI parsing).
- Asserts exit code 0 **or** gracefully skips if Podman is not ready.

---

## Manual Blinko-style smoke

This validates the full user-facing path against a real app graph.

### 1. Create fixture directory

```sh
mkdir -p /tmp/blinko-smoke && cd /tmp/blinko-smoke
```

Create `docker-compose.yml`:

```yaml
services:
  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: blinko
      POSTGRES_DB: blinko
      POSTGRES_PASSWORD: changeme123
    volumes:
      - pg_data:/var/lib/postgresql/data

  blinko:
    image: blinkospace/blinko:latest
    ports:
      - "1111:1111"
    depends_on:
      - postgres
    environment:
      DATABASE_URL: "postgresql://blinko:changeme123@postgres:5432/blinko"
      NEXTAUTH_SECRET: "smoketestsecret"

volumes:
  pg_data:
```

### 2. Build the CLI

```sh
# from repo root:
cargo build -p ato-cli
```

### 3. Run with --oci-compose

```sh
ATO_CLI=target/debug/ato  # or `ato` if installed
$ATO_CLI run /tmp/blinko-smoke --oci-compose
```

### Expected output

```
📋 Compose file: /tmp/blinko-smoke/docker-compose.yml
🔧 Services: postgres, blinko
✅ [postgres] Resolved: sha256:...
✅ [blinko]   Resolved: sha256:...
ℹ️  Starting service: postgres
ℹ️  Starting service: blinko
ℹ️  Main endpoint: http://127.0.0.1:<host-port>
```

**Must NOT appear in output:**
- `changeme123` (raw password)
- `smoketestsecret` (raw secret)
- Any `POSTGRES_PASSWORD` value
- Literal `DATABASE_URL` value containing the password

### 4. Verify startup order

`postgres` must appear in logs before `blinko`. This is enforced by the
service DAG (`blinko` depends_on `postgres`).

### 5. Verify host port

Only `blinko` publishes a host port. `postgres` is internal-only.

```sh
podman ps --format '{{.Names}} {{.Ports}}'
```

Expected: only the `blinko` container has a host port binding.

### 6. Cleanup

```sh
$ATO_CLI run /tmp/blinko-smoke --oci-compose --cleanup  # if cleanup flag is wired
# or manually:
podman stop $(podman ps -q)
podman rm $(podman ps -aq)
podman network rm $(podman network ls -q)
podman volume prune -f  # only if pg_data should be removed
```

---

## Known failure modes

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `oci_provider_not_ready` | Podman machine not running (macOS) | `podman machine start` |
| `oci_provider_not_ready: rootless ambiguous` | Multiple Podman machines, none selected | `podman machine stop` unused machines |
| `image not found` / pull fails | Registry rate-limit or network | Retry or pre-pull with `podman pull <image>` |
| `oci_execution_gate_failed` | `OciPolicyMode::Strict` with `egress_allow` set | Use `--policy loose` or unset egress |
| `compose_not_found` | No compose file in directory | Ensure `docker-compose.yml` exists in the project root |
| `dependency cycle` | `depends_on` forms a cycle | Fix the compose file |
| Secret values visible in output | Bug — report immediately | Check `redact_secret_like_env_values` in `oci_compose_runner.rs` |

---

## Coverage summary

| Test path | What it covers |
|-----------|---------------|
| Fake-provider unit tests (24 total) | Import, resolve, policy gate, pull failure, diagnostics, lock persistence replay |
| `#[ignore]` opt-in real Podman test | Minimal two-service alpine smoke |
| Manual Blinko smoke (this doc) | Full user-facing path with real images |

---

## Lock persistence behavior (PR 10.6)

From PR 10.6 onward, `ato.oci.lock.json` is written to the project directory
after image digest resolution.

### First run (no lock)

```
📋 Compose file: /tmp/blinko-smoke/docker-compose.yml
🔧 Services: postgres, blinko
🔍 [postgres] Resolving image digest: postgres:16-alpine
✅ [postgres] Resolved: sha256:abcdef12345...
🔍 [blinko] Resolving image digest: blinkospace/blinko:latest
✅ [blinko] Resolved: sha256:deadbeef678...
🔒 Lock written: ato.oci.lock.json
```

`ato.oci.lock.json` is created in the project directory.

### Second run (lock present and fresh)

```
📋 Compose file: /tmp/blinko-smoke/docker-compose.yml
🔧 Services: postgres, blinko
♻️  [postgres] Reusing lock: postgres:16-alpine → sha256:abcdef12345...
♻️  [blinko] Reusing lock: blinkospace/blinko:latest → sha256:deadbeef678...
🔒 Lock written: ato.oci.lock.json
```

No provider round-trip for either service. Execution identity is unchanged.

### Compose file changed (lock drift)

If the `docker-compose.yml` content changes (hash changes), all entries are
re-resolved and the lock is updated with fresh digests.

---

## Deferred (future PRs)

- `install.sh` / `docker run` intent extractor (PR 11)
- Persistent volume GC policy
- Readiness probe timeout tests against real containers
