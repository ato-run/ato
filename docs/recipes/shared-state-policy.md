# Shared State Policy

Ato OCI recipes support explicit shared mutable state between services within the same capsule.

## Default: Exclusive

By default each `[state.*]` declaration is **exclusive** — only one service may write to it:

```toml
[state.uploads]
kind = "filesystem"
durability = "persistent"
purpose = "app uploads"
attach = "explicit"
schema_id = "sha256:app-uploads-v1"

[[services.main.state_bindings]]
state = "uploads"
target = "/app/storage"
```

## Same-Capsule Shared State

When multiple services within the same capsule need to share a writable directory, declare `sharing = "same-capsule"`:

```toml
[state.uploads]
kind = "filesystem"
durability = "persistent"
purpose = "shared uploads"
attach = "explicit"
schema_id = "sha256:shared-uploads-v1"
sharing = "same-capsule"

[[services.app.state_bindings]]
state = "uploads"
target = "/app/storage"

[[services.worker.state_bindings]]
state = "uploads"
target = "/app/storage"
```

### Rules

- `sharing = "same-capsule"` must be explicitly set on the state declaration
- `schema_id` is required for shared state
- All consumers must be within the same capsule
- No arbitrary host path bind mounts for shared state
- All declared state keys must exist under `[state]`

### Validation Errors

| Error | Description | Status |
|-------|-------------|--------|
| `StateSharedRequiresPolicy` | State is bound by multiple services but `sharing` is not `same-capsule` | ✅ Active |
| `StateSharedRequiresSchemaId` | Shared state is missing `schema_id` | ✅ Active |
| `StateKeyUndeclared` | State key used in bindings is not declared under `[state]` | ✅ Active |
| `StateSharedConflictingMountMode` | Conflicting mount modes across services | Deferred (requires per-binding mode) |
| `StateSharedCrossCapsuleForbidden` | Cross-capsule sharing is not supported | Deferred (no cross-capsule refs in v0.3) |
| `StateSharedHostBindForbidden` | Absolute host path not allowed for shared state | Deferred (requires runtime-layer check) |

## Writable Semantics

In v1, all state mounts are writable (`readonly: false`). Per-binding mount mode control
(e.g., `api: read-write`, `web: read-only`) and a `writable` field are deferred to a future version.

## Safety Boundary

- Shared state is scoped to the same capsule/session
- Cross-capsule state sharing is not supported
- Arbitrary host bind mounts are not permitted through shared state
- Host paths are not exposed in user-facing output unless Ato-managed

## Execution Identity

Shared state policy is part of the execution identity. Changing a state from exclusive to same-capsule produces a different execution identity hash, ensuring auditability and reproducibility.

Fields included in identity:
- State key name
- Schema ID
- Sharing mode
- Mount target paths per service

Fields excluded:
- Actual host path
- Volume ID
- Container ID
- File contents
- Timestamps

## Cleanup Behavior

- **Persistent shared state**: preserved across sessions (named Podman volumes)
- **Ephemeral shared state**: removed on session cleanup
- `run_once` services: may mount shared state and their writes persist

## Usage Patterns

### App + Worker Shared Uploads

```toml
[state.uploads]
sharing = "same-capsule"
schema_id = "sha256:app-uploads-v1"
# ...

[services.api]
target = "api"
[[services.api.state_bindings]]
state = "uploads"
target = "/app/storage"

[services.worker]
target = "worker"
[[services.worker.state_bindings]]
state = "uploads"
target = "/app/storage"
```

### Dify Storage Pattern

Dify requires `api`, `worker`, and `init-permissions` to share `/app/api/storage`:

```toml
[state.api-storage]
kind = "filesystem"
durability = "persistent"
purpose = "Dify file uploads and storage"
attach = "explicit"
schema_id = "sha256:dify-api-storage-v1"
sharing = "same-capsule"

[[services.main.state_bindings]]
state = "api-storage"
target = "/app/api/storage"
service_target = "api"

[[services.worker.state_bindings]]
state = "api-storage"
target = "/app/api/storage"
```
