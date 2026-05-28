# Multi-Service `depends_on` Support

## Overview

Ato recipes can now express explicit service dependency ordering via `depends_on`.
This unblocks multi-container stacks where one service (e.g. an app) must wait
for another service (e.g. a database) to be ready before starting.

## Syntax

```toml
[targets.db]
runtime = "oci"
image = "postgres:16-alpine"

[targets.app]
runtime = "oci"
image = "example/app:1.0"
depends_on = ["db"]          # list of target names
```

The alias `needs` is also accepted:

```toml
[targets.app]
needs = ["db"]
```

## Exec Readiness Probe

For services like PostgreSQL that bind their port before accepting connections,
use an exec probe instead of a TCP probe:

```toml
[services.db]
target = "db"
readiness_probe = { exec = ["pg_isready", "-U", "myuser"], port = "5432" }
```

The exec probe runs `podman exec <container> pg_isready -U myuser` and waits
for exit code 0. This is more reliable than a TCP probe which fires as soon as
the port binds (before Postgres is ready for queries).

## Start Order Semantics

1. Ato builds a dependency graph from all `depends_on` edges.
2. Services start in topological order — dependencies before dependents.
3. Each dependency's readiness probe must pass before the dependent starts.
4. If no readiness probe is defined for a dependency, the condition falls back
   to "container started".
5. Services stop in **reverse** topological order on cleanup.

## Validation Rules

| Rule | Error |
|------|-------|
| Unknown dependency target | `dependency_unknown_target` |
| Self-dependency | `dependency_self_reference` |
| Dependency cycle | `dependency_cycle` |

## Identity Semantics

Dependency edges are part of execution identity. Adding or removing a
`depends_on` edge produces a different derivation hash. Container IDs,
host ports, network IDs, and session IDs are excluded from identity.

## Example: App + Postgres

```toml
schema_version = "0.3"
name = "umami"

[targets.db]
runtime = "oci"
image = "postgres:16-alpine"
env = { POSTGRES_DB = "umami", POSTGRES_USER = "umami", POSTGRES_PASSWORD = "secret" }

[targets.app]
runtime = "oci"
image = "ghcr.io/umami-software/umami:postgresql-v2.17.0"
port = 3000
env = { DATABASE_URL = "postgresql://umami:secret@db:5432/umami" }
depends_on = ["db"]

[state.db-data]
kind = "filesystem"
durability = "persistent"
attach = "explicit"

[services.db]
target = "db"
readiness_probe = { exec = ["pg_isready", "-U", "umami"], port = "5432" }

[services.main]
target = "app"
readiness_probe = { http_get = "/api/heartbeat", port = "3000" }

[[services.main.state_bindings]]
state = "db-data"
target = "/var/lib/postgresql/data"
service_target = "db"
```

## AODD Validation

| App | Stack | Status | Notes |
|-----|-------|--------|-------|
| umami | app + postgres | **pass** | HTTP 200 `/api/heartbeat`, clean ATO_HOME, `ato stop --all` OK |
| logto | app + postgres | **partial** | v1.26.0 requires multi-step DB init; upstream issue, not Ato |

## Follow-ups

- Per-target readiness timing controls (`initial_delay_seconds`, `timeout_seconds` in schema)
- Logto: use `svhd/logto:latest` or add init container pattern once supported
