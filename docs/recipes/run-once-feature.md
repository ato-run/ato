# OCI `run_once` (one-shot service lifecycle)

## Overview

OCI named targets can now opt into a **run-to-completion** lifecycle via
`run_once = true`. The runtime starts the container, waits for it to exit,
and treats **exit code 0 as the readiness condition** for any dependent
service. This unblocks the common multi-service pattern of "init job →
long-running services":

- database migrations (`alembic upgrade head`)
- permission initialization (`chown -R …`)
- bucket / schema seed (`mc mb`, `psql -f init.sql`)
- bootstrap admin user creation

## Syntax (v0.3)

```toml
[targets.init-permissions]
runtime = "oci"
image = "ghcr.io/example/app:latest"
cmd = ["sh", "-c", "chown -R 1000:1000 /app/storage"]
run_once = true
depends_on = ["db"]

[targets.db]
runtime = "oci"
image = "postgres:16-alpine"
port = 5432

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 8080
depends_on = ["init-permissions"]
```

Start order: `db` → `init-permissions` (run to exit 0) → `app`.

## Start-order semantics

1. `run_once` targets participate in the `depends_on` graph like any other
   target.
2. A `run_once` target's dependencies must be ready (their readiness probe
   has passed, or "container started" if no probe) before the run-once
   container is started.
3. Dependents of a `run_once` target wait until the run-once container exits
   with status 0. Exit-0 *is* the readiness condition — no `readiness_probe`
   is needed (and is rejected if set).
4. Long-running services continue to use their normal readiness probe.

## Success / failure semantics

| Outcome | Typed error | Effect |
|---------|-------------|--------|
| Exit code 0 | — | Dependents start. Container is removed immediately. |
| Exit code ≠ 0 | `oci_run_once_failed` | Dependents are **not** started. Previously-started long-running services are stopped + removed by the existing reverse-order cleanup. |
| Wait error (provider) | `oci_run_once_failed` | Same as above. |
| Timeout | `oci_run_once_timeout` | Same as above. Default timeout is **300 s**, overridable via `ATO_OCI_RUN_ONCE_TIMEOUT_SECS`. |

`run_once` containers are never added to the long-running session record, so
they do not appear in `ato ps`, and `ato stop --all` does not try to stop
them after they have exited.

## Validation rules

| Rule | Error |
|------|-------|
| `run_once` on non-OCI runtime | parse error: `'run_once' is only supported for OCI targets` |
| `run_once` without `cmd` | parse error: `'run_once' requires 'cmd' to be set` |
| `run_once` + `readiness_probe` | parse error: `'run_once' targets must not define 'readiness_probe'` |
| `run_once` + `port` | parse error: `'run_once' targets must not define 'port'` |

The `cmd` requirement is intentional: relying on the image's default
`CMD` for a one-shot job is a foot-gun (servers often default to a
long-running entry point).

## Identity behavior

`run_once` is part of the **execution identity**:

- Flipping a service from long-running to `run_once` (or vice versa) changes
  the JCS-canonical execution_id, because it changes the start-order
  contract and the success/failure semantics.
- Exit timestamp, container id, allocated host port, log content, and
  elapsed time are **excluded** from identity — they are runtime artifacts.

See the regression tests:
- `run_once_lifecycle_changes_execution_identity`
- `run_once_exit_timestamp_not_in_identity`
- `run_once_container_id_not_in_identity`

## Interaction with shared mutable state

`run_once` solves the **lifecycle** half of the init-container pattern.
Many real-world init jobs (Dify's `init_permissions` is the motivating
example) also need to write to a volume that a long-running service
later reads from. That second half is **shared mutable state**, which
Ato v0.3 does not relax. If your init container's only side effect is
filesystem state that some other service must also write to, you still
hit Ato's shared-state policy.

## Non-goals (explicitly out of scope for this feature)

- **Cron / scheduled jobs** — `run_once` runs once per `ato run` invocation,
  not on a recurring schedule.
- **Restarting / retrying jobs** — failure is terminal. Use the recipe's
  `cmd` to wrap retry logic if needed.
- **Background workers** — long-running queue consumers are not `run_once`
  candidates. Use a normal long-running target with a readiness probe.
- **Full Kubernetes `initContainers` compatibility** — we deliberately
  implement the one-shot subset, not the full pod lifecycle.
- **Relaxing shared mutable state** — separate concern, separate PR.

## Sample recipe

A minimal end-to-end synthetic recipe lives at
[`samples/recipes/oci-run-once-smoke/capsule.toml`](../../samples/recipes/oci-run-once-smoke/capsule.toml).
It exercises `db → init (run_once) → app` in a single invocation.
